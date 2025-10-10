use std::collections::HashMap;

use anyhow::Result;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};
use tycho_simulation::{
    models::Token,
    tycho_common::{models::Chain, Bytes},
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
}

impl TokenStore {
    pub fn new(
        initial: HashMap<Bytes, Token>,
        tycho_url: String,
        api_key: String,
        chain: Chain,
    ) -> Self {
        TokenStore {
            tokens: RwLock::new(initial),
            refresh_lock: Mutex::new(()),
            tycho_url,
            api_key,
            chain,
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
    pub async fn ensure(&self, address: &Bytes) -> Result<Option<Token>> {
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

    async fn refresh(&self) -> Result<()> {
        let guard = self.refresh_lock.lock().await;
        let new_tokens = load_all_tokens(
            &self.tycho_url,
            false,
            Some(&self.api_key),
            self.chain,
            None,
            None,
        )
        .await;
        info!("Token cache refreshed: total={} entries", new_tokens.len());
        *self.tokens.write().await = new_tokens;
        drop(guard);
        Ok(())
    }
}
