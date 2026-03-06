use std::any::Any;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::NaiveDateTime;
use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};
use tokio::sync::{RwLock, Semaphore};
use tower::ServiceExt;
use tycho_simulation::protocol::models::{ProtocolComponent, Update};
use tycho_simulation::tycho_common::dto::ProtocolStateDelta;
use tycho_simulation::tycho_common::models::{token::Token, Chain};
use tycho_simulation::tycho_common::simulation::errors::{SimulationError, TransitionError};
use tycho_simulation::tycho_common::simulation::protocol_sim::{
    Balances, GetAmountOutResult, ProtocolSim,
};
use tycho_simulation::tycho_common::Bytes;
use tycho_simulation_server::api::create_router;
use tycho_simulation_server::models::state::{AppState, StateStore, VmStreamStatus};
use tycho_simulation_server::models::stream_health::StreamHealth;
use tycho_simulation_server::models::tokens::TokenStore;

const GAS_PRICE_WEI: u128 = 50_000_000_000; // 50 gwei
const WEI_PER_ETH: u128 = 1_000_000_000_000_000_000;
const SPOT_PRICE_SCALE: u128 = 1_000_000_000;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct SimPool {
    gas_ladder: Vec<u64>,
    spot_quotes: Vec<(Bytes, Bytes, f64)>,
    #[serde(skip, default = "default_calls")]
    calls: Arc<AtomicUsize>,
}

fn default_calls() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

#[typetag::serde]
impl ProtocolSim for SimPool {
    fn fee(&self) -> f64 {
        0.0
    }

    fn spot_price(&self, base: &Token, quote: &Token) -> Result<f64, SimulationError> {
        for (base_address, quote_address, price) in &self.spot_quotes {
            if *base_address == base.address && *quote_address == quote.address {
                return Ok(*price);
            }
        }
        Err(SimulationError::FatalError(
            "spot price not available".to_string(),
        ))
    }

    fn get_amount_out(
        &self,
        amount_in: BigUint,
        _token_in: &Token,
        _token_out: &Token,
    ) -> Result<GetAmountOutResult, SimulationError> {
        let idx = self.calls.fetch_add(1, Ordering::SeqCst);
        let gas = self
            .gas_ladder
            .get(idx)
            .copied()
            .or_else(|| self.gas_ladder.last().copied())
            .unwrap_or(0);

        Ok(GetAmountOutResult::new(
            amount_in.clone(),
            BigUint::from(gas),
            self.clone_box(),
        ))
    }

    fn get_limits(
        &self,
        _sell_token: Bytes,
        _buy_token: Bytes,
    ) -> Result<(BigUint, BigUint), SimulationError> {
        Ok((BigUint::from(u128::MAX), BigUint::zero()))
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
        other.as_any().is::<SimPool>()
    }
}

fn expected_gas_in_sell_base_units(
    gas_used: u64,
    sell_decimals: u32,
    eth_to_sell_spot: f64,
) -> u128 {
    let spot_scaled = (eth_to_sell_spot * SPOT_PRICE_SCALE as f64).floor() as u128;
    let numerator = BigUint::from(gas_used)
        * BigUint::from(GAS_PRICE_WEI)
        * BigUint::from(spot_scaled)
        * BigUint::from(10u32).pow(sell_decimals);
    let denominator = BigUint::from(SPOT_PRICE_SCALE) * BigUint::from(WEI_PER_ETH);
    (numerator / denominator)
        .to_u128()
        .expect("expected gas_in_sell to fit into u128")
}

fn make_token(address: &Bytes, symbol: &str, decimals: u32) -> Token {
    Token::new(address, symbol, decimals, 0, &[], Chain::Ethereum, 100)
}

fn make_component(
    pool_address: &str,
    token_a: Token,
    token_b: Token,
    protocol_system: &str,
    protocol_type_name: &str,
) -> ProtocolComponent {
    ProtocolComponent::new(
        Bytes::from_str(pool_address).expect("valid pool address"),
        protocol_system.to_string(),
        protocol_type_name.to_string(),
        Chain::Ethereum,
        vec![token_a, token_b],
        Vec::new(),
        HashMap::new(),
        Bytes::default(),
        NaiveDateTime::default(),
    )
}

fn make_app_state(
    token_store: Arc<TokenStore>,
    native_state_store: Arc<StateStore>,
    vm_state_store: Arc<StateStore>,
    gas_price_wei: Option<u128>,
    gas_reporting_enabled: bool,
) -> AppState {
    AppState {
        tokens: token_store,
        native_state_store,
        vm_state_store,
        native_stream_health: Arc::new(StreamHealth::new()),
        vm_stream_health: Arc::new(StreamHealth::new()),
        vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
        latest_native_gas_price_wei: Arc::new(RwLock::new(gas_price_wei)),
        native_gas_price_reporting_enabled: Arc::new(RwLock::new(gas_reporting_enabled)),
        enable_vm_pools: false,
        readiness_stale: Duration::from_secs(120),
        quote_timeout: Duration::from_secs(1),
        pool_timeout_native: Duration::from_millis(200),
        pool_timeout_vm: Duration::from_millis(200),
        request_timeout: Duration::from_secs(2),
        native_sim_semaphore: Arc::new(Semaphore::new(1)),
        vm_sim_semaphore: Arc::new(Semaphore::new(1)),
        reset_allowance_tokens: Arc::new(HashMap::new()),
        native_sim_concurrency: 1,
        vm_sim_concurrency: 1,
    }
}

async fn post_simulate(router: axum::Router, payload: serde_json::Value) -> serde_json::Value {
    let response = router
        .oneshot(
            Request::post("/simulate")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .expect("request"),
        )
        .await
        .expect("router response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    serde_json::from_slice(&body).expect("json body")
}

#[tokio::test]
async fn simulate_gas_in_sell_uses_direct_conversion_for_wrapped_native_sell_token() {
    let weth = Chain::Ethereum.wrapped_native_token().address;
    let usdc = Bytes::from_str("0xA0b86991c6218b36c1d19d4a2e9Eb0cE3606eB48").unwrap();

    let weth_token = make_token(&weth, "WETH", 18);
    let usdc_token = make_token(&usdc, "USDC", 6);

    let token_store = Arc::new(TokenStore::new(
        HashMap::from([
            (weth.clone(), weth_token.clone()),
            (usdc.clone(), usdc_token.clone()),
        ]),
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_secs(1),
    ));
    let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

    let mut states: HashMap<String, Box<dyn ProtocolSim>> = HashMap::new();
    let mut new_pairs = HashMap::new();
    states.insert(
        "pool-weth-usdc".to_string(),
        Box::new(SimPool {
            gas_ladder: vec![120_000, 150_000],
            spot_quotes: Vec::new(),
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        "pool-weth-usdc".to_string(),
        make_component(
            "0x0000000000000000000000000000000000000101",
            weth_token,
            usdc_token,
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    native_state_store
        .apply_update(Update::new(1, states, new_pairs))
        .await;

    let app_state = make_app_state(
        Arc::clone(&token_store),
        native_state_store,
        vm_state_store,
        Some(1),
        true,
    );

    let response = post_simulate(
        create_router(app_state),
        serde_json::json!({
            "request_id": "req-direct-weth-gas",
            "token_in": weth.to_string(),
            "token_out": usdc.to_string(),
            "amounts": ["2", "5"]
        }),
    )
    .await;

    let entry = &response["data"][0];
    assert_eq!(entry["gas_used"], serde_json::json!([120000, 150000]));
    assert_eq!(entry["gas_in_sell"], "150000");
}

#[tokio::test]
async fn simulate_gas_in_sell_uses_sell_token_spot_pool_for_non_native_sell_token() {
    let usdt = Bytes::from_str("0xdAC17F958D2ee523a2206206994597C13D831ec7").unwrap();
    let usdc = Bytes::from_str("0xA0b86991c6218b36c1d19d4a2e9Eb0cE3606eB48").unwrap();
    let weth = Chain::Ethereum.wrapped_native_token().address;

    let usdt_token = make_token(&usdt, "USDT", 6);
    let usdc_token = make_token(&usdc, "USDC", 6);
    let weth_token = make_token(&weth, "WETH", 18);

    let token_store = Arc::new(TokenStore::new(
        HashMap::from([
            (usdt.clone(), usdt_token.clone()),
            (usdc.clone(), usdc_token.clone()),
            (weth.clone(), weth_token.clone()),
        ]),
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_secs(1),
    ));
    let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

    let mut states: HashMap<String, Box<dyn ProtocolSim>> = HashMap::new();
    let mut new_pairs = HashMap::new();
    states.insert(
        "pool-usdt-usdc".to_string(),
        Box::new(SimPool {
            gas_ladder: vec![120_000, 150_000],
            spot_quotes: Vec::new(),
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        "pool-usdt-usdc".to_string(),
        make_component(
            "0x0000000000000000000000000000000000000102",
            usdt_token.clone(),
            usdc_token,
            "uniswap_v2",
            "uniswap_v2",
        ),
    );
    states.insert(
        "pool-spot-usdt-weth".to_string(),
        Box::new(SimPool {
            gas_ladder: vec![0],
            spot_quotes: vec![(weth.clone(), usdt.clone(), 2_000.0)],
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        "pool-spot-usdt-weth".to_string(),
        make_component(
            "0x0000000000000000000000000000000000000201",
            usdt_token.clone(),
            weth_token,
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    native_state_store
        .apply_update(Update::new(1, states, new_pairs))
        .await;

    let app_state = make_app_state(
        Arc::clone(&token_store),
        native_state_store,
        vm_state_store,
        Some(GAS_PRICE_WEI),
        true,
    );

    let response = post_simulate(
        create_router(app_state),
        serde_json::json!({
            "request_id": "req-usdt-gas",
            "token_in": usdt.to_string(),
            "token_out": usdc.to_string(),
            "amounts": ["2", "5"]
        }),
    )
    .await;

    let entry = &response["data"][0];
    let gas_in_sell = entry["gas_in_sell"]
        .as_str()
        .and_then(|value| value.parse::<u128>().ok())
        .expect("gas_in_sell must be a u128 string");

    assert_eq!(entry["gas_used"], serde_json::json!([120000, 150000]));
    assert_eq!(
        gas_in_sell,
        expected_gas_in_sell_base_units(150_000, usdt_token.decimals, 2_000.0)
    );
}

#[tokio::test]
async fn simulate_gas_in_sell_is_zero_when_reporting_disabled() {
    let usdt = Bytes::from_str("0xdAC17F958D2ee523a2206206994597C13D831ec7").unwrap();
    let usdc = Bytes::from_str("0xA0b86991c6218b36c1d19d4a2e9Eb0cE3606eB48").unwrap();

    let usdt_token = make_token(&usdt, "USDT", 6);
    let usdc_token = make_token(&usdc, "USDC", 6);

    let token_store = Arc::new(TokenStore::new(
        HashMap::from([
            (usdt.clone(), usdt_token.clone()),
            (usdc.clone(), usdc_token.clone()),
        ]),
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_secs(1),
    ));
    let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

    let mut states: HashMap<String, Box<dyn ProtocolSim>> = HashMap::new();
    let mut new_pairs = HashMap::new();
    states.insert(
        "pool-usdt-usdc".to_string(),
        Box::new(SimPool {
            gas_ladder: vec![120_000, 150_000],
            spot_quotes: Vec::new(),
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        "pool-usdt-usdc".to_string(),
        make_component(
            "0x0000000000000000000000000000000000000102",
            usdt_token,
            usdc_token,
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    native_state_store
        .apply_update(Update::new(1, states, new_pairs))
        .await;

    let app_state = make_app_state(
        Arc::clone(&token_store),
        native_state_store,
        vm_state_store,
        Some(1),
        false,
    );

    let response = post_simulate(
        create_router(app_state),
        serde_json::json!({
            "request_id": "req-reporting-disabled",
            "token_in": usdt.to_string(),
            "token_out": usdc.to_string(),
            "amounts": ["2", "5"]
        }),
    )
    .await;

    let entry = &response["data"][0];
    assert_eq!(entry["gas_used"], serde_json::json!([120000, 150000]));
    assert_eq!(entry["gas_in_sell"], "0");
}
