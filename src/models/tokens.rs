use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use reqwest::{header, Client, ClientBuilder};
use tokio::sync::{watch, Mutex, RwLock, Semaphore};
use tracing::{info, warn};
use tycho_simulation::tycho_common::{
    dto::{TokensRequestBody, TokensRequestResponse},
    models::{token::Token, Chain},
    Bytes,
};

/// In-memory cache of Tycho token metadata that prefers updates from the stream and
/// falls back to a single token fetch from the Tycho RPC when a miss occurs.
type InflightRx = watch::Receiver<Option<Result<Option<Token>, TokenStoreError>>>;
type InflightMap = HashMap<Bytes, InflightRx>;

pub struct TokenStore {
    tokens: RwLock<HashMap<Bytes, Token>>,
    inflight: Mutex<InflightMap>,
    fetch_semaphore: Semaphore,
    tycho_url: String,
    api_key: String,
    chain: Chain,
    fetch_timeout: Duration,
    client: Client,
}

impl TokenStore {
    pub fn new(
        initial: HashMap<Bytes, Token>,
        tycho_url: String,
        api_key: String,
        chain: Chain,
        fetch_timeout: Duration,
    ) -> Self {
        let client = ClientBuilder::new()
            .connect_timeout(Duration::from_millis(300))
            .pool_idle_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(1)
            .tcp_nodelay(true)
            .build()
            .expect("failed to build HTTP client");

        TokenStore {
            tokens: RwLock::new(initial),
            inflight: Mutex::new(HashMap::new()),
            fetch_semaphore: Semaphore::new(16),
            tycho_url,
            api_key,
            chain,
            fetch_timeout,
            client,
        }
    }

    pub async fn snapshot(&self) -> HashMap<Bytes, Token> {
        self.tokens.read().await.clone()
    }

    pub async fn get(&self, address: &Bytes) -> Option<Token> {
        self.tokens.read().await.get(address).cloned()
    }

    /// Ensure the token metadata exists. If missing, fetch just that token via
    /// the Tycho RPC instead of refreshing the full list.
    pub async fn ensure(&self, address: &Bytes) -> Result<Option<Token>, TokenStoreError> {
        if let Some(token) = self.get(address).await {
            return Ok(Some(token));
        }

        self.fetch_token(address).await
    }

    /// Merge a batch of tokens into the in-memory cache. Existing entries are
    /// left untouched to preserve any richer metadata already present.
    pub async fn insert_batch(&self, tokens: impl IntoIterator<Item = Token>) {
        let mut guard = self.tokens.write().await;
        for token in tokens {
            guard.entry(token.address.clone()).or_insert(token);
        }
    }

    async fn fetch_token(&self, address: &Bytes) -> Result<Option<Token>, TokenStoreError> {
        // Coalesce concurrent fetches for the same address using a watch channel.
        {
            let mut inflight = self.inflight.lock().await;
            if let Some(rx) = inflight.get(address) {
                let mut rx = rx.clone();
                drop(inflight);
                while rx.changed().await.is_ok() {
                    if let Some(res) = rx.borrow().clone() {
                        return res;
                    }
                }
            } else {
                let (tx, rx) = watch::channel(None);
                inflight.insert(address.clone(), rx);
                drop(inflight);
                let res = self.fetch_token_inner(address, tx).await;
                let mut inflight = self.inflight.lock().await;
                inflight.remove(address);
                return res;
            }
        }

        // If the watch channel closed unexpectedly, perform a direct fetch as a fallback.
        let (tx, rx) = watch::channel(None);
        {
            let mut inflight = self.inflight.lock().await;
            inflight.insert(address.clone(), rx);
        }
        let res = self.fetch_token_inner(address, tx).await;
        let mut inflight = self.inflight.lock().await;
        inflight.remove(address);
        res
    }

    async fn fetch_token_inner(
        &self,
        address: &Bytes,
        tx: watch::Sender<Option<Result<Option<Token>, TokenStoreError>>>,
    ) -> Result<Option<Token>, TokenStoreError> {
        let _permit = self.fetch_semaphore.acquire().await.unwrap();

        // Another task may have populated the cache while we were waiting.
        if let Some(token) = self.tokens.read().await.get(address).cloned() {
            let _ = tx.send(Some(Ok(Some(token.clone()))));
            return Ok(Some(token));
        }

        warn!(
            scope = "token_single_fetch",
            token_address = %address,
            "Fetching single token from Tycho RPC - this should be rare"
        );

        let start = Instant::now();

        let body = TokensRequestBody {
            token_addresses: Some(vec![address.clone()]),
            chain: self.chain.into(),
            ..Default::default()
        };

        let url = format!("{}/v1/tokens", self.rpc_base_url());

        let result: Result<Option<Token>, TokenStoreError> = async {
            let response = self
                .client
                .post(url)
                .header(header::AUTHORIZATION, self.api_key.clone())
                .json(&body)
                .timeout(self.fetch_timeout)
                .send()
                .await
                .map_err(|err| {
                    let elapsed_ms = start.elapsed().as_millis() as u64;
                    warn!(
                        scope = "token_single_fetch",
                        token_address = %address,
                        elapsed_ms,
                        is_timeout = err.is_timeout(),
                        is_connect = err.is_connect(),
                        error = %err,
                        "Token fetch request failed before response"
                    );
                    if err.is_timeout() {
                        TokenStoreError::FetchTimeout(self.fetch_timeout)
                    } else {
                        TokenStoreError::RequestFailed(err.to_string())
                    }
                })?;

            if !response.status().is_success() {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                warn!(
                    scope = "token_single_fetch",
                    token_address = %address,
                    status = %response.status(),
                    elapsed_ms,
                    "Token fetch returned non-success status"
                );
                return Err(TokenStoreError::RequestFailed(format!(
                    "Tycho RPC returned status {} when fetching token {}",
                    response.status(),
                    address
                )));
            }

            let TokensRequestResponse { tokens, .. } = response
                .json::<TokensRequestResponse>()
                .await
                .map_err(|err| {
                    let elapsed_ms = start.elapsed().as_millis() as u64;
                    warn!(
                        scope = "token_single_fetch",
                        token_address = %address,
                        elapsed_ms,
                        error = %err,
                        "Failed to decode token response"
                    );
                    TokenStoreError::RequestFailed(format!(
                        "Failed to parse token response: {}",
                        err
                    ))
                })?;

            let maybe_token = tokens
                .into_iter()
                .find(|token| token.address == *address)
                .and_then(|token| Token::try_from(token).ok());

            if let Some(token) = maybe_token {
                self.tokens
                    .write()
                    .await
                    .entry(token.address.clone())
                    .or_insert_with(|| token.clone());
                let elapsed_ms = start.elapsed().as_millis() as u64;
                info!(
                    scope = "token_single_fetch",
                    token_address = %address,
                    elapsed_ms,
                    "Token fetch succeeded"
                );
                Ok(Some(token))
            } else {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                warn!(
                    scope = "token_single_fetch",
                    token_address = %address,
                    elapsed_ms,
                    "Token response contained no matching token"
                );
                Ok(None)
            }
        }
        .await;

        let _ = tx.send(Some(result.clone()));
        result
    }

    fn rpc_base_url(&self) -> String {
        if self.tycho_url.starts_with("http://") || self.tycho_url.starts_with("https://") {
            self.tycho_url.trim_end_matches('/').to_string()
        } else {
            format!("https://{}", self.tycho_url.trim_end_matches('/'))
        }
    }
}

#[derive(Debug, Clone)]
pub enum TokenStoreError {
    FetchTimeout(Duration),
    RequestFailed(String),
}

impl fmt::Display for TokenStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenStoreError::FetchTimeout(duration) => {
                write!(f, "token fetch timed out after {} ms", duration.as_millis())
            }
            TokenStoreError::RequestFailed(message) => write!(f, "token fetch failed: {}", message),
        }
    }
}

impl std::error::Error for TokenStoreError {}
