use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::NaiveDateTime;
use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};
use tokio::sync::Semaphore;
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
use tycho_simulation_server::models::messages::AmountOutRequest;
use tycho_simulation_server::models::state::{AppState, StateStore, VmStreamStatus};
use tycho_simulation_server::models::stream_health::StreamHealth;
use tycho_simulation_server::models::tokens::TokenStore;

const ETH_USD_PRICE: u128 = 2_000;
const GAS_PRICE_WEI: u128 = 50_000_000_000; // 50 gwei
const WEI_PER_ETH: u128 = 1_000_000_000_000_000_000;
const USD_MICROS: u128 = 1_000_000;
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
        for (b, q, price) in &self.spot_quotes {
            if *b == base.address && *q == quote.address {
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

#[derive(Clone, Copy)]
struct PairCase {
    request_id: &'static str,
    pool_id: &'static str,
    token_in: &'static str,
    token_out: &'static str,
    sell_decimals: u32,
    sell_usd_micros: u128,
    eth_to_sell_spot: f64,
    expected_last_gas: u64,
}

fn gas_cost_usd_micros(gas_used: u64) -> u128 {
    (u128::from(gas_used) * GAS_PRICE_WEI * ETH_USD_PRICE * USD_MICROS) / WEI_PER_ETH
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

#[tokio::test]
async fn simulate_gas_in_sell_matches_gas_price_cost_in_usd_for_multiple_pairs() {
    let usdt = Bytes::from_str("0xdAC17F958D2ee523a2206206994597C13D831ec7").unwrap();
    let usdc = Bytes::from_str("0xA0b86991c6218b36c1d19d4a2e9Eb0cE3606eB48").unwrap();
    let dai = Bytes::from_str("0x6B175474E89094C44Da98b954EedeAC495271d0F").unwrap();
    let wbtc = Bytes::from_str("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599").unwrap();
    let weth = Chain::Ethereum.wrapped_native_token().address;

    let usdt_token = make_token(&usdt, "USDT", 6);
    let usdc_token = make_token(&usdc, "USDC", 6);
    let dai_token = make_token(&dai, "DAI", 18);
    let wbtc_token = make_token(&wbtc, "WBTC", 8);
    let weth_token = make_token(&weth, "WETH", 18);

    let mut tokens = HashMap::new();
    tokens.insert(usdt.clone(), usdt_token.clone());
    tokens.insert(usdc.clone(), usdc_token.clone());
    tokens.insert(dai.clone(), dai_token.clone());
    tokens.insert(wbtc.clone(), wbtc_token.clone());
    tokens.insert(weth.clone(), weth_token.clone());

    let token_store = Arc::new(TokenStore::new(
        tokens,
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_secs(1),
    ));
    let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

    let mut states: HashMap<String, Box<dyn ProtocolSim>> = HashMap::new();
    let mut new_pairs = HashMap::new();

    // Quote pools used by /simulate requests.
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
            "0x0000000000000000000000000000000000000101",
            usdt_token.clone(),
            usdc_token.clone(),
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    states.insert(
        "pool-usdc-dai".to_string(),
        Box::new(SimPool {
            gas_ladder: vec![180_000, 210_000],
            spot_quotes: Vec::new(),
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        "pool-usdc-dai".to_string(),
        make_component(
            "0x0000000000000000000000000000000000000102",
            usdc_token.clone(),
            dai_token.clone(),
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    states.insert(
        "pool-dai-usdt".to_string(),
        Box::new(SimPool {
            gas_ladder: vec![160_000, 180_000],
            spot_quotes: Vec::new(),
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        "pool-dai-usdt".to_string(),
        make_component(
            "0x0000000000000000000000000000000000000103",
            dai_token.clone(),
            usdt_token.clone(),
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    states.insert(
        "pool-wbtc-usdc".to_string(),
        Box::new(SimPool {
            gas_ladder: vec![170_000, 190_000],
            spot_quotes: Vec::new(),
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        "pool-wbtc-usdc".to_string(),
        make_component(
            "0x0000000000000000000000000000000000000104",
            wbtc_token.clone(),
            usdc_token.clone(),
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    // Dedicated spot pools for ETH/sell conversion.
    states.insert(
        "pool-spot-usdt-weth".to_string(),
        Box::new(SimPool {
            gas_ladder: vec![0],
            spot_quotes: vec![(weth.clone(), usdt.clone(), ETH_USD_PRICE as f64)],
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        "pool-spot-usdt-weth".to_string(),
        make_component(
            "0x0000000000000000000000000000000000000201",
            usdt_token.clone(),
            weth_token.clone(),
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    states.insert(
        "pool-spot-usdc-weth".to_string(),
        Box::new(SimPool {
            gas_ladder: vec![0],
            spot_quotes: vec![(weth.clone(), usdc.clone(), ETH_USD_PRICE as f64)],
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        "pool-spot-usdc-weth".to_string(),
        make_component(
            "0x0000000000000000000000000000000000000202",
            usdc_token.clone(),
            weth_token.clone(),
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    states.insert(
        "pool-spot-dai-weth".to_string(),
        Box::new(SimPool {
            gas_ladder: vec![0],
            spot_quotes: vec![(weth.clone(), dai.clone(), ETH_USD_PRICE as f64)],
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        "pool-spot-dai-weth".to_string(),
        make_component(
            "0x0000000000000000000000000000000000000203",
            dai_token.clone(),
            weth_token.clone(),
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    states.insert(
        "pool-spot-wbtc-weth".to_string(),
        Box::new(SimPool {
            gas_ladder: vec![0],
            // ETH priced in WBTC (1 ETH = 0.03125 WBTC) for deterministic rounding.
            spot_quotes: vec![(weth.clone(), wbtc.clone(), 0.03125)],
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        "pool-spot-wbtc-weth".to_string(),
        make_component(
            "0x0000000000000000000000000000000000000204",
            wbtc_token.clone(),
            weth_token.clone(),
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    native_state_store
        .apply_update(Update::new(42, states, new_pairs))
        .await;

    let app_state = AppState {
        tokens: Arc::clone(&token_store),
        native_state_store: Arc::clone(&native_state_store),
        vm_state_store: Arc::clone(&vm_state_store),
        native_stream_health: Arc::new(StreamHealth::new()),
        vm_stream_health: Arc::new(StreamHealth::new()),
        vm_stream: Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
        latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(Some(GAS_PRICE_WEI))),
        enable_vm_pools: false,
        readiness_stale: Duration::from_secs(120),
        quote_timeout: Duration::from_secs(1),
        pool_timeout_native: Duration::from_millis(200),
        pool_timeout_vm: Duration::from_millis(200),
        request_timeout: Duration::from_secs(2),
        native_sim_semaphore: Arc::new(Semaphore::new(8)),
        vm_sim_semaphore: Arc::new(Semaphore::new(1)),
        reset_allowance_tokens: Arc::new(HashMap::<u64, HashSet<Bytes>>::new()),
        native_sim_concurrency: 8,
        vm_sim_concurrency: 1,
    };

    let app = create_router(app_state);

    let cases = vec![
        PairCase {
            request_id: "case-usdt-usdc",
            pool_id: "pool-usdt-usdc",
            token_in: "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            token_out: "0xA0b86991c6218b36c1d19d4a2e9Eb0cE3606eB48",
            sell_decimals: 6,
            sell_usd_micros: 1_000_000,
            eth_to_sell_spot: 2_000.0,
            expected_last_gas: 150_000,
        },
        PairCase {
            request_id: "case-usdc-dai",
            pool_id: "pool-usdc-dai",
            token_in: "0xA0b86991c6218b36c1d19d4a2e9Eb0cE3606eB48",
            token_out: "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            sell_decimals: 6,
            sell_usd_micros: 1_000_000,
            eth_to_sell_spot: 2_000.0,
            expected_last_gas: 210_000,
        },
        PairCase {
            request_id: "case-dai-usdt",
            pool_id: "pool-dai-usdt",
            token_in: "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            token_out: "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            sell_decimals: 18,
            sell_usd_micros: 1_000_000,
            eth_to_sell_spot: 2_000.0,
            expected_last_gas: 180_000,
        },
        PairCase {
            request_id: "case-wbtc-usdc",
            pool_id: "pool-wbtc-usdc",
            token_in: "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
            token_out: "0xA0b86991c6218b36c1d19d4a2e9Eb0cE3606eB48",
            sell_decimals: 8,
            sell_usd_micros: 64_000 * USD_MICROS,
            eth_to_sell_spot: 0.03125,
            expected_last_gas: 190_000,
        },
    ];

    for case in cases {
        let request = AmountOutRequest {
            request_id: case.request_id.to_string(),
            auction_id: None,
            token_in: case.token_in.to_string(),
            token_out: case.token_out.to_string(),
            amounts: vec!["1000000".to_string(), "2000000".to_string()],
        };

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/simulate")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&request).expect("serialize simulate request"),
                    ))
                    .expect("build request"),
            )
            .await
            .expect("simulate response");

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected status {}: {}",
            status,
            String::from_utf8_lossy(&body)
        );

        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("simulate response JSON");
        assert_eq!(value["meta"]["status"], "ready");

        let data = value["data"].as_array().expect("data must be array");
        let entry = data
            .iter()
            .find(|item| item["pool"] == case.pool_id)
            .unwrap_or_else(|| panic!("pool {} not found in simulate response", case.pool_id));

        let gas_used = entry["gas_used"]
            .as_array()
            .expect("gas_used must be array");
        let last_gas_used = gas_used
            .last()
            .and_then(|v| v.as_u64())
            .expect("last gas_used must be u64");
        assert_eq!(last_gas_used, case.expected_last_gas);

        let gas_in_sell = entry["gas_in_sell"]
            .as_str()
            .and_then(|v| v.parse::<u128>().ok())
            .expect("gas_in_sell must be u128 string");

        let gas_in_sell_usd_micros =
            (gas_in_sell * case.sell_usd_micros) / 10u128.pow(case.sell_decimals);
        let gas_price_cost_usd_micros = gas_cost_usd_micros(last_gas_used);
        let usd_diff = gas_in_sell_usd_micros.abs_diff(gas_price_cost_usd_micros);
        let usd_tolerance = (case.sell_usd_micros / 10u128.pow(case.sell_decimals)).max(1);
        assert!(
            usd_diff <= usd_tolerance,
            "USD mismatch for {}: left={} right={} diff={} tol={}",
            case.request_id,
            gas_in_sell_usd_micros,
            gas_price_cost_usd_micros,
            usd_diff,
            usd_tolerance
        );

        let expected_base_units = expected_gas_in_sell_base_units(
            last_gas_used,
            case.sell_decimals,
            case.eth_to_sell_spot,
        );
        assert_eq!(
            gas_in_sell, expected_base_units,
            "base-unit mismatch for {}",
            case.request_id
        );
    }
}
