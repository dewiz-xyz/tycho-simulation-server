use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, RwLock, Semaphore};
use tokio::time::Instant;
use tracing::{debug, warn};
use tycho_simulation::{
    protocol::models::{ProtocolComponent, Update},
    tycho_common::{models::token::Token, simulation::protocol_sim::ProtocolSim, Bytes},
};

use super::{protocol::ProtocolKind, stream_health::StreamHealth, tokens::TokenStore};

#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<TokenStore>,
    pub native_state_store: Arc<StateStore>,
    pub vm_state_store: Arc<StateStore>,
    pub native_stream_health: Arc<StreamHealth>,
    pub vm_stream_health: Arc<StreamHealth>,
    pub vm_stream: Arc<RwLock<VmStreamStatus>>,
    pub enable_vm_pools: bool,
    pub readiness_stale: Duration,
    pub quote_timeout: Duration,
    pub pool_timeout_native: Duration,
    pub pool_timeout_vm: Duration,
    pub request_timeout: Duration,
    pub native_sim_semaphore: Arc<Semaphore>,
    pub vm_sim_semaphore: Arc<Semaphore>,
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

    pub async fn has_pool(&self, id: &str) -> bool {
        self.id_to_kind.read().await.contains_key(id)
    }

    pub async fn pool_components_by_kind(
        &self,
        kind: ProtocolKind,
    ) -> Vec<(String, Arc<ProtocolComponent>)> {
        let Some(shard) = self.shards.get(&kind) else {
            return Vec::new();
        };
        let guard = shard.states.read().await;
        guard
            .iter()
            .map(|(id, (_, component))| (id.clone(), Arc::clone(component)))
            .collect()
    }

    pub async fn pool_entries_by_kind(
        &self,
        kind: ProtocolKind,
    ) -> Vec<(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> {
        let Some(shard) = self.shards.get(&kind) else {
            return Vec::new();
        };
        let guard = shard.states.read().await;
        guard
            .iter()
            .map(|(id, (state, component))| (id.clone(), Arc::clone(state), Arc::clone(component)))
            .collect()
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

    async fn ids_for_token(&self, token: &Bytes) -> Option<HashSet<String>> {
        let wrapped_native_token = self.tokens.wrapped_native_token();
        let needs_native = wrapped_native_token
            .as_ref()
            .map_or(false, |weth| weth == token);
        let native_address = Bytes::from([0u8; 20]);

        let index = self.token_index.read().await;
        let mut ids = index.get(token).cloned()?;

        if needs_native {
            if let Some(native_ids) = index.get(&native_address) {
                ids.extend(native_ids.iter().cloned());
            }
        }

        Some(ids)
    }

    pub(crate) async fn matching_pools_by_addresses(
        &self,
        token_in: &Bytes,
        token_out: &Bytes,
    ) -> Vec<(String, PoolEntry)> {
        let (Some(token_in_ids), Some(token_out_ids)) = (
            self.ids_for_token(token_in).await,
            self.ids_for_token(token_out).await,
        ) else {
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

    #[derive(Debug, Clone)]
    struct DummySim;

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
            enable_vm_pools: true,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(100),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_millis(1000),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        assert_eq!(app_state.total_pools().await, 2);
        assert_eq!(app_state.current_vm_block().await, Some(1));
    }

    #[tokio::test]
    async fn pool_components_by_kind_filters_protocol() {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let store = StateStore::new(token_store);

        let token_a = mk_token(10, "TKNA");
        let token_b = mk_token(11, "TKNB");
        let token_c = mk_token(12, "TKNC");

        let curve_component = mk_component(
            30,
            "vm:curve",
            "curve_pool",
            vec![token_a.clone(), token_b.clone()],
        );
        let v2_component =
            mk_component(31, "uniswap_v2", "uniswap_v2_pool", vec![token_a, token_c]);

        store
            .apply_update(mk_update(vec![
                (
                    "pool-curve".to_string(),
                    curve_component,
                    Box::new(DummySim),
                ),
                ("pool-v2".to_string(), v2_component, Box::new(DummySim)),
            ]))
            .await;

        let curve_pools = store.pool_components_by_kind(ProtocolKind::Curve).await;
        assert_eq!(curve_pools.len(), 1);
        assert_eq!(curve_pools[0].0, "pool-curve");
        assert_eq!(curve_pools[0].1.protocol_system, "vm:curve");
    }

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
}
