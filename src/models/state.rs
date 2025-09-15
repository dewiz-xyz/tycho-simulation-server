use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, watch, RwLock};
use tracing::{debug, warn};
use tycho_simulation::{
    protocol::{
        models::{BlockUpdate, ProtocolComponent},
        state::ProtocolSim,
    },
    tycho_core::Bytes,
};

use super::{messages::UpdateMessage, protocol::ProtocolKind, tokens::TokenStore};

#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<TokenStore>,
    pub state_store: Arc<StateStore>,
    pub update_tx: broadcast::Sender<UpdateMessage>,
}

impl AppState {
    pub async fn current_block(&self) -> u64 {
        self.state_store.current_block().await
    }

    pub async fn total_pools(&self) -> usize {
        self.state_store.total_states().await
    }

    pub fn is_ready(&self) -> bool {
        self.state_store.is_ready()
    }

    pub async fn wait_for_readiness(&self, wait: Duration) -> bool {
        self.state_store.wait_until_ready(wait).await
    }
}

#[derive(Clone, Default)]
struct ProtocolShard {
    states: Arc<RwLock<HashMap<String, (Box<dyn ProtocolSim>, ProtocolComponent)>>>,
}

impl ProtocolShard {
    async fn insert_new(
        &self,
        id: String,
        state: Box<dyn ProtocolSim>,
        component: ProtocolComponent,
    ) {
        let mut guard = self.states.write().await;
        guard.insert(id, (state, component));
    }

    async fn update_state(&self, id: &str, state: Box<dyn ProtocolSim>) -> bool {
        let mut guard = self.states.write().await;
        if let Some((existing_state, _)) = guard.get_mut(id) {
            *existing_state = state;
            true
        } else {
            false
        }
    }

    async fn remove(&self, id: &str) -> Option<(Box<dyn ProtocolSim>, ProtocolComponent)> {
        let mut guard = self.states.write().await;
        guard.remove(id)
    }

    async fn len(&self) -> usize {
        let guard = self.states.read().await;
        guard.len()
    }
}

pub struct UpdateMetrics {
    pub block_number: u64,
    pub updated_states: usize,
    pub new_pairs: usize,
    pub removed_pairs: usize,
    pub total_pairs: usize,
}

pub struct StateStore {
    shards: HashMap<ProtocolKind, ProtocolShard>,
    id_to_kind: RwLock<HashMap<String, ProtocolKind>>,
    block_number: RwLock<u64>,
    ready: AtomicBool,
    ready_tx: watch::Sender<bool>,
}

impl StateStore {
    pub fn new() -> Self {
        let mut shards = HashMap::new();
        for kind in ProtocolKind::ALL {
            shards.insert(kind, ProtocolShard::default());
        }
        let (ready_tx, _) = watch::channel(false);
        StateStore {
            shards,
            id_to_kind: RwLock::new(HashMap::new()),
            block_number: RwLock::new(0),
            ready: AtomicBool::new(false),
            ready_tx,
        }
    }

    pub async fn apply_update(&self, mut update: BlockUpdate) -> UpdateMetrics {
        let block_number = update.block_number;
        {
            let mut guard = self.block_number.write().await;
            *guard = block_number;
        }

        let mut new_pairs_count = 0;
        for (id, component) in update.new_pairs.into_iter() {
            match ProtocolKind::from_component(&component) {
                Some(kind) => {
                    let state = update.states.remove(&id);
                    if let Some(state) = state {
                        self.id_to_kind.write().await.insert(id.clone(), kind);
                        if let Some(shard) = self.shards.get(&kind) {
                            shard.insert_new(id.clone(), state, component).await;
                            new_pairs_count += 1;
                        }
                    } else {
                        warn!("new pair {} missing state on update", id);
                    }
                }
                None => {
                    warn!(
                        "ignoring new pair with unknown protocol: {}",
                        component.protocol_type_name
                    );
                }
            }
        }

        let mut updated_states = 0;
        for (id, state) in update.states.into_iter() {
            let kind_opt = { self.id_to_kind.read().await.get(&id).copied() };
            if let Some(kind) = kind_opt {
                if let Some(shard) = self.shards.get(&kind) {
                    if shard.update_state(&id, state).await {
                        updated_states += 1;
                    } else {
                        warn!("state missing in shard for id {}", id);
                    }
                }
            } else {
                warn!("update for unknown pair {}; dropping", id);
            }
        }

        let mut removed_pairs_count = 0;
        for (id, component) in update.removed_pairs.into_iter() {
            let removed_kind = {
                let mut guard = self.id_to_kind.write().await;
                guard.remove(&id)
            };

            if let Some(kind) = removed_kind {
                if let Some(shard) = self.shards.get(&kind) {
                    shard.remove(&id).await;
                    removed_pairs_count += 1;
                }
            } else {
                debug!(
                    "received removal for unknown pair {} ({})",
                    id, component.protocol_type_name
                );
            }
        }

        let total_pairs = self.total_states().await;

        if total_pairs > 0 && !self.ready.load(Ordering::Acquire) {
            self.ready.store(true, Ordering::Release);
            let _ = self.ready_tx.send(true);
        }

        UpdateMetrics {
            block_number,
            updated_states,
            new_pairs: new_pairs_count,
            removed_pairs: removed_pairs_count,
            total_pairs,
        }
    }

    pub async fn current_block(&self) -> u64 {
        let guard = self.block_number.read().await;
        *guard
    }

    pub async fn total_states(&self) -> usize {
        let mut total = 0;
        for shard in self.shards.values() {
            total += shard.len().await;
        }
        total
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    pub async fn wait_until_ready(&self, wait: Duration) -> bool {
        if self.is_ready() {
            return true;
        }
        let mut rx = self.ready_tx.subscribe();
        let sleep = tokio::time::sleep(wait);
        tokio::pin!(sleep);

        loop {
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() {
                        return self.is_ready();
                    }
                    if *rx.borrow() {
                        return true;
                    }
                }
                _ = sleep.as_mut() => {
                    return self.is_ready();
                }
            }
        }
    }

    pub async fn matching_pools_by_addresses(
        &self,
        token_in: &Bytes,
        token_out: &Bytes,
    ) -> Vec<(String, (Box<dyn ProtocolSim>, ProtocolComponent))> {
        let mut result = Vec::new();
        for shard in self.shards.values() {
            let guard = shard.states.read().await;
            for (id, (state, component)) in guard.iter() {
                let has_in = component
                    .tokens
                    .iter()
                    .any(|token| &token.address == token_in);
                if !has_in {
                    continue;
                }
                if component
                    .tokens
                    .iter()
                    .any(|token| &token.address == token_out)
                {
                    result.push((id.clone(), (state.clone(), component.clone())));
                }
            }
        }
        result
    }
}
