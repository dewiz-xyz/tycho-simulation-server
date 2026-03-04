use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, RwLock, Semaphore};
use tokio::time::Instant;
use tycho_simulation::{
    protocol::models::{ProtocolComponent, Update},
    tycho_common::{models::token::Token, simulation::protocol_sim::ProtocolSim, Bytes},
};

use super::{protocol::ProtocolKind, stream_health::StreamHealth, tokens::TokenStore};

const UPDATE_ANOMALY_SAMPLE_CAP: usize = 6;

#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<TokenStore>,
    pub native_state_store: Arc<StateStore>,
    pub vm_state_store: Arc<StateStore>,
    pub native_stream_health: Arc<StreamHealth>,
    pub vm_stream_health: Arc<StreamHealth>,
    pub vm_stream: Arc<RwLock<VmStreamStatus>>,
    pub latest_native_gas_price_wei: Arc<RwLock<Option<u128>>>,
    pub native_gas_price_reporting_enabled: Arc<RwLock<bool>>,
    pub enable_vm_pools: bool,
    pub readiness_stale: Duration,
    pub quote_timeout: Duration,
    pub pool_timeout_native: Duration,
    pub pool_timeout_vm: Duration,
    pub request_timeout: Duration,
    pub native_sim_semaphore: Arc<Semaphore>,
    pub vm_sim_semaphore: Arc<Semaphore>,
    pub reset_allowance_tokens: Arc<HashMap<u64, HashSet<Bytes>>>,
    pub native_sim_concurrency: usize,
    pub vm_sim_concurrency: usize,
}

impl AppState {
    pub async fn current_block(&self) -> u64 {
        self.native_state_store.current_block().await
    }

    pub async fn current_vm_block(&self) -> Option<u64> {
        if !self.vm_ready().await {
            return None;
        }
        Some(self.vm_state_store.current_block().await)
    }

    pub async fn total_pools(&self) -> usize {
        let native = self.native_state_store.total_states().await;
        let vm = self.vm_state_store.total_states().await;
        native + vm
    }

    pub fn is_ready(&self) -> bool {
        self.native_state_store.is_ready()
    }

    pub async fn wait_for_readiness(&self, wait: Duration) -> bool {
        self.native_state_store.wait_until_ready(wait).await
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

    pub async fn pool_by_id(&self, id: &str) -> Option<PoolEntry> {
        if let Some(entry) = self.native_state_store.pool_by_id(id).await {
            return Some(entry);
        }

        if !self.vm_ready().await {
            return None;
        }

        self.vm_state_store.pool_by_id(id).await
    }

    pub async fn vm_ready(&self) -> bool {
        if !self.enable_vm_pools {
            return false;
        }
        if self.vm_rebuilding().await {
            return false;
        }
        self.vm_state_store.is_ready()
    }

    pub async fn vm_rebuilding(&self) -> bool {
        self.vm_stream.read().await.rebuilding
    }

    pub async fn vm_block(&self) -> u64 {
        self.vm_state_store.current_block().await
    }

    pub async fn vm_pools(&self) -> usize {
        self.vm_state_store.total_states().await
    }

    pub async fn latest_native_gas_price_wei(&self) -> Option<u128> {
        *self.latest_native_gas_price_wei.read().await
    }

    pub async fn set_latest_native_gas_price_wei(&self, value: Option<u128>) {
        *self.latest_native_gas_price_wei.write().await = value;
    }

    pub async fn native_gas_price_reporting_enabled(&self) -> bool {
        *self.native_gas_price_reporting_enabled.read().await
    }

    pub async fn set_native_gas_price_reporting_enabled(&self, enabled: bool) {
        *self.native_gas_price_reporting_enabled.write().await = enabled;
    }

    pub async fn effective_native_gas_price_wei_for_quotes(&self) -> Option<u128> {
        // Keep the reporting flag read lock held while reading the cached value so a disable
        // transition cannot interleave between the flag check and cached-gas lookup.
        let reporting_enabled = self.native_gas_price_reporting_enabled.read().await;
        if !*reporting_enabled {
            return None;
        }

        *self.latest_native_gas_price_wei.read().await
    }
}

#[derive(Debug, Clone, Default)]
pub struct VmStreamStatus {
    pub rebuilding: bool,
    pub restart_count: u64,
    pub last_error: Option<String>,
    pub rebuild_started_at: Option<Instant>,
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

    async fn clear(&self) {
        let mut guard = self.states.write().await;
        *guard = HashMap::new();
    }
}

pub struct UpdateMetrics {
    pub block_number: u64,
    pub updated_states: usize,
    pub new_pairs: usize,
    pub removed_pairs: usize,
    pub total_pairs: usize,
    pub new_pairs_missing_state: usize,
    pub unknown_protocol_new_pairs: usize,
    pub updates_for_unknown_pair: usize,
    pub states_missing_in_shard: usize,
    pub removed_unknown_pair: usize,
    pub anomaly_samples: Vec<String>,
}

impl UpdateMetrics {
    pub fn has_anomalies(&self) -> bool {
        self.new_pairs_missing_state > 0
            || self.unknown_protocol_new_pairs > 0
            || self.updates_for_unknown_pair > 0
            || self.states_missing_in_shard > 0
            || self.removed_unknown_pair > 0
    }
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

    async fn rebuild_indexes(&self) -> usize {
        let mut id_to_kind = HashMap::new();
        let mut token_index: HashMap<Bytes, HashSet<String>> = HashMap::new();
        let mut total_pairs = 0usize;

        for (kind, shard) in self.shards.iter() {
            let guard = shard.states.read().await;
            for (id, (_, component)) in guard.iter() {
                id_to_kind.insert(id.clone(), *kind);
                total_pairs += 1;
                for token in component.tokens.iter() {
                    token_index
                        .entry(token.address.clone())
                        .or_default()
                        .insert(id.clone());
                }
            }
        }

        *self.id_to_kind.write().await = id_to_kind;
        *self.token_index.write().await = token_index;
        total_pairs
    }

    pub async fn reset(&self) {
        for shard in self.shards.values() {
            shard.clear().await;
        }
        *self.id_to_kind.write().await = HashMap::new();
        *self.token_index.write().await = HashMap::new();
        *self.block_number.write().await = 0;
        self.ready.store(false, Ordering::Relaxed);
        let _ = self.ready_tx.send(false);
    }

    pub async fn reset_protocols(&self, protocols: &[ProtocolKind]) {
        if protocols.is_empty() {
            return;
        }

        if protocols.len() >= self.shards.len() {
            self.reset().await;
            return;
        }

        let mut any_cleared = false;
        for kind in protocols {
            if let Some(shard) = self.shards.get(kind) {
                shard.clear().await;
                any_cleared = true;
            }
        }

        if !any_cleared {
            return;
        }

        let total_pairs = self.rebuild_indexes().await;
        let was_ready = self.ready.load(Ordering::Acquire);
        let now_ready = total_pairs > 0;
        if was_ready != now_ready {
            self.ready.store(now_ready, Ordering::Release);
            let _ = self.ready_tx.send(now_ready);
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
        let mut anomaly_samples = Vec::new();

        let mut new_pairs_count = 0;
        let mut new_pairs_missing_state = 0;
        let mut unknown_protocol_new_pairs = 0;
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
                        new_pairs_missing_state += 1;
                        add_anomaly_sample(
                            &mut anomaly_samples,
                            "new_pairs_missing_state",
                            id.as_str(),
                        );
                    }
                }
                None => {
                    unknown_protocol_new_pairs += 1;
                    add_anomaly_sample(
                        &mut anomaly_samples,
                        "unknown_protocol_new_pairs",
                        id.as_str(),
                    );
                }
            }
        }

        if !tokens_to_cache.is_empty() {
            self.tokens.insert_batch(tokens_to_cache.into_iter()).await;
        }

        let mut updated_states = 0;
        let mut updates_for_unknown_pair = 0;
        let mut states_missing_in_shard = 0;
        for (id, state) in update.states.into_iter() {
            let kind_opt = { self.id_to_kind.read().await.get(&id).copied() };
            if let Some(kind) = kind_opt {
                if let Some(shard) = self.shards.get(&kind) {
                    if shard.update_state(&id, Arc::from(state)).await {
                        updated_states += 1;
                    } else {
                        states_missing_in_shard += 1;
                        add_anomaly_sample(
                            &mut anomaly_samples,
                            "states_missing_in_shard",
                            id.as_str(),
                        );
                    }
                }
            } else {
                updates_for_unknown_pair += 1;
                add_anomaly_sample(
                    &mut anomaly_samples,
                    "updates_for_unknown_pair",
                    id.as_str(),
                );
            }
        }

        let mut removed_pairs_count = 0;
        let mut removed_unknown_pair = 0;
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
                removed_unknown_pair += 1;
                add_anomaly_sample(&mut anomaly_samples, "removed_unknown_pair", id.as_str());
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
            new_pairs_missing_state,
            unknown_protocol_new_pairs,
            updates_for_unknown_pair,
            states_missing_in_shard,
            removed_unknown_pair,
            anomaly_samples,
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

    pub async fn has_pool(&self, id: &str) -> bool {
        self.id_to_kind.read().await.contains_key(id)
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

    fn ids_for_token_from_index(
        &self,
        index: &HashMap<Bytes, HashSet<String>>,
        token: &Bytes,
    ) -> Option<HashSet<String>> {
        let wrapped_native_token = self.tokens.wrapped_native_token();
        let native_address = Bytes::from([0u8; 20]);
        let needs_native = wrapped_native_token.as_ref() == Some(token);
        let needs_wrapped = *token == native_address;

        let mut ids = index.get(token).cloned().unwrap_or_default();

        if needs_native {
            if let Some(native_ids) = index.get(&native_address) {
                ids.extend(native_ids.iter().cloned());
            }
        }

        if needs_wrapped {
            if let Some(wrapped_address) = wrapped_native_token.as_ref() {
                if let Some(wrapped_ids) = index.get(wrapped_address) {
                    ids.extend(wrapped_ids.iter().cloned());
                }
            }
        }

        (!ids.is_empty()).then_some(ids)
    }

    fn is_opposite_native_wrapped_pair(&self, token_in: &Bytes, token_out: &Bytes) -> bool {
        let native_address = Bytes::from([0u8; 20]);
        let Some(wrapped_address) = self.tokens.wrapped_native_token() else {
            return false;
        };

        (token_in == &native_address && token_out == &wrapped_address)
            || (token_in == &wrapped_address && token_out == &native_address)
    }

    pub(crate) async fn matching_pools_by_addresses(
        &self,
        token_in: &Bytes,
        token_out: &Bytes,
    ) -> Vec<(String, PoolEntry)> {
        // Keep both token ID lookups on the same index snapshot under concurrent updates.
        let (token_in_ids, token_out_ids) = {
            let index = self.token_index.read().await;
            if self.is_opposite_native_wrapped_pair(token_in, token_out) {
                (index.get(token_in).cloned(), index.get(token_out).cloned())
            } else {
                (
                    self.ids_for_token_from_index(&index, token_in),
                    self.ids_for_token_from_index(&index, token_out),
                )
            }
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

fn add_anomaly_sample(samples: &mut Vec<String>, kind: &str, value: &str) {
    if samples.len() >= UPDATE_ANOMALY_SAMPLE_CAP {
        return;
    }
    samples.push(format!("{kind}:{value}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::time::Duration;

    use num_bigint::BigUint;
    use tycho_simulation::{
        protocol::models::ProtocolComponent,
        tycho_common::{
            dto::ProtocolStateDelta,
            models::{token::Token, Chain},
            simulation::{
                errors::{SimulationError, TransitionError},
                protocol_sim::{Balances, GetAmountOutResult, ProtocolSim},
            },
            Bytes,
        },
    };

    #[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
    struct DummySim;

    #[typetag::serde]
    impl ProtocolSim for DummySim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(&self, _base: &Token, _quote: &Token) -> Result<f64, SimulationError> {
            Ok(0.0)
        }

        fn get_amount_out(
            &self,
            _amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<GetAmountOutResult, SimulationError> {
            Ok(GetAmountOutResult::new(
                BigUint::from(0u8),
                BigUint::from(0u8),
                Box::new(self.clone()),
            ))
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<(BigUint, BigUint), SimulationError> {
            Ok((BigUint::from(0u8), BigUint::from(0u8)))
        }

        fn delta_transition(
            &mut self,
            _delta: ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &Balances,
        ) -> Result<(), TransitionError<String>> {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(self.clone())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other.as_any().is::<DummySim>()
        }
    }

    fn token(address: &str) -> Bytes {
        Bytes::from_str(address.trim_start_matches("0x")).expect("valid token address")
    }

    fn address(seed: u8) -> Bytes {
        Bytes::from([seed; 20])
    }

    fn mk_token(seed: u8, symbol: &str) -> Token {
        Token::new(&address(seed), symbol, 18, 0, &[], Chain::Ethereum, 100)
    }

    fn mk_component(
        id_seed: u8,
        protocol_system: &str,
        protocol_type_name: &str,
        tokens: Vec<Token>,
    ) -> ProtocolComponent {
        ProtocolComponent::new(
            address(id_seed),
            protocol_system.to_string(),
            protocol_type_name.to_string(),
            Chain::Ethereum,
            tokens,
            Vec::new(),
            HashMap::new(),
            Bytes::new(),
            chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
                .expect("valid timestamp")
                .naive_utc(),
        )
    }

    fn mk_update(pairs: Vec<(String, ProtocolComponent, Box<dyn ProtocolSim>)>) -> Update {
        let mut states = HashMap::new();
        let mut new_pairs = HashMap::new();
        for (id, component, state) in pairs {
            states.insert(id.clone(), state);
            new_pairs.insert(id, component);
        }
        Update::new(1, states, new_pairs)
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

    #[tokio::test]
    async fn apply_update_tracks_anomaly_counters_and_samples() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let token_a = mk_token(20, "TKNA");
        let token_b = mk_token(21, "TKNB");

        let missing_state_component = mk_component(
            30,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![token_a.clone(), token_b.clone()],
        );
        let unknown_protocol_component = mk_component(
            31,
            "mystery_exchange",
            "mystery_pool",
            vec![token_a.clone(), token_b.clone()],
        );
        let removed_unknown_component =
            mk_component(32, "uniswap_v2", "uniswap_v2_pool", vec![token_a, token_b]);

        store
            .id_to_kind
            .write()
            .await
            .insert("stale-shard".to_string(), ProtocolKind::UniswapV2);

        let mut states: HashMap<String, Box<dyn ProtocolSim>> = HashMap::new();
        states.insert("unknown-update".to_string(), Box::new(DummySim));
        states.insert("stale-shard".to_string(), Box::new(DummySim));

        let mut new_pairs = HashMap::new();
        new_pairs.insert("missing-state".to_string(), missing_state_component);
        new_pairs.insert("unknown-proto".to_string(), unknown_protocol_component);

        let mut removed_pairs = HashMap::new();
        removed_pairs.insert("missing-removal".to_string(), removed_unknown_component);

        let metrics = store
            .apply_update(Update::new(2, states, new_pairs).set_removed_pairs(removed_pairs))
            .await;

        assert_eq!(metrics.new_pairs_missing_state, 1);
        assert_eq!(metrics.unknown_protocol_new_pairs, 1);
        assert_eq!(metrics.updates_for_unknown_pair, 1);
        assert_eq!(metrics.states_missing_in_shard, 1);
        assert_eq!(metrics.removed_unknown_pair, 1);
        assert!(metrics.has_anomalies());

        assert_eq!(metrics.anomaly_samples.len(), 5);
        assert!(metrics
            .anomaly_samples
            .iter()
            .any(|sample| sample == "new_pairs_missing_state:missing-state"));
        assert!(metrics
            .anomaly_samples
            .iter()
            .any(|sample| sample.starts_with("unknown_protocol_new_pairs:")));
        assert!(metrics
            .anomaly_samples
            .iter()
            .any(|sample| sample == "updates_for_unknown_pair:unknown-update"));
        assert!(metrics
            .anomaly_samples
            .iter()
            .any(|sample| sample == "states_missing_in_shard:stale-shard"));
        assert!(metrics
            .anomaly_samples
            .iter()
            .any(|sample| sample == "removed_unknown_pair:missing-removal"));
    }

    #[tokio::test]
    async fn apply_update_caps_anomaly_samples() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let mut states: HashMap<String, Box<dyn ProtocolSim>> = HashMap::new();
        for idx in 0..10 {
            states.insert(format!("unknown-{idx}"), Box::new(DummySim));
        }

        let metrics = store
            .apply_update(Update::new(7, states, HashMap::new()))
            .await;

        assert_eq!(metrics.updates_for_unknown_pair, 10);
        assert_eq!(metrics.anomaly_samples.len(), UPDATE_ANOMALY_SAMPLE_CAP);
        assert!(metrics
            .anomaly_samples
            .iter()
            .all(|sample| sample.starts_with("updates_for_unknown_pair:")));
    }

    #[tokio::test]
    async fn apply_update_without_anomalies_has_zero_counters() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let token_a = mk_token(40, "TKNA");
        let token_b = mk_token(41, "TKNB");
        let component = mk_component(42, "uniswap_v2", "uniswap_v2_pool", vec![token_a, token_b]);

        let metrics = store
            .apply_update(mk_update(vec![(
                "pool-1".to_string(),
                component,
                Box::new(DummySim),
            )]))
            .await;

        assert_eq!(metrics.new_pairs_missing_state, 0);
        assert_eq!(metrics.unknown_protocol_new_pairs, 0);
        assert_eq!(metrics.updates_for_unknown_pair, 0);
        assert_eq!(metrics.states_missing_in_shard, 0);
        assert_eq!(metrics.removed_unknown_pair, 0);
        assert!(metrics.anomaly_samples.is_empty());
        assert!(!metrics.has_anomalies());
    }

    #[tokio::test]
    async fn reset_protocols_preserves_unaffected_pools() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let token_a = mk_token(1, "TKNA");
        let token_b = mk_token(2, "TKNB");
        let token_c = mk_token(3, "TKNC");
        let token_d = mk_token(4, "TKND");

        let component_v2 = mk_component(
            10,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![token_a.clone(), token_b.clone()],
        );
        let component_v3 = mk_component(
            11,
            "uniswap_v3",
            "uniswap_v3_pool",
            vec![token_c.clone(), token_d.clone()],
        );

        let update = mk_update(vec![
            ("pool-v2".to_string(), component_v2, Box::new(DummySim)),
            ("pool-v3".to_string(), component_v3, Box::new(DummySim)),
        ]);
        store.apply_update(update).await;

        assert_eq!(store.total_states().await, 2);
        assert!(store.is_ready());

        store.reset_protocols(&[ProtocolKind::UniswapV2]).await;

        assert_eq!(store.total_states().await, 1);
        assert!(store.is_ready());

        let remaining = store
            .matching_pools_by_addresses(&token_c.address, &token_d.address)
            .await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].0, "pool-v3");

        let cleared = store
            .matching_pools_by_addresses(&token_a.address, &token_b.address)
            .await;
        assert!(cleared.is_empty());
    }

    #[tokio::test]
    async fn reset_protocols_updates_ready_when_empty() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let token_a = mk_token(5, "TKN1");
        let token_b = mk_token(6, "TKN2");
        let component = mk_component(12, "uniswap_v2", "uniswap_v2_pool", vec![token_a, token_b]);

        let update = mk_update(vec![(
            "pool-only".to_string(),
            component,
            Box::new(DummySim),
        )]);
        store.apply_update(update).await;

        assert!(store.is_ready());

        store.reset_protocols(&[ProtocolKind::UniswapV2]).await;

        assert_eq!(store.total_states().await, 0);
        assert!(!store.is_ready());
    }

    #[tokio::test]
    async fn total_pools_includes_vm_store() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let native_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let token_a = mk_token(7, "TKNA");
        let token_b = mk_token(8, "TKNB");

        let native_component = mk_component(
            20,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![token_a.clone(), token_b.clone()],
        );
        let vm_component = mk_component(21, "vm:curve", "curve_pool", vec![token_a, token_b]);

        native_store
            .apply_update(mk_update(vec![(
                "pool-native".to_string(),
                native_component,
                Box::new(DummySim),
            )]))
            .await;
        vm_store
            .apply_update(mk_update(vec![(
                "pool-vm".to_string(),
                vm_component,
                Box::new(DummySim),
            )]))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_store),
            vm_state_store: Arc::clone(&vm_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: true,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(100),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_millis(1000),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        assert_eq!(app_state.total_pools().await, 2);
        assert_eq!(app_state.current_vm_block().await, Some(1));
    }
    #[tokio::test]
    async fn matching_pools_includes_native_for_wrapped_native() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let weth_address =
            Bytes::from_str("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2").expect("valid address");
        let weth_token = Token::new(&weth_address, "WETH", 18, 0, &[], Chain::Ethereum, 100);
        let native_address = Bytes::from([0u8; 20]);
        let native_token = Token::new(&native_address, "ETH", 18, 0, &[], Chain::Ethereum, 100);
        let token_x = mk_token(9, "TKNX");

        let component_wrapped = mk_component(
            20,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![weth_token, token_x.clone()],
        );
        let component_native = mk_component(
            21,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![native_token, token_x.clone()],
        );

        let update = mk_update(vec![
            (
                "pool-weth".to_string(),
                component_wrapped,
                Box::new(DummySim),
            ),
            (
                "pool-native".to_string(),
                component_native,
                Box::new(DummySim),
            ),
        ]);
        store.apply_update(update).await;

        let matches = store
            .matching_pools_by_addresses(&weth_address, &token_x.address)
            .await;
        let ids: HashSet<String> = matches.into_iter().map(|(id, _)| id).collect();

        assert_eq!(ids.len(), 2);
        assert!(ids.contains("pool-weth"));
        assert!(ids.contains("pool-native"));
    }

    #[tokio::test]
    async fn matching_pools_includes_native_only_pool_for_wrapped_native() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let weth_address =
            Bytes::from_str("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2").expect("valid address");
        let native_address = Bytes::from([0u8; 20]);
        let native_token = Token::new(&native_address, "ETH", 18, 0, &[], Chain::Ethereum, 100);
        let token_x = mk_token(9, "TKNX");

        let component_native = mk_component(
            21,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![native_token, token_x.clone()],
        );

        let update = mk_update(vec![(
            "pool-native".to_string(),
            component_native,
            Box::new(DummySim),
        )]);
        store.apply_update(update).await;

        let matches = store
            .matching_pools_by_addresses(&weth_address, &token_x.address)
            .await;
        let ids: HashSet<String> = matches.into_iter().map(|(id, _)| id).collect();

        assert_eq!(ids.len(), 1);
        assert!(ids.contains("pool-native"));
    }

    #[tokio::test]
    async fn matching_pools_includes_wrapped_for_native_token() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let weth_address =
            Bytes::from_str("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2").expect("valid address");
        let weth_token = Token::new(&weth_address, "WETH", 18, 0, &[], Chain::Ethereum, 100);
        let native_address = Bytes::from([0u8; 20]);
        let native_token = Token::new(&native_address, "ETH", 18, 0, &[], Chain::Ethereum, 100);
        let token_x = mk_token(9, "TKNX");

        let component_wrapped = mk_component(
            22,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![weth_token, token_x.clone()],
        );
        let component_native = mk_component(
            23,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![native_token, token_x.clone()],
        );

        let update = mk_update(vec![
            (
                "pool-weth".to_string(),
                component_wrapped,
                Box::new(DummySim),
            ),
            (
                "pool-native".to_string(),
                component_native,
                Box::new(DummySim),
            ),
        ]);
        store.apply_update(update).await;

        let matches = store
            .matching_pools_by_addresses(&native_address, &token_x.address)
            .await;
        let ids: HashSet<String> = matches.into_iter().map(|(id, _)| id).collect();

        assert_eq!(ids.len(), 2);
        assert!(ids.contains("pool-weth"));
        assert!(ids.contains("pool-native"));
    }

    #[tokio::test]
    async fn matching_pools_includes_wrapped_only_pool_for_native_token() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let weth_address =
            Bytes::from_str("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2").expect("valid address");
        let weth_token = Token::new(&weth_address, "WETH", 18, 0, &[], Chain::Ethereum, 100);
        let native_address = Bytes::from([0u8; 20]);
        let token_x = mk_token(9, "TKNX");

        let component_wrapped = mk_component(
            24,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![weth_token, token_x.clone()],
        );

        let update = mk_update(vec![(
            "pool-weth".to_string(),
            component_wrapped,
            Box::new(DummySim),
        )]);
        store.apply_update(update).await;

        let matches = store
            .matching_pools_by_addresses(&native_address, &token_x.address)
            .await;
        let ids: HashSet<String> = matches.into_iter().map(|(id, _)| id).collect();

        assert_eq!(ids.len(), 1);
        assert!(ids.contains("pool-weth"));
    }

    #[tokio::test]
    async fn matching_pools_does_not_include_native_for_non_wrapped_token() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let native_address = Bytes::from([0u8; 20]);
        let native_token = Token::new(&native_address, "ETH", 18, 0, &[], Chain::Ethereum, 100);
        let token_x = mk_token(9, "TKNX");
        let token_y = mk_token(10, "TKNY");

        let component_native = mk_component(
            21,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![native_token, token_x.clone()],
        );

        let update = mk_update(vec![(
            "pool-native".to_string(),
            component_native,
            Box::new(DummySim),
        )]);
        store.apply_update(update).await;

        let matches = store
            .matching_pools_by_addresses(&token_y.address, &token_x.address)
            .await;

        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn matching_pools_does_not_union_native_wrapped_opposite_pair() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let native_address = Bytes::from([0u8; 20]);
        let native_token = Token::new(&native_address, "ETH", 18, 0, &[], Chain::Ethereum, 100);
        let weth_address =
            Bytes::from_str("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2").expect("valid address");
        let weth_token = Token::new(&weth_address, "WETH", 18, 0, &[], Chain::Ethereum, 100);
        let token_x = mk_token(11, "TKNX");

        let component_native_x = mk_component(
            30,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![native_token.clone(), token_x.clone()],
        );
        let component_weth_x = mk_component(
            31,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![weth_token.clone(), token_x],
        );
        let component_native_weth = mk_component(
            32,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![native_token, weth_token],
        );

        let update = mk_update(vec![
            (
                "pool-native-x".to_string(),
                component_native_x,
                Box::new(DummySim),
            ),
            (
                "pool-weth-x".to_string(),
                component_weth_x,
                Box::new(DummySim),
            ),
            (
                "pool-native-weth".to_string(),
                component_native_weth,
                Box::new(DummySim),
            ),
        ]);
        store.apply_update(update).await;

        let native_to_weth = store
            .matching_pools_by_addresses(&native_address, &weth_address)
            .await;
        let native_to_weth_ids: HashSet<String> =
            native_to_weth.into_iter().map(|(id, _)| id).collect();

        assert_eq!(native_to_weth_ids.len(), 1);
        assert!(native_to_weth_ids.contains("pool-native-weth"));

        let weth_to_native = store
            .matching_pools_by_addresses(&weth_address, &native_address)
            .await;
        let weth_to_native_ids: HashSet<String> =
            weth_to_native.into_iter().map(|(id, _)| id).collect();

        assert_eq!(weth_to_native_ids.len(), 1);
        assert!(weth_to_native_ids.contains("pool-native-weth"));
    }

    #[tokio::test]
    async fn matching_pools_reads_token_ids_from_single_snapshot() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = Arc::new(StateStore::new(token_store));

        let token_a = mk_token(70, "TKNA");
        let token_b = mk_token(71, "TKNB");

        let component_v1 = mk_component(
            40,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![token_a.clone(), token_b.clone()],
        );
        let component_v2 = mk_component(
            41,
            "uniswap_v2",
            "uniswap_v2_pool",
            vec![token_a.clone(), token_b.clone()],
        );

        store
            .apply_update(mk_update(vec![
                ("pool-v1".to_string(), component_v1, Box::new(DummySim)),
                ("pool-v2".to_string(), component_v2, Box::new(DummySim)),
            ]))
            .await;

        let token_a_address = token_a.address.clone();
        let token_b_address = token_b.address.clone();
        let mut index_guard = store.token_index.write().await;
        *index_guard = HashMap::from([
            (
                token_a_address.clone(),
                HashSet::from(["pool-v1".to_string()]),
            ),
            (
                token_b_address.clone(),
                HashSet::from(["pool-v1".to_string()]),
            ),
        ]);

        let query_store = Arc::clone(&store);
        let query_token_a = token_a_address.clone();
        let query_token_b = token_b_address.clone();
        let query_task = tokio::spawn(async move {
            query_store
                .matching_pools_by_addresses(&query_token_a, &query_token_b)
                .await
        });

        // Queue the reader before the writer while an external write lock is held.
        // The previous two-read implementation could then observe v1 for token_a and v2
        // for token_b, causing a transient empty intersection.
        tokio::task::yield_now().await;

        let writer_store = Arc::clone(&store);
        let writer_token_a = token_a_address;
        let writer_token_b = token_b_address;
        let writer_task = tokio::spawn(async move {
            let mut index = writer_store.token_index.write().await;
            *index = HashMap::from([
                (writer_token_a, HashSet::from(["pool-v2".to_string()])),
                (writer_token_b, HashSet::from(["pool-v2".to_string()])),
            ]);
        });

        drop(index_guard);

        writer_task.await.expect("writer task should succeed");
        let matches = query_task.await.expect("query task should succeed");
        let ids: HashSet<String> = matches.into_iter().map(|(id, _)| id).collect();

        assert_eq!(ids, HashSet::from(["pool-v1".to_string()]));
    }

    #[tokio::test]
    async fn app_state_native_gas_price_defaults_to_none() {
        let app_state = AppState {
            tokens: Arc::new(TokenStore::new(
                HashMap::new(),
                "http://localhost".to_string(),
                "test".to_string(),
                Chain::Ethereum,
                Duration::from_millis(10),
            )),
            native_state_store: Arc::new(StateStore::new(Arc::new(TokenStore::new(
                HashMap::new(),
                "http://localhost".to_string(),
                "test".to_string(),
                Chain::Ethereum,
                Duration::from_millis(10),
            )))),
            vm_state_store: Arc::new(StateStore::new(Arc::new(TokenStore::new(
                HashMap::new(),
                "http://localhost".to_string(),
                "test".to_string(),
                Chain::Ethereum,
                Duration::from_millis(10),
            )))),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: true,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(100),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_millis(1000),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        assert_eq!(app_state.latest_native_gas_price_wei().await, None);
    }

    #[tokio::test]
    async fn app_state_native_gas_price_updates() {
        let app_state = AppState {
            tokens: Arc::new(TokenStore::new(
                HashMap::new(),
                "http://localhost".to_string(),
                "test".to_string(),
                Chain::Ethereum,
                Duration::from_millis(10),
            )),
            native_state_store: Arc::new(StateStore::new(Arc::new(TokenStore::new(
                HashMap::new(),
                "http://localhost".to_string(),
                "test".to_string(),
                Chain::Ethereum,
                Duration::from_millis(10),
            )))),
            vm_state_store: Arc::new(StateStore::new(Arc::new(TokenStore::new(
                HashMap::new(),
                "http://localhost".to_string(),
                "test".to_string(),
                Chain::Ethereum,
                Duration::from_millis(10),
            )))),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: true,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(100),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_millis(1000),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        app_state.set_latest_native_gas_price_wei(Some(42)).await;
        assert_eq!(app_state.latest_native_gas_price_wei().await, Some(42));
    }

    #[tokio::test]
    async fn app_state_effective_native_gas_price_for_quotes_serializes_with_disable_transition() {
        let app_state = AppState {
            tokens: Arc::new(TokenStore::new(
                HashMap::new(),
                "http://localhost".to_string(),
                "test".to_string(),
                Chain::Ethereum,
                Duration::from_millis(10),
            )),
            native_state_store: Arc::new(StateStore::new(Arc::new(TokenStore::new(
                HashMap::new(),
                "http://localhost".to_string(),
                "test".to_string(),
                Chain::Ethereum,
                Duration::from_millis(10),
            )))),
            vm_state_store: Arc::new(StateStore::new(Arc::new(TokenStore::new(
                HashMap::new(),
                "http://localhost".to_string(),
                "test".to_string(),
                Chain::Ethereum,
                Duration::from_millis(10),
            )))),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(RwLock::new(Some(42))),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(true)),
            enable_vm_pools: true,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(100),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_millis(1000),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        assert_eq!(
            app_state.effective_native_gas_price_wei_for_quotes().await,
            Some(42)
        );

        let reporting_read_guard = app_state.native_gas_price_reporting_enabled.read().await;
        let state_for_disable = app_state.clone();
        let disable_task = tokio::spawn(async move {
            state_for_disable
                .set_native_gas_price_reporting_enabled(false)
                .await;
        });
        tokio::task::yield_now().await;
        assert!(!disable_task.is_finished());

        drop(reporting_read_guard);
        disable_task.await.expect("disable task should complete");

        assert_eq!(
            app_state.effective_native_gas_price_wei_for_quotes().await,
            None
        );
    }
}
