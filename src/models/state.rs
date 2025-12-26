use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, RwLock, Semaphore};
use tracing::{debug, warn};
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
    pub native_sim_concurrency: usize,
    pub vm_sim_concurrency: usize,
    pub metrics: Arc<QuoteMetrics>,
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

    pub fn native_sim_concurrency(&self) -> usize {
        self.native_sim_concurrency
    }

    pub fn vm_sim_concurrency(&self) -> usize {
        self.vm_sim_concurrency
    }

    pub fn metrics(&self) -> Arc<QuoteMetrics> {
        Arc::clone(&self.metrics)
    }
}

#[derive(Default)]
pub struct QuoteMetrics {
    skipped_native: AtomicU64,
    skipped_vm: AtomicU64,
    pool_timeout_native: AtomicU64,
    pool_timeout_vm: AtomicU64,
    quote_timeout: AtomicU64,
}

pub struct MetricsSnapshot {
    pub skipped_native: u64,
    pub skipped_vm: u64,
    pub pool_timeout_native: u64,
    pub pool_timeout_vm: u64,
    pub quote_timeout: u64,
}

impl QuoteMetrics {
    pub fn record_skip(&self, is_vm: bool) {
        if is_vm {
            self.skipped_vm.fetch_add(1, Ordering::Relaxed);
        } else {
            self.skipped_native.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_pool_timeout(&self, is_vm: bool) {
        if is_vm {
            self.pool_timeout_vm.fetch_add(1, Ordering::Relaxed);
        } else {
            self.pool_timeout_native.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_quote_timeout(&self) {
        self.quote_timeout.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            skipped_native: self.skipped_native.load(Ordering::Relaxed),
            skipped_vm: self.skipped_vm.load(Ordering::Relaxed),
            pool_timeout_native: self.pool_timeout_native.load(Ordering::Relaxed),
            pool_timeout_vm: self.pool_timeout_vm.load(Ordering::Relaxed),
            quote_timeout: self.quote_timeout.load(Ordering::Relaxed),
        }
    }
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
                            shard
                                .insert_new(id.clone(), Arc::from(state), Arc::clone(&component))
                                .await;
                            self.add_to_index(&id, component.as_ref()).await;
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
            let removed_kind = {
                let mut guard = self.id_to_kind.write().await;
                guard.remove(&id)
            };

            if let Some(kind) = removed_kind {
                if let Some(shard) = self.shards.get(&kind) {
                    shard.remove(&id).await;
                    removed_pairs_count += 1;
                }
                self.remove_from_index(&id, &component).await;
            } else {
                debug!(
                    "received removal for unknown pair {} ({})",
                    id, component.protocol_type_name
                );
                self.remove_from_index(&id, &component).await;
            }
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

    async fn add_to_index(&self, id: &str, component: &ProtocolComponent) {
        let pool_id = id.to_string();
        let mut index = self.token_index.write().await;
        for token in &component.tokens {
            index
                .entry(token.address.clone())
                .or_insert_with(HashSet::new)
                .insert(pool_id.clone());
        }
    }

    async fn remove_from_index(&self, id: &str, component: &ProtocolComponent) {
        let mut index = self.token_index.write().await;
        for token in &component.tokens {
            if let Some(ids) = index.get_mut(&token.address) {
                ids.remove(id);
                if ids.is_empty() {
                    index.remove(&token.address);
                }
            }
        }
    }
}
