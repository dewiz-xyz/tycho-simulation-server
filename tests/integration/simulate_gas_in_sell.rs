use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
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
    ) -> Result<(), TransitionError> {
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

#[expect(
    clippy::expect_used,
    reason = "Integration fixtures only pass hard-coded valid pool addresses."
)]
fn make_component(
    pool_address: &str,
    token_a: Token,
    token_b: Token,
    protocol_system: &str,
    protocol_type_name: &str,
) -> ProtocolComponent {
    ProtocolComponent::new(
        parse_bytes(pool_address).expect("valid pool address"),
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

fn parse_bytes(address: &str) -> Result<Bytes> {
    Ok(Bytes::from_str(address)?)
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

struct GasFixtureTokens {
    usdt: Bytes,
    usdc: Bytes,
    dai: Bytes,
    wbtc: Bytes,
    weth: Bytes,
    usdt_token: Token,
    usdc_token: Token,
    dai_token: Token,
    wbtc_token: Token,
    weth_token: Token,
}

fn build_gas_fixture_tokens() -> Result<GasFixtureTokens> {
    let usdt = parse_bytes("0xdAC17F958D2ee523a2206206994597C13D831ec7")?;
    let usdc = parse_bytes("0xA0b86991c6218b36c1d19d4a2e9Eb0cE3606eB48")?;
    let dai = parse_bytes("0x6B175474E89094C44Da98b954EedeAC495271d0F")?;
    let wbtc = parse_bytes("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")?;
    let weth = Chain::Ethereum.wrapped_native_token().address;

    Ok(GasFixtureTokens {
        usdt_token: make_token(&usdt, "USDT", 6),
        usdc_token: make_token(&usdc, "USDC", 6),
        dai_token: make_token(&dai, "DAI", 18),
        wbtc_token: make_token(&wbtc, "WBTC", 8),
        weth_token: make_token(&weth, "WETH", 18),
        usdt,
        usdc,
        dai,
        wbtc,
        weth,
    })
}

fn build_token_store(tokens: &GasFixtureTokens) -> Arc<TokenStore> {
    let initial_tokens = HashMap::from([
        (tokens.usdt.clone(), tokens.usdt_token.clone()),
        (tokens.usdc.clone(), tokens.usdc_token.clone()),
        (tokens.dai.clone(), tokens.dai_token.clone()),
        (tokens.wbtc.clone(), tokens.wbtc_token.clone()),
        (tokens.weth.clone(), tokens.weth_token.clone()),
    ]);
    Arc::new(TokenStore::new(
        initial_tokens,
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_secs(1),
    ))
}

fn insert_quote_pool(
    states: &mut HashMap<String, Box<dyn ProtocolSim>>,
    new_pairs: &mut HashMap<String, ProtocolComponent>,
    pool_id: &str,
    pool_address: &str,
    token_a: Token,
    token_b: Token,
    gas_ladder: Vec<u64>,
) {
    states.insert(
        pool_id.to_string(),
        Box::new(SimPool {
            gas_ladder,
            spot_quotes: Vec::new(),
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        pool_id.to_string(),
        make_component(pool_address, token_a, token_b, "uniswap_v2", "uniswap_v2"),
    );
}

fn insert_spot_pool(
    states: &mut HashMap<String, Box<dyn ProtocolSim>>,
    new_pairs: &mut HashMap<String, ProtocolComponent>,
    pool_id: &str,
    pool_address: &str,
    sell_token: &Token,
    spot_price: f64,
    quote_token: &Token,
) {
    states.insert(
        pool_id.to_string(),
        Box::new(SimPool {
            gas_ladder: vec![0],
            spot_quotes: vec![(
                quote_token.address.clone(),
                sell_token.address.clone(),
                spot_price,
            )],
            calls: default_calls(),
        }) as Box<dyn ProtocolSim>,
    );
    new_pairs.insert(
        pool_id.to_string(),
        make_component(
            pool_address,
            sell_token.clone(),
            quote_token.clone(),
            "uniswap_v2",
            "uniswap_v2",
        ),
    );
}

fn build_multi_pair_updates(
    tokens: &GasFixtureTokens,
) -> (
    HashMap<String, Box<dyn ProtocolSim>>,
    HashMap<String, ProtocolComponent>,
) {
    let mut states: HashMap<String, Box<dyn ProtocolSim>> = HashMap::new();
    let mut new_pairs = HashMap::new();

    insert_quote_pool(
        &mut states,
        &mut new_pairs,
        "pool-usdt-usdc",
        "0x0000000000000000000000000000000000000101",
        tokens.usdt_token.clone(),
        tokens.usdc_token.clone(),
        vec![120_000, 150_000],
    );
    insert_quote_pool(
        &mut states,
        &mut new_pairs,
        "pool-usdc-dai",
        "0x0000000000000000000000000000000000000102",
        tokens.usdc_token.clone(),
        tokens.dai_token.clone(),
        vec![180_000, 210_000],
    );
    insert_quote_pool(
        &mut states,
        &mut new_pairs,
        "pool-dai-usdt",
        "0x0000000000000000000000000000000000000103",
        tokens.dai_token.clone(),
        tokens.usdt_token.clone(),
        vec![160_000, 180_000],
    );
    insert_quote_pool(
        &mut states,
        &mut new_pairs,
        "pool-wbtc-usdc",
        "0x0000000000000000000000000000000000000104",
        tokens.wbtc_token.clone(),
        tokens.usdc_token.clone(),
        vec![170_000, 190_000],
    );

    insert_spot_pool(
        &mut states,
        &mut new_pairs,
        "pool-spot-usdt-weth",
        "0x0000000000000000000000000000000000000201",
        &tokens.usdt_token,
        ETH_USD_PRICE as f64,
        &tokens.weth_token,
    );
    insert_spot_pool(
        &mut states,
        &mut new_pairs,
        "pool-spot-usdc-weth",
        "0x0000000000000000000000000000000000000202",
        &tokens.usdc_token,
        ETH_USD_PRICE as f64,
        &tokens.weth_token,
    );
    insert_spot_pool(
        &mut states,
        &mut new_pairs,
        "pool-spot-dai-weth",
        "0x0000000000000000000000000000000000000203",
        &tokens.dai_token,
        ETH_USD_PRICE as f64,
        &tokens.weth_token,
    );
    insert_spot_pool(
        &mut states,
        &mut new_pairs,
        "pool-spot-wbtc-weth",
        "0x0000000000000000000000000000000000000204",
        &tokens.wbtc_token,
        0.03125,
        &tokens.weth_token,
    );

    (states, new_pairs)
}

fn build_quote_app(
    token_store: Arc<TokenStore>,
    native_state_store: Arc<StateStore>,
    vm_state_store: Arc<StateStore>,
    native_sim_concurrency: usize,
    reporting_enabled: bool,
) -> axum::Router {
    create_router(AppState {
        chain: Chain::Ethereum,
        native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
        tokens: token_store,
        native_state_store,
        vm_state_store,
        native_stream_health: Arc::new(StreamHealth::new()),
        vm_stream_health: Arc::new(StreamHealth::new()),
        vm_stream: Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
        latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(Some(GAS_PRICE_WEI))),
        native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(reporting_enabled)),
        enable_vm_pools: false,
        readiness_stale: Duration::from_secs(120),
        quote_timeout: Duration::from_secs(1),
        pool_timeout_native: Duration::from_millis(200),
        pool_timeout_vm: Duration::from_millis(200),
        request_timeout: Duration::from_secs(2),
        native_sim_semaphore: Arc::new(Semaphore::new(native_sim_concurrency)),
        vm_sim_semaphore: Arc::new(Semaphore::new(1)),
        erc4626_deposits_enabled: false,
        reset_allowance_tokens: Arc::new(HashMap::<u64, HashSet<Bytes>>::new()),
        native_sim_concurrency,
        vm_sim_concurrency: 1,
    })
}

fn multi_pair_cases() -> Vec<PairCase> {
    vec![
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
    ]
}
fn gas_cost_usd_micros(gas_used: u64) -> u128 {
    (u128::from(gas_used) * GAS_PRICE_WEI * ETH_USD_PRICE * USD_MICROS) / WEI_PER_ETH
}

fn expected_gas_in_sell_base_units(
    gas_used: u64,
    sell_decimals: u32,
    eth_to_sell_spot: f64,
) -> Result<u128> {
    let spot_scaled = (eth_to_sell_spot * SPOT_PRICE_SCALE as f64).floor() as u128;
    let numerator = BigUint::from(gas_used)
        * BigUint::from(GAS_PRICE_WEI)
        * BigUint::from(spot_scaled)
        * BigUint::from(10u32).pow(sell_decimals);
    let denominator = BigUint::from(SPOT_PRICE_SCALE) * BigUint::from(WEI_PER_ETH);
    (numerator / denominator)
        .to_u128()
        .ok_or_else(|| anyhow!("expected gas_in_sell to fit into u128"))
}

async fn post_simulate(app: axum::Router, request: &AmountOutRequest) -> Result<serde_json::Value> {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/simulate")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(request)?))?,
        )
        .await?;

    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );

    Ok(serde_json::from_slice(&body)?)
}

fn find_pool_entry<'a>(
    value: &'a serde_json::Value,
    pool_id: &str,
) -> Result<&'a serde_json::Value> {
    value["data"]
        .as_array()
        .ok_or_else(|| anyhow!("data must be array"))?
        .iter()
        .find(|item| item["pool"] == pool_id)
        .ok_or_else(|| anyhow!("pool {pool_id} not found in simulate response"))
}

fn last_gas_used(entry: &serde_json::Value) -> Result<u64> {
    entry["gas_used"]
        .as_array()
        .ok_or_else(|| anyhow!("gas_used must be array"))?
        .last()
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| anyhow!("last gas_used must be u64"))
}

fn parse_gas_in_sell(entry: &serde_json::Value) -> Result<u128> {
    entry["gas_in_sell"]
        .as_str()
        .ok_or_else(|| anyhow!("gas_in_sell must be string"))?
        .parse::<u128>()
        .map_err(Into::into)
}

#[tokio::test]
async fn simulate_gas_in_sell_matches_gas_price_cost_in_usd_for_multiple_pairs() -> Result<()> {
    let tokens = build_gas_fixture_tokens()?;
    let token_store = build_token_store(&tokens);
    let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
    let (states, new_pairs) = build_multi_pair_updates(&tokens);

    native_state_store
        .apply_update(Update::new(42, states, new_pairs))
        .await;
    let app = build_quote_app(
        Arc::clone(&token_store),
        Arc::clone(&native_state_store),
        Arc::clone(&vm_state_store),
        8,
        true,
    );

    for case in multi_pair_cases() {
        let request = AmountOutRequest {
            request_id: case.request_id.to_string(),
            auction_id: None,
            token_in: case.token_in.to_string(),
            token_out: case.token_out.to_string(),
            amounts: vec!["1000000".to_string(), "2000000".to_string()],
        };

        let value = post_simulate(app.clone(), &request).await?;
        assert_eq!(value["meta"]["status"], "ready");

        let entry = find_pool_entry(&value, case.pool_id)?;
        let last_gas_used = last_gas_used(entry)?;
        assert_eq!(last_gas_used, case.expected_last_gas);

        let gas_in_sell = parse_gas_in_sell(entry)?;

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
        )?;
        assert_eq!(
            gas_in_sell, expected_base_units,
            "base-unit mismatch for {}",
            case.request_id
        );
    }
    Ok(())
}

#[tokio::test]
async fn simulate_gas_in_sell_is_zero_when_reporting_disabled() -> Result<()> {
    let usdt = parse_bytes("0xdAC17F958D2ee523a2206206994597C13D831ec7")?;
    let usdc = parse_bytes("0xA0b86991c6218b36c1d19d4a2e9Eb0cE3606eB48")?;
    let weth = Chain::Ethereum.wrapped_native_token().address;

    let usdt_token = make_token(&usdt, "USDT", 6);
    let usdc_token = make_token(&usdc, "USDC", 6);
    let weth_token = make_token(&weth, "WETH", 18);

    let mut tokens = HashMap::new();
    tokens.insert(usdt.clone(), usdt_token.clone());
    tokens.insert(usdc.clone(), usdc_token.clone());
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
            "0x0000000000000000000000000000000000000301",
            usdt_token.clone(),
            usdc_token.clone(),
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

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
            "0x0000000000000000000000000000000000000302",
            usdt_token.clone(),
            weth_token.clone(),
            "uniswap_v2",
            "uniswap_v2",
        ),
    );

    native_state_store
        .apply_update(Update::new(42, states, new_pairs))
        .await;

    let app_state = AppState {
        chain: Chain::Ethereum,
        native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
        tokens: Arc::clone(&token_store),
        native_state_store: Arc::clone(&native_state_store),
        vm_state_store: Arc::clone(&vm_state_store),
        native_stream_health: Arc::new(StreamHealth::new()),
        vm_stream_health: Arc::new(StreamHealth::new()),
        vm_stream: Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
        latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(Some(GAS_PRICE_WEI))),
        native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
        enable_vm_pools: false,
        readiness_stale: Duration::from_secs(120),
        quote_timeout: Duration::from_secs(1),
        pool_timeout_native: Duration::from_millis(200),
        pool_timeout_vm: Duration::from_millis(200),
        request_timeout: Duration::from_secs(2),
        native_sim_semaphore: Arc::new(Semaphore::new(2)),
        vm_sim_semaphore: Arc::new(Semaphore::new(1)),
        erc4626_deposits_enabled: false,
        reset_allowance_tokens: Arc::new(HashMap::<u64, HashSet<Bytes>>::new()),
        native_sim_concurrency: 2,
        vm_sim_concurrency: 1,
    };

    let app = create_router(app_state);
    let request = AmountOutRequest {
        request_id: "reporting-disabled".to_string(),
        auction_id: None,
        token_in: "0xdAC17F958D2ee523a2206206994597C13D831ec7".to_string(),
        token_out: "0xA0b86991c6218b36c1d19d4a2e9Eb0cE3606eB48".to_string(),
        amounts: vec!["1000000".to_string(), "2000000".to_string()],
    };

    let value = post_simulate(app, &request).await?;
    let entry = find_pool_entry(&value, "pool-usdt-usdc")?;
    assert_eq!(entry["gas_in_sell"], "0");
    Ok(())
}
