use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, RwLock, Semaphore};
use tracing::{debug, warn};
use tycho_execution::encoding::tycho_encoder::TychoEncoder;
use tycho_simulation::{
    protocol::models::{ProtocolComponent, Update},
    tycho_common::{models::token::Token, simulation::protocol_sim::ProtocolSim, Bytes},
};

use super::{protocol::ProtocolKind, tokens::TokenStore};

#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<TokenStore>,
    pub state_store: Arc<StateStore>,
    pub quote_timeout: Duration,
    pub pool_timeout_native: Duration,
    pub pool_timeout_vm: Duration,
    pub request_timeout: Duration,
    pub native_sim_semaphore: Arc<Semaphore>,
    pub vm_sim_semaphore: Arc<Semaphore>,
    pub encode_state: Arc<EncodeState>,
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

    pub fn quote_timeout(&self) -> Duration {
        self.quote_timeout
    }

    pub fn pool_timeout_native(&self) -> Duration {
        self.pool_timeout_native
    }

    pub fn pool_timeout_vm(&self) -> Duration {
        self.pool_timeout_vm
    }

    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }
    pub fn native_sim_semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.native_sim_semaphore)
    }

    pub fn vm_sim_semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.vm_sim_semaphore)
    }

    pub fn encode_state(&self) -> Arc<EncodeState> {
        Arc::clone(&self.encode_state)
    }

    pub async fn pool_by_id(&self, id: &str) -> Option<PoolEntry> {
        self.state_store.pool_by_id(id).await
    }
}

#[derive(Clone)]
pub struct EncodeState {
    pub cow_settlement_contract: Option<Bytes>,
    pub transfer_from_encoder: Arc<dyn TychoEncoder>,
    pub none_encoder: Arc<dyn TychoEncoder>,
}

pub(crate) type PoolEntry = (Arc<dyn ProtocolSim>, Arc<ProtocolComponent>);
#[allow(clippy::type_complexity)]
#[derive(Clone, Default)]
struct ProtocolShard {
    states: Arc<RwLock<HashMap<String, PoolEntry>>>,
}

impl ProtocolShard {
    async fn insert_new(
        &self,
        id: String,
        state: Arc<dyn ProtocolSim>,
        component: Arc<ProtocolComponent>,
    ) {
        let mut guard = self.states.write().await;
        guard.insert(id, (state, component));
    }

    async fn update_state(&self, id: &str, state: Arc<dyn ProtocolSim>) -> bool {
        let mut guard = self.states.write().await;
        if let Some((existing_state, _)) = guard.get_mut(id) {
            *existing_state = state;
            true
        } else {
            false
        }
    }

    async fn remove(&self, id: &str) -> Option<PoolEntry> {
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
    tokens: Arc<TokenStore>,
    shards: HashMap<ProtocolKind, ProtocolShard>,
    id_to_kind: RwLock<HashMap<String, ProtocolKind>>,
    // Token -> pool id index to avoid scanning all pools on each quote.
    token_index: RwLock<HashMap<Bytes, HashSet<String>>>,
    block_number: RwLock<u64>,
    ready: AtomicBool,
    ready_tx: watch::Sender<bool>,
}

impl StateStore {
    pub fn new(tokens: Arc<TokenStore>) -> Self {
        let mut shards = HashMap::new();
        for kind in ProtocolKind::ALL {
            shards.insert(kind, ProtocolShard::default());
        }
        let (ready_tx, _) = watch::channel(false);
        StateStore {
            tokens,
            shards,
            id_to_kind: RwLock::new(HashMap::new()),
            token_index: RwLock::new(HashMap::new()),
            block_number: RwLock::new(0),
            ready: AtomicBool::new(false),
            ready_tx,
        }
    }

    pub async fn apply_update(&self, mut update: Update) -> UpdateMetrics {
        // EVM feeds always return a block number
        let block_number = update.block_number_or_timestamp;
        {
            let mut guard = self.block_number.write().await;
            *guard = block_number;
        }

        let mut index_additions: Vec<(Bytes, String)> = Vec::new();
        let mut index_removals: Vec<(Bytes, String)> = Vec::new();

        let mut new_pairs_count = 0;
        let mut tokens_to_cache: Vec<Token> = Vec::new();
        for (id, component) in update.new_pairs.into_iter() {
            match ProtocolKind::from_component(&component) {
                Some(kind) => {
                    let state = update.states.remove(&id);
                    if let Some(state) = state {
                        let component = Arc::new(component);
                        self.id_to_kind.write().await.insert(id.clone(), kind);
                        if let Some(shard) = self.shards.get(&kind) {
                            tokens_to_cache.extend(component.tokens.iter().cloned());
                            for token in component.tokens.iter() {
                                index_additions.push((token.address.clone(), id.clone()));
                            }
                            shard
                                .insert_new(id.clone(), Arc::from(state), Arc::clone(&component))
                                .await;
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

        if !tokens_to_cache.is_empty() {
            self.tokens.insert_batch(tokens_to_cache.into_iter()).await;
        }

        let mut updated_states = 0;
        for (id, state) in update.states.into_iter() {
            let kind_opt = { self.id_to_kind.read().await.get(&id).copied() };
            if let Some(kind) = kind_opt {
                if let Some(shard) = self.shards.get(&kind) {
                    if shard.update_state(&id, Arc::from(state)).await {
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
            for token in component.tokens.iter() {
                index_removals.push((token.address.clone(), id.clone()));
            }

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

        if !index_additions.is_empty() || !index_removals.is_empty() {
            let mut index = self.token_index.write().await;
            apply_token_index_changes(&mut index, index_additions, index_removals);
        }

        let total_pairs = self.total_states().await;

        let mut broadcast_value = None;
        if total_pairs > 0 && !self.ready.load(Ordering::Acquire) {
            self.ready.store(true, Ordering::Release);
            broadcast_value = Some(true);
        } else if total_pairs == 0 && self.ready.load(Ordering::Acquire) {
            self.ready.store(false, Ordering::Release);
            broadcast_value = Some(false);
        }

        if let Some(value) = broadcast_value {
            let _ = self.ready_tx.send(value);
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

    pub(crate) async fn matching_pools_by_addresses(
        &self,
        token_in: &Bytes,
        token_out: &Bytes,
    ) -> Vec<(String, PoolEntry)> {
        let (token_in_ids, token_out_ids) = {
            let index = self.token_index.read().await;
            let token_in_ids = index.get(token_in).cloned();
            let token_out_ids = index.get(token_out).cloned();
            (token_in_ids, token_out_ids)
        };

        let (Some(token_in_ids), Some(token_out_ids)) = (token_in_ids, token_out_ids) else {
            return Vec::new();
        };

        let (smaller, larger) = if token_in_ids.len() <= token_out_ids.len() {
            (token_in_ids, token_out_ids)
        } else {
            (token_out_ids, token_in_ids)
        };

        let mut candidate_ids = Vec::with_capacity(smaller.len());
        for id in smaller {
            if larger.contains(&id) {
                candidate_ids.push(id);
            }
        }

        if candidate_ids.is_empty() {
            return Vec::new();
        }

        let id_kinds: Vec<(String, ProtocolKind)> = {
            let guard = self.id_to_kind.read().await;
            candidate_ids
                .iter()
                .filter_map(|id| guard.get(id).copied().map(|kind| (id.clone(), kind)))
                .collect()
        };

        let mut by_kind: HashMap<ProtocolKind, Vec<String>> = HashMap::new();
        for (id, kind) in id_kinds {
            by_kind.entry(kind).or_default().push(id);
        }

        let mut result = Vec::with_capacity(candidate_ids.len());
        for (kind, ids) in by_kind {
            if let Some(shard) = self.shards.get(&kind) {
                let guard = shard.states.read().await;
                for id in ids {
                    if let Some((state, component)) = guard.get(&id) {
                        result.push((id, (Arc::clone(state), Arc::clone(component))));
                    }
                }
            }
        }

        result
    }

    pub(crate) async fn pool_by_id(&self, id: &str) -> Option<PoolEntry> {
        let kind_opt = { self.id_to_kind.read().await.get(id).copied() };
        let kind = kind_opt?;
        let shard = self.shards.get(&kind)?;
        let guard = shard.states.read().await;
        guard
            .get(id)
            .map(|(state, component)| (Arc::clone(state), Arc::clone(component)))
    }
}

fn apply_token_index_changes(
    index: &mut HashMap<Bytes, HashSet<String>>,
    index_additions: Vec<(Bytes, String)>,
    index_removals: Vec<(Bytes, String)>,
) {
    for (token, id) in index_additions {
        index.entry(token).or_default().insert(id);
    }

    for (token, id) in index_removals {
        if let Some(set) = index.get_mut(&token) {
            set.remove(&id);
            if set.is_empty() {
                index.remove(&token);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn token(address: &str) -> Bytes {
        Bytes::from_str(address.trim_start_matches("0x")).expect("valid token address")
    }

    #[test]
    fn apply_token_index_changes_additions_and_removals() {
        let mut index: HashMap<Bytes, HashSet<String>> = HashMap::new();

        let token_a = token("0x0000000000000000000000000000000000000001");
        let token_b = token("0x0000000000000000000000000000000000000002");

        apply_token_index_changes(
            &mut index,
            vec![
                (token_a.clone(), "pool1".to_string()),
                (token_a.clone(), "pool2".to_string()),
                (token_b.clone(), "pool1".to_string()),
            ],
            vec![],
        );

        assert_eq!(
            index.get(&token_a).unwrap(),
            &HashSet::from(["pool1".to_string(), "pool2".to_string()])
        );
        assert_eq!(
            index.get(&token_b).unwrap(),
            &HashSet::from(["pool1".to_string()])
        );

        apply_token_index_changes(
            &mut index,
            vec![],
            vec![
                (token_a.clone(), "pool1".to_string()),
                (token_b.clone(), "pool1".to_string()),
            ],
        );

        assert_eq!(
            index.get(&token_a).unwrap(),
            &HashSet::from(["pool2".to_string()])
        );
        assert!(!index.contains_key(&token_b));
    }

    #[test]
    fn apply_token_index_changes_matches_sequential_application() {
        let token_a = token("0x0000000000000000000000000000000000000003");
        let token_b = token("0x0000000000000000000000000000000000000004");

        let additions = vec![
            (token_a.clone(), "pool1".to_string()),
            (token_a.clone(), "pool2".to_string()),
            (token_b.clone(), "pool2".to_string()),
        ];
        let removals = vec![
            (token_a.clone(), "pool1".to_string()),
            (token_b.clone(), "pool2".to_string()),
        ];

        let mut batched: HashMap<Bytes, HashSet<String>> = HashMap::new();
        apply_token_index_changes(&mut batched, additions.clone(), removals.clone());

        let mut sequential: HashMap<Bytes, HashSet<String>> = HashMap::new();
        for (token, id) in additions {
            sequential.entry(token).or_default().insert(id);
        }
        for (token, id) in removals {
            if let Some(set) = sequential.get_mut(&token) {
                set.remove(&id);
                if set.is_empty() {
                    sequential.remove(&token);
                }
            }
        }

        assert_eq!(batched, sequential);
    }
}
