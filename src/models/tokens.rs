use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use reqwest::{header, Client};
use tokio::sync::{Mutex, RwLock};
use tracing::warn;
use tycho_simulation::tycho_common::{
    dto::{TokensRequestBody, TokensRequestResponse},
    models::{token::Token, Chain},
    Bytes,
};

/// In-memory cache of Tycho token metadata that prefers updates from the stream and
/// falls back to a single token fetch from the Tycho RPC when a miss occurs.
pub struct TokenStore {
    tokens: RwLock<HashMap<Bytes, Token>>,
    fetch_lock: Mutex<()>,
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
        TokenStore {
            tokens: RwLock::new(initial),
            fetch_lock: Mutex::new(()),
            tycho_url,
            api_key,
            chain,
            fetch_timeout,
            client: Client::new(),
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
        let _guard = self.fetch_lock.lock().await;

        // Another task may have populated the cache while we were waiting.
        if let Some(token) = self.tokens.read().await.get(address).cloned() {
            return Ok(Some(token));
        }

        warn!(
            scope = "token_single_fetch",
            token_address = %address,
            "Fetching single token from Tycho RPC - this should be rare"
        );

        let body = TokensRequestBody {
            token_addresses: Some(vec![address.clone()]),
            chain: self.chain.into(),
            ..Default::default()
        };

        let url = format!("{}/v1/tokens", self.rpc_base_url());

        let response = self
            .client
            .post(url)
            .header(header::AUTHORIZATION, self.api_key.clone())
            .json(&body)
            .timeout(self.fetch_timeout)
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    TokenStoreError::FetchTimeout(self.fetch_timeout)
                } else {
                    TokenStoreError::RequestFailed(err.to_string())
                }
            })?;

        if !response.status().is_success() {
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
            TokenStoreError::RequestFailed(format!("Failed to parse token response: {}", err))
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
            Ok(Some(token))
        } else {
            Ok(None)
        }
    }

    fn rpc_base_url(&self) -> String {
        if self.tycho_url.starts_with("http://") || self.tycho_url.starts_with("https://") {
            self.tycho_url.trim_end_matches('/').to_string()
        } else {
            format!("https://{}", self.tycho_url.trim_end_matches('/'))
        }
    }
}

#[derive(Debug)]
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
