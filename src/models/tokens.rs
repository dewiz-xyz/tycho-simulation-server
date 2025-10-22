use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, RwLock};
use tokio::time::timeout;
use tracing::{info, warn};
use tycho_simulation::{
    tycho_common::{
        models::{token::Token, Chain},
        Bytes,
    },
    utils::load_all_tokens,
};

/// In-memory cache of Tycho token metadata with best-effort refresh when
/// lookups miss. Refreshes reuse the Tycho `/tokens` endpoint to cover cases
/// where the initial snapshot lacked assets requested by the solver.
pub struct TokenStore {
    tokens: RwLock<HashMap<Bytes, Token>>,
    refresh_lock: Mutex<()>,
    tycho_url: String,
    api_key: String,
    chain: Chain,
    refresh_timeout: Duration,
}

impl TokenStore {
    pub fn new(
        initial: HashMap<Bytes, Token>,
        tycho_url: String,
        api_key: String,
        chain: Chain,
        refresh_timeout: Duration,
    ) -> Self {
        TokenStore {
            tokens: RwLock::new(initial),
            refresh_lock: Mutex::new(()),
            tycho_url,
            api_key,
            chain,
            refresh_timeout,
        }
    }

    pub async fn snapshot(&self) -> HashMap<Bytes, Token> {
        self.tokens.read().await.clone()
    }

    pub async fn get(&self, address: &Bytes) -> Option<Token> {
        self.tokens.read().await.get(address).cloned()
    }

    /// Ensure the token metadata exists. If missing, trigger a refresh of the
    /// Tycho token list and attempt to resolve again.
    pub async fn ensure(&self, address: &Bytes) -> Result<Option<Token>, TokenStoreError> {
        if let Some(token) = self.get(address).await {
            return Ok(Some(token));
        }

        warn!(
            "Token {} missing from cache; refreshing from Tycho",
            address
        );
        self.refresh().await?;
        Ok(self.get(address).await)
    }

    async fn refresh(&self) -> Result<(), TokenStoreError> {
        let guard = self.refresh_lock.lock().await;
        let start = Instant::now();

        let fetch_future = load_all_tokens(
            &self.tycho_url,
            false,
            Some(&self.api_key),
            self.chain,
            None,
            None,
        );

        let new_tokens = match timeout(self.refresh_timeout, fetch_future).await {
            Ok(tokens) => tokens,
            Err(_) => {
                warn!(
                    scope = "token_refresh_timeout",
                    timeout_ms = self.refresh_timeout.as_millis() as u64,
                    "Token refresh timed out"
                );
                drop(guard);
                return Err(TokenStoreError::RefreshTimeout(self.refresh_timeout));
            }
        };

        let elapsed_ms = start.elapsed().as_millis() as u64;
        info!(
            duration_ms = elapsed_ms,
            total = new_tokens.len(),
            "Token cache refreshed"
        );
        *self.tokens.write().await = new_tokens;
        drop(guard);
        Ok(())
    }
}

#[derive(Debug)]
pub enum TokenStoreError {
    RefreshTimeout(Duration),
}

impl fmt::Display for TokenStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenStoreError::RefreshTimeout(duration) => {
                write!(
                    f,
                    "token refresh timed out after {} ms",
                    duration.as_millis()
                )
            }
        }
    }
}

impl std::error::Error for TokenStoreError {}
