use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::stream;
use jemalloc_ctl as jemalloc;
use jemallocator::Jemalloc;
use num_bigint::BigUint;
use tycho_simulation::protocol::models::{ProtocolComponent, Update};
use tycho_simulation::tycho_client::feed::{BlockHeader, SynchronizerState};
use tycho_simulation::tycho_common::{
    dto::ProtocolStateDelta,
    models::{token::Token, Chain},
    simulation::{
        errors::{SimulationError, TransitionError},
        protocol_sim::{Balances, GetAmountOutResult, ProtocolSim},
    },
    Bytes,
};
use tycho_simulation_server::handlers::stream::process_stream;
use tycho_simulation_server::models::state::StateStore;
use tycho_simulation_server::models::tokens::TokenStore;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

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

fn address(seed: u8) -> Bytes {
    Bytes::from([seed; 20])
}

fn make_token(seed: u8, symbol: &str) -> Token {
    Token::new(&address(seed), symbol, 18, 0, &[], Chain::Ethereum, 100)
}

fn make_component(
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

fn make_update(
    block: u64,
    pairs: Vec<(String, ProtocolComponent, Box<dyn ProtocolSim>)>,
    sync_states: HashMap<String, SynchronizerState>,
) -> Update {
    let mut states = HashMap::new();
    let mut new_pairs = HashMap::new();
    for (id, component, state) in pairs {
        states.insert(id.clone(), state);
        new_pairs.insert(id, component);
    }
    Update::new(block, states, new_pairs).set_sync_states(sync_states)
}

fn block_header(number: u64) -> BlockHeader {
    BlockHeader {
        number,
        timestamp: number,
        ..Default::default()
    }
}

fn ready_state(protocol: &str, number: u64) -> (String, SynchronizerState) {
    (
        protocol.to_string(),
        SynchronizerState::Ready(block_header(number)),
    )
}

fn advanced_state(protocol: &str, number: u64) -> (String, SynchronizerState) {
    (
        protocol.to_string(),
        SynchronizerState::Advanced(block_header(number)),
    )
}

fn ok_update(update: Update) -> Result<Update, Box<dyn std::error::Error + Send + Sync + 'static>> {
    Ok(update)
}

fn build_pairs(
    protocol_system: &str,
    protocol_type_name: &str,
    seed_offset: u8,
    count: usize,
) -> Vec<(String, ProtocolComponent, Box<dyn ProtocolSim>)> {
    let mut pairs: Vec<(String, ProtocolComponent, Box<dyn ProtocolSim>)> =
        Vec::with_capacity(count);
    for i in 0..count {
        let seed = seed_offset as u16 + (i as u16) * 2;
        assert!(
            seed <= u8::MAX as u16,
            "seed offset too large for deterministic ids"
        );
        let seed = seed as u8;
        let next_seed = seed.checked_add(1).expect("seed overflow");
        let symbol_a = format!("T{}", seed);
        let symbol_b = format!("T{}", next_seed);
        let token_a = make_token(seed, &symbol_a);
        let token_b = make_token(next_seed, &symbol_b);
        let id = format!("{}-{}", protocol_system, seed);
        let component = make_component(
            seed,
            protocol_system,
            protocol_type_name,
            vec![token_a, token_b],
        );
        pairs.push((id, component, Box::new(DummySim) as Box<dyn ProtocolSim>));
    }
    pairs
}

fn jemalloc_allocated_bytes() -> usize {
    // Jemalloc requires epoch advancement to refresh stats.
    jemalloc::epoch::advance().expect("jemalloc epoch advance");
    jemalloc::stats::allocated::read().expect("jemalloc allocated bytes")
}

#[tokio::test]
async fn protocol_reset_preserves_unaffected_pools() {
    let token_store = Arc::new(TokenStore::new(
        HashMap::new(),
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_secs(1),
    ));
    let state_store = Arc::new(StateStore::new(token_store));

    let token_a = make_token(10, "TKNA");
    let token_b = make_token(11, "TKNB");
    let token_c = make_token(12, "TKNC");
    let token_d = make_token(13, "TKND");

    let component_v2 = make_component(50, "uniswap_v2", "uniswap_v2_pool", vec![token_a, token_b]);
    let component_v3 = make_component(51, "uniswap_v3", "uniswap_v3_pool", vec![token_c, token_d]);

    let mut initial_states = HashMap::new();
    initial_states.insert(
        "pool-v2".to_string(),
        Box::new(DummySim) as Box<dyn ProtocolSim>,
    );
    initial_states.insert(
        "pool-v3".to_string(),
        Box::new(DummySim) as Box<dyn ProtocolSim>,
    );
    let mut initial_pairs = HashMap::new();
    initial_pairs.insert("pool-v2".to_string(), component_v2);
    initial_pairs.insert("pool-v3".to_string(), component_v3);
    let initial_update =
        Update::new(1, initial_states, initial_pairs).set_sync_states(HashMap::from([
            ready_state("uniswap_v2", 1),
            ready_state("uniswap_v3", 1),
        ]));

    process_stream(
        stream::iter(vec![ok_update(initial_update)]),
        Arc::clone(&state_store),
    )
    .await;

    assert!(state_store.has_pool("pool-v2").await);
    assert!(state_store.has_pool("pool-v3").await);

    let component_v2_resync = make_component(
        52,
        "uniswap_v2",
        "uniswap_v2_pool",
        vec![make_token(14, "TKNX"), make_token(15, "TKNY")],
    );

    let resync_update = make_update(
        2,
        vec![(
            "pool-v2".to_string(),
            component_v2_resync,
            Box::new(DummySim),
        )],
        HashMap::from([
            advanced_state("uniswap_v2", 2),
            ready_state("uniswap_v3", 2),
        ]),
    );

    process_stream(
        stream::iter(vec![ok_update(resync_update)]),
        Arc::clone(&state_store),
    )
    .await;

    assert!(state_store.has_pool("pool-v2").await);
    assert!(state_store.has_pool("pool-v3").await);
    assert_eq!(state_store.total_states().await, 2);
}

#[tokio::test]
async fn overlapping_resyncs_reset_newly_advanced_protocols() {
    let token_store = Arc::new(TokenStore::new(
        HashMap::new(),
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_secs(1),
    ));
    let state_store = Arc::new(StateStore::new(token_store));

    let token_a = make_token(20, "TKA");
    let token_b = make_token(21, "TKB");
    let token_c = make_token(22, "TKC");
    let token_d = make_token(23, "TKD");

    let component_v2_old =
        make_component(70, "uniswap_v2", "uniswap_v2_pool", vec![token_a, token_b]);
    let component_v3_old =
        make_component(71, "uniswap_v3", "uniswap_v3_pool", vec![token_c, token_d]);

    let initial_update = make_update(
        1,
        vec![
            (
                "pool-v2-old".to_string(),
                component_v2_old,
                Box::new(DummySim),
            ),
            (
                "pool-v3-old".to_string(),
                component_v3_old,
                Box::new(DummySim),
            ),
        ],
        HashMap::from([ready_state("uniswap_v2", 1), ready_state("uniswap_v3", 1)]),
    );

    let component_v2_new = make_component(
        72,
        "uniswap_v2",
        "uniswap_v2_pool",
        vec![make_token(24, "TKE"), make_token(25, "TKF")],
    );
    let resync_v2_update = make_update(
        2,
        vec![(
            "pool-v2-new".to_string(),
            component_v2_new,
            Box::new(DummySim),
        )],
        HashMap::from([
            advanced_state("uniswap_v2", 2),
            ready_state("uniswap_v3", 2),
        ]),
    );

    let component_v3_new = make_component(
        73,
        "uniswap_v3",
        "uniswap_v3_pool",
        vec![make_token(26, "TKG"), make_token(27, "TKH")],
    );
    let resync_v3_update = make_update(
        3,
        vec![(
            "pool-v3-new".to_string(),
            component_v3_new,
            Box::new(DummySim),
        )],
        HashMap::from([
            advanced_state("uniswap_v2", 3),
            advanced_state("uniswap_v3", 3),
        ]),
    );

    process_stream(
        stream::iter(vec![
            ok_update(initial_update),
            ok_update(resync_v2_update),
            ok_update(resync_v3_update),
        ]),
        Arc::clone(&state_store),
    )
    .await;

    assert!(state_store.has_pool("pool-v2-new").await);
    assert!(state_store.has_pool("pool-v3-new").await);
    assert!(!state_store.has_pool("pool-v2-old").await);
    assert!(!state_store.has_pool("pool-v3-old").await);
    assert_eq!(state_store.total_states().await, 2);
}

#[tokio::test]
async fn jemalloc_memory_plateau_after_reset() {
    const POOL_COUNT: usize = 64;
    const DRIFT_FLOOR_BYTES: usize = 8 * 1024 * 1024;

    let token_store = Arc::new(TokenStore::new(
        HashMap::new(),
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_secs(1),
    ));
    let state_store = Arc::new(StateStore::new(token_store));

    let mut initial_pairs = build_pairs("uniswap_v2", "uniswap_v2_pool", 1, POOL_COUNT);
    initial_pairs.extend(build_pairs(
        "uniswap_v3",
        "uniswap_v3_pool",
        120,
        POOL_COUNT,
    ));

    let initial_update = make_update(
        10,
        initial_pairs,
        HashMap::from([ready_state("uniswap_v2", 10), ready_state("uniswap_v3", 10)]),
    );

    process_stream(
        stream::iter(vec![ok_update(initial_update)]),
        Arc::clone(&state_store),
    )
    .await;

    assert_eq!(state_store.total_states().await, POOL_COUNT * 2);
    let baseline = jemalloc_allocated_bytes();

    let resync_pairs = build_pairs("uniswap_v2", "uniswap_v2_pool", 1, POOL_COUNT);
    let resync_update = make_update(
        11,
        resync_pairs,
        HashMap::from([
            advanced_state("uniswap_v2", 11),
            ready_state("uniswap_v3", 11),
        ]),
    );

    process_stream(
        stream::iter(vec![ok_update(resync_update)]),
        Arc::clone(&state_store),
    )
    .await;

    assert_eq!(state_store.total_states().await, POOL_COUNT * 2);
    let after = jemalloc_allocated_bytes();

    let allowed_drift = (baseline / 20).max(DRIFT_FLOOR_BYTES);
    let delta = baseline.abs_diff(after);
    assert!(
        delta <= allowed_drift,
        "jemalloc allocated drifted too far: baseline={} after={} delta={} allowed={}",
        baseline,
        after,
        delta,
        allowed_drift
    );
}
