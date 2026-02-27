use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::NaiveDateTime;
use num_bigint::BigUint;
use num_traits::Zero;
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
use tycho_simulation_server::models::messages::{
    EncodeErrorResponse, HopDraft, InteractionKind, PoolRef, PoolSwapDraft, RouteEncodeRequest,
    RouteEncodeResponse, SegmentDraft, SwapKind,
};
use tycho_simulation_server::models::state::{AppState, StateStore, VmStreamStatus};
use tycho_simulation_server::models::stream_health::StreamHealth;
use tycho_simulation_server::models::tokens::TokenStore;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct EchoAmountSim;

#[typetag::serde]
impl ProtocolSim for EchoAmountSim {
    fn fee(&self) -> f64 {
        0.0
    }

    fn spot_price(&self, _base: &Token, _quote: &Token) -> Result<f64, SimulationError> {
        Ok(0.0)
    }

    fn get_amount_out(
        &self,
        amount_in: BigUint,
        _token_in: &Token,
        _token_out: &Token,
    ) -> Result<GetAmountOutResult, SimulationError> {
        Ok(GetAmountOutResult::new(
            amount_in,
            BigUint::zero(),
            self.clone_box(),
        ))
    }

    fn get_limits(
        &self,
        _sell_token: Bytes,
        _buy_token: Bytes,
    ) -> Result<(BigUint, BigUint), SimulationError> {
        Ok((BigUint::zero(), BigUint::zero()))
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
        other.as_any().is::<EchoAmountSim>()
    }
}

fn make_token(address: &Bytes, symbol: &str) -> Token {
    Token::new(address, symbol, 18, 0, &[], Chain::Ethereum, 100)
}

fn parse_bytes(value: &str) -> Result<Bytes> {
    Ok(Bytes::from_str(value)?)
}

fn hex_to_bytes(value: &str) -> Result<Vec<u8>> {
    let stripped = value.strip_prefix("0x").unwrap_or(value);
    Ok(alloy_primitives::hex::decode(stripped)?)
}

fn decode_transfer_from_allowed(calldata: &[u8]) -> Result<bool> {
    const SELECTOR_BYTES: usize = 4;
    const ABI_WORD_BYTES: usize = 32;
    const TRANSFER_FROM_ALLOWED_INDEX: usize = 7;

    let value_offset =
        SELECTOR_BYTES + (TRANSFER_FROM_ALLOWED_INDEX * ABI_WORD_BYTES) + (ABI_WORD_BYTES - 1);
    Ok(calldata
        .get(value_offset)
        .copied()
        .ok_or_else(|| anyhow!("single-swap calldata should include transferFrom flag"))?
        == 1)
}

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct ScopedEnvVar {
    key: &'static str,
    previous: Option<OsString>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl ScopedEnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        let guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self {
            key,
            previous,
            _guard: guard,
        }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "integration fixture keeps test knobs explicit and local to this file"
)]
struct EncodeFixtureConfig<'a> {
    min_amount_out: &'a str,
    token_in_hex: &'a str,
    token_out_hex: &'a str,
    component_address_hex: &'a str,
    request_pool_protocol: &'a str,
    component_protocol_system: &'a str,
    component_protocol_type_name: &'a str,
    component_static_attributes: HashMap<String, Bytes>,
    vm_pool: bool,
    enable_vm_pools: bool,
    erc4626_deposits_enabled: bool,
    reset_allowance: bool,
    request_id: &'a str,
}

impl Default for EncodeFixtureConfig<'_> {
    fn default() -> Self {
        Self {
            min_amount_out: "8",
            token_in_hex: "0x0000000000000000000000000000000000000001",
            token_out_hex: "0x0000000000000000000000000000000000000002",
            component_address_hex: "0x0000000000000000000000000000000000000009",
            request_pool_protocol: "uniswap_v2",
            component_protocol_system: "uniswap_v2",
            component_protocol_type_name: "uniswap_v2",
            component_static_attributes: HashMap::new(),
            vm_pool: false,
            enable_vm_pools: false,
            erc4626_deposits_enabled: false,
            reset_allowance: true,
            request_id: "req-1",
        }
    }
}

struct FixtureTokens {
    token_in: Bytes,
    token_in_meta: Token,
    token_out_meta: Token,
    store: Arc<TokenStore>,
}

fn build_fixture_tokens(config: &EncodeFixtureConfig<'_>) -> Result<FixtureTokens> {
    let token_in = parse_bytes(config.token_in_hex)?;
    let token_out = parse_bytes(config.token_out_hex)?;
    let token_in_symbol = if token_in == Chain::Ethereum.native_token().address {
        "ETH"
    } else {
        "TK1"
    };
    let token_in_meta = make_token(&token_in, token_in_symbol);
    let token_out_meta = make_token(&token_out, "TK2");

    let mut initial_tokens = HashMap::new();
    initial_tokens.insert(token_in.clone(), token_in_meta.clone());
    initial_tokens.insert(token_out.clone(), token_out_meta.clone());

    Ok(FixtureTokens {
        token_in,
        token_in_meta,
        token_out_meta,
        store: Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_millis(10),
        )),
    })
}

async fn build_fixture_stores(
    config: &EncodeFixtureConfig<'_>,
    fixture_tokens: &FixtureTokens,
    pool_id: &str,
) -> Result<(Arc<StateStore>, Arc<StateStore>)> {
    let native_state_store = Arc::new(StateStore::new(Arc::clone(&fixture_tokens.store)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(&fixture_tokens.store)));
    let component = ProtocolComponent::new(
        parse_bytes(config.component_address_hex)?,
        config.component_protocol_system.to_string(),
        config.component_protocol_type_name.to_string(),
        Chain::Ethereum,
        vec![
            fixture_tokens.token_in_meta.clone(),
            fixture_tokens.token_out_meta.clone(),
        ],
        Vec::new(),
        config.component_static_attributes.clone(),
        Bytes::default(),
        NaiveDateTime::default(),
    );

    let states = HashMap::from([(
        pool_id.to_string(),
        Box::new(EchoAmountSim) as Box<dyn ProtocolSim>,
    )]);
    let new_pairs = HashMap::from([(pool_id.to_string(), component)]);
    let update = Update::new(42, states, new_pairs);
    if config.vm_pool {
        vm_state_store.apply_update(update).await;
    } else {
        native_state_store.apply_update(update).await;
    }
    Ok((native_state_store, vm_state_store))
}

fn build_route_encode_request(
    config: &EncodeFixtureConfig<'_>,
    pool_id: String,
    settlement: String,
    router: String,
) -> RouteEncodeRequest {
    RouteEncodeRequest {
        chain_id: 1,
        token_in: config.token_in_hex.to_string(),
        token_out: config.token_out_hex.to_string(),
        amount_in: "10".to_string(),
        min_amount_out: config.min_amount_out.to_string(),
        settlement_address: settlement,
        tycho_router_address: router,
        swap_kind: SwapKind::SimpleSwap,
        segments: vec![SegmentDraft {
            kind: SwapKind::SimpleSwap,
            // A single segment must have `share_bps=0` to take the remainder.
            share_bps: 0,
            hops: vec![HopDraft {
                token_in: config.token_in_hex.to_string(),
                token_out: config.token_out_hex.to_string(),
                swaps: vec![PoolSwapDraft {
                    pool: PoolRef {
                        protocol: config.request_pool_protocol.to_string(),
                        component_id: pool_id,
                        pool_address: None,
                    },
                    token_in: config.token_in_hex.to_string(),
                    token_out: config.token_out_hex.to_string(),
                    // A single swap must have `split_bps=0` to take the remainder.
                    split_bps: 0,
                }],
            }],
        }],
        request_id: Some(config.request_id.to_string()),
    }
}

async fn setup_app_state_and_request(
    config: EncodeFixtureConfig<'_>,
) -> Result<(axum::Router, RouteEncodeRequest)> {
    let fixture_tokens = build_fixture_tokens(&config)?;
    let settlement = "0x0000000000000000000000000000000000000003".to_string();
    let router = "0x0000000000000000000000000000000000000004".to_string();
    let pool_id = "pool-1".to_string();
    let (native_state_store, vm_state_store) =
        build_fixture_stores(&config, &fixture_tokens, &pool_id).await?;

    let mut reset_allowance_tokens: HashMap<u64, HashSet<Bytes>> = HashMap::new();
    if config.reset_allowance {
        reset_allowance_tokens
            .entry(1)
            .or_default()
            .insert(fixture_tokens.token_in.clone());
    }

    let state = AppState {
        chain: Chain::Ethereum,
        native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
        tokens: Arc::clone(&fixture_tokens.store),
        native_state_store: Arc::clone(&native_state_store),
        vm_state_store: Arc::clone(&vm_state_store),
        native_stream_health: Arc::new(StreamHealth::new()),
        vm_stream_health: Arc::new(StreamHealth::new()),
        vm_stream: Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
        latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
        native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
        enable_vm_pools: config.enable_vm_pools,
        readiness_stale: Duration::from_secs(120),
        quote_timeout: Duration::from_secs(1),
        pool_timeout_native: Duration::from_secs(1),
        pool_timeout_vm: Duration::from_secs(1),
        request_timeout: Duration::from_secs(2),
        native_sim_semaphore: Arc::new(Semaphore::new(4)),
        vm_sim_semaphore: Arc::new(Semaphore::new(1)),
        erc4626_deposits_enabled: config.erc4626_deposits_enabled,
        reset_allowance_tokens: Arc::new(reset_allowance_tokens),
        native_sim_concurrency: 4,
        vm_sim_concurrency: 1,
    };

    let app = create_router(state);
    Ok((
        app,
        build_route_encode_request(&config, pool_id, settlement, router),
    ))
}

async fn post_encode(
    app: axum::Router,
    request: &RouteEncodeRequest,
) -> Result<(StatusCode, axum::body::Bytes)> {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/encode")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(request)?))?,
        )
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, body))
}

#[tokio::test]
async fn encode_route_end_to_end_returns_interactions_and_debug() -> Result<()> {
    let (app, request) = setup_app_state_and_request(EncodeFixtureConfig::default()).await?;
    let (status, body) = post_encode(app, &request).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );
    let response: RouteEncodeResponse = serde_json::from_slice(&body)?;

    assert_eq!(response.interactions.len(), 3, "reset-then-approve path");
    assert_eq!(response.interactions[0].kind, InteractionKind::Erc20Approve);
    assert_eq!(response.interactions[1].kind, InteractionKind::Erc20Approve);
    assert_eq!(response.interactions[2].kind, InteractionKind::Call);

    // ERC20 approve selector for `approve(address,uint256)`.
    let approve_prefix = "0x095ea7b3";
    assert_eq!(
        response.interactions[0].target,
        "0x0000000000000000000000000000000000000001"
    );
    assert_eq!(
        response.interactions[1].target,
        "0x0000000000000000000000000000000000000001"
    );
    assert_eq!(
        response.interactions[2].target,
        "0x0000000000000000000000000000000000000004"
    );
    assert!(
        response.interactions[0]
            .calldata
            .starts_with(approve_prefix),
        "unexpected approve calldata: {}",
        response.interactions[0].calldata
    );
    assert!(
        response.interactions[1]
            .calldata
            .starts_with(approve_prefix),
        "unexpected approve calldata: {}",
        response.interactions[1].calldata
    );
    assert!(
        response.interactions[2].calldata.starts_with("0x"),
        "router calldata must be hex with 0x prefix"
    );
    assert!(
        !response.interactions[2]
            .calldata
            .starts_with(approve_prefix),
        "router calldata should not be an ERC20 approve"
    );

    // Assert reset allowance uses a 0 approval, followed by the full amount_in (10).
    let reset_calldata = hex_to_bytes(&response.interactions[0].calldata)?;
    let approve_calldata = hex_to_bytes(&response.interactions[1].calldata)?;
    assert_eq!(reset_calldata.len(), 4 + 32 + 32);
    assert_eq!(approve_calldata.len(), 4 + 32 + 32);

    let router_bytes = hex_to_bytes("0x0000000000000000000000000000000000000004")?;
    assert_eq!(router_bytes.len(), 20);

    // ABI encoded addresses are left-padded to 32 bytes.
    let reset_spender = &reset_calldata[4 + 12..4 + 32];
    let approve_spender = &approve_calldata[4 + 12..4 + 32];
    assert_eq!(reset_spender, router_bytes.as_slice());
    assert_eq!(approve_spender, router_bytes.as_slice());

    let reset_amount = &reset_calldata[4 + 32..];
    let approve_amount = &approve_calldata[4 + 32..];
    assert!(reset_amount.iter().all(|byte| *byte == 0));
    assert!(approve_amount[..31].iter().all(|byte| *byte == 0));
    assert_eq!(approve_amount[31], 10);

    let debug = response
        .debug
        .ok_or_else(|| anyhow!("debug should be present with requestId"))?;
    assert_eq!(debug.request_id.as_deref(), Some("req-1"));
    assert_eq!(
        debug
            .resimulation
            .as_ref()
            .and_then(|resim| resim.block_number),
        Some(42)
    );
    Ok(())
}

#[tokio::test]
async fn encode_route_rejects_when_min_amount_out_exceeds_expected() -> Result<()> {
    let config = EncodeFixtureConfig {
        min_amount_out: "11",
        ..EncodeFixtureConfig::default()
    };
    let (app, request) = setup_app_state_and_request(config).await?;
    let (status, body) = post_encode(app, &request).await?;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );
    let response: EncodeErrorResponse = serde_json::from_slice(&body)?;
    assert!(
        response.error.contains("below minAmountOut"),
        "unexpected error: {}",
        response.error
    );
    assert_eq!(response.request_id.as_deref(), Some("req-1"));
    Ok(())
}

#[tokio::test]
async fn encode_route_succeeds_for_vm_maverick_v2_pool() -> Result<()> {
    let config = EncodeFixtureConfig {
        request_pool_protocol: "vm:maverick_v2",
        component_protocol_system: "vm:maverick_v2",
        component_protocol_type_name: "maverick_v2",
        vm_pool: true,
        enable_vm_pools: true,
        reset_allowance: false,
        ..EncodeFixtureConfig::default()
    };
    let (app, request) = setup_app_state_and_request(config).await?;
    let (status, body) = post_encode(app, &request).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );
    let response: RouteEncodeResponse = serde_json::from_slice(&body)?;
    assert_eq!(response.interactions.len(), 2);
    assert_eq!(response.interactions[0].kind, InteractionKind::Erc20Approve);
    assert_eq!(response.interactions[1].kind, InteractionKind::Call);
    Ok(())
}

#[tokio::test]
async fn encode_route_succeeds_for_ekubo_v3_pool() -> Result<()> {
    let config = EncodeFixtureConfig {
        request_pool_protocol: "ekubo_v3",
        component_protocol_system: "ekubo_v3",
        component_protocol_type_name: "ekubo_v3",
        component_static_attributes: HashMap::from([
            (
                "extension".to_string(),
                parse_bytes("0x517e506700271aea091b02f42756f5e174af5230")?,
            ),
            ("fee".to_string(), Bytes::from(0_u64)),
            ("pool_type_config".to_string(), Bytes::from(0_u32)),
        ]),
        reset_allowance: false,
        ..EncodeFixtureConfig::default()
    };
    let (app, request) = setup_app_state_and_request(config).await?;
    let (status, body) = post_encode(app, &request).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );
    let response: RouteEncodeResponse = serde_json::from_slice(&body)?;
    assert_eq!(response.interactions.len(), 2);
    assert_eq!(response.interactions[0].kind, InteractionKind::Erc20Approve);
    assert_eq!(response.interactions[1].kind, InteractionKind::Call);
    Ok(())
}

#[tokio::test]
async fn encode_route_succeeds_for_rocketpool_native_input() -> Result<()> {
    let config = EncodeFixtureConfig {
        token_in_hex: "0x0000000000000000000000000000000000000000",
        token_out_hex: "0xae78736cd615f374d3085123a210448e74fc6393",
        request_pool_protocol: "rocketpool",
        component_protocol_system: "rocketpool",
        component_protocol_type_name: "rocketpool",
        reset_allowance: false,
        ..EncodeFixtureConfig::default()
    };
    let (app, request) = setup_app_state_and_request(config).await?;
    let (status, body) = post_encode(app, &request).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );
    let response: RouteEncodeResponse = serde_json::from_slice(&body)?;
    assert_eq!(response.interactions.len(), 1);
    assert_eq!(response.interactions[0].kind, InteractionKind::Call);
    assert_eq!(response.interactions[0].value, "10");
    let router_calldata = hex_to_bytes(&response.interactions[0].calldata)?;
    assert!(
        !decode_transfer_from_allowed(&router_calldata)?,
        "native-input router calldata must disable transferFrom"
    );
    Ok(())
}

#[tokio::test]
async fn encode_route_succeeds_for_rocketpool_native_output() -> Result<()> {
    let config = EncodeFixtureConfig {
        token_in_hex: "0xae78736cd615f374d3085123a210448e74fc6393",
        token_out_hex: "0x0000000000000000000000000000000000000000",
        request_pool_protocol: "rocketpool",
        component_protocol_system: "rocketpool",
        component_protocol_type_name: "rocketpool",
        reset_allowance: false,
        ..EncodeFixtureConfig::default()
    };
    let (app, request) = setup_app_state_and_request(config).await?;
    let (status, body) = post_encode(app, &request).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );
    let response: RouteEncodeResponse = serde_json::from_slice(&body)?;
    assert_eq!(response.interactions.len(), 2);
    assert_eq!(response.interactions[0].kind, InteractionKind::Erc20Approve);
    assert_eq!(response.interactions[1].kind, InteractionKind::Call);
    assert_eq!(response.interactions[1].value, "0");
    Ok(())
}

#[tokio::test]
async fn encode_route_rejects_native_input_for_non_allowlisted_protocol() -> Result<()> {
    let config = EncodeFixtureConfig {
        token_in_hex: "0x0000000000000000000000000000000000000000",
        token_out_hex: "0x0000000000000000000000000000000000000002",
        request_pool_protocol: "uniswap_v2",
        component_protocol_system: "uniswap_v2",
        component_protocol_type_name: "uniswap_v2",
        reset_allowance: false,
        ..EncodeFixtureConfig::default()
    };
    let (app, request) = setup_app_state_and_request(config).await?;
    let (status, body) = post_encode(app, &request).await?;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );
    let response: EncodeErrorResponse = serde_json::from_slice(&body)?;
    assert!(
        response
            .error
            .contains("only supported for protocols [rocketpool]"),
        "unexpected error: {}",
        response.error
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn encode_route_rejects_allowlisted_erc4626_deposit_when_deposits_disabled() -> Result<()> {
    let config = EncodeFixtureConfig {
        token_in_hex: "0xdC035D45d973E3EC169d2276DDab16f1e407384F",
        token_out_hex: "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
        component_address_hex: "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
        request_pool_protocol: "erc4626",
        component_protocol_system: "erc4626",
        component_protocol_type_name: "erc4626_pool",
        reset_allowance: false,
        request_id: "req-erc4626-deposit",
        ..EncodeFixtureConfig::default()
    };
    let (app, request) = setup_app_state_and_request(config).await?;
    let (status, body) = post_encode(app, &request).await?;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );
    let response: EncodeErrorResponse = serde_json::from_slice(&body)?;
    assert!(
        response.error.contains("USDS -> sUSDS"),
        "unexpected error: {}",
        response.error
    );
    assert!(
        response.error.contains("sUSDS -> USDS"),
        "unexpected error: {}",
        response.error
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn encode_route_succeeds_for_allowlisted_erc4626_redeem() -> Result<()> {
    // The real encoder touches its ERC4626 provider path for redeems, so keep the test
    // self-contained instead of depending on a developer-local `.env`.
    let _rpc_url = ScopedEnvVar::set("RPC_URL", "http://localhost:8545");
    let config = EncodeFixtureConfig {
        token_in_hex: "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
        token_out_hex: "0xdC035D45d973E3EC169d2276DDab16f1e407384F",
        component_address_hex: "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
        request_pool_protocol: "erc4626",
        component_protocol_system: "erc4626",
        component_protocol_type_name: "erc4626_pool",
        reset_allowance: false,
        request_id: "req-erc4626-redeem",
        ..EncodeFixtureConfig::default()
    };
    let (app, request) = setup_app_state_and_request(config).await?;
    let (status, body) = post_encode(app, &request).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );
    let response: RouteEncodeResponse = serde_json::from_slice(&body)?;
    assert_eq!(response.interactions.len(), 2);
    assert_eq!(response.interactions[0].kind, InteractionKind::Erc20Approve);
    assert_eq!(response.interactions[1].kind, InteractionKind::Call);
    Ok(())
}

#[tokio::test]
async fn encode_route_rejects_unsupported_erc4626_direction() -> Result<()> {
    let config = EncodeFixtureConfig {
        token_in_hex: "0x9d39a5de30e57443bff2a8307a4256c8797a3497",
        token_out_hex: "0x4c9EDD5852cd905f086C759E8383e09bff1E68B3",
        component_address_hex: "0x9d39a5de30e57443bff2a8307a4256c8797a3497",
        request_pool_protocol: "erc4626",
        component_protocol_system: "erc4626",
        component_protocol_type_name: "erc4626_pool",
        reset_allowance: false,
        request_id: "req-erc4626-reject",
        ..EncodeFixtureConfig::default()
    };
    let (app, request) = setup_app_state_and_request(config).await?;
    let (status, body) = post_encode(app, &request).await?;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );
    let response: EncodeErrorResponse = serde_json::from_slice(&body)?;
    assert!(
        response.error.contains("ERC4626 direction"),
        "unexpected error: {}",
        response.error
    );
    assert!(
        response.error.contains("not currently supported"),
        "unexpected error: {}",
        response.error
    );
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "mixed-route rejection test keeps the full request setup inline for clarity"
)]
async fn encode_route_rejects_mixed_route_with_unsupported_erc4626_hop() -> Result<()> {
    let token_a = "0x0000000000000000000000000000000000000011";
    let token_b = "0x4c9EDD5852cd905f086C759E8383e09bff1E68B3";
    let token_d = "0x9d39a5de30e57443bff2a8307a4256c8797a3497";
    let token_store = Arc::new(TokenStore::new(
        HashMap::from([
            (
                parse_bytes(token_a)?,
                Token::new(
                    &parse_bytes(token_a)?,
                    "TKA",
                    18,
                    0,
                    &[],
                    Chain::Ethereum,
                    100,
                ),
            ),
            (
                parse_bytes(token_b)?,
                Token::new(
                    &parse_bytes(token_b)?,
                    "TKB",
                    18,
                    0,
                    &[],
                    Chain::Ethereum,
                    100,
                ),
            ),
        ]),
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_millis(10),
    ));
    let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
    let state = AppState {
        chain: Chain::Ethereum,
        native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
        tokens: token_store,
        native_state_store,
        vm_state_store,
        native_stream_health: Arc::new(StreamHealth::new()),
        vm_stream_health: Arc::new(StreamHealth::new()),
        vm_stream: Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
        latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
        native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
        enable_vm_pools: false,
        readiness_stale: Duration::from_secs(120),
        quote_timeout: Duration::from_secs(1),
        pool_timeout_native: Duration::from_secs(1),
        pool_timeout_vm: Duration::from_secs(1),
        request_timeout: Duration::from_secs(2),
        native_sim_semaphore: Arc::new(Semaphore::new(4)),
        vm_sim_semaphore: Arc::new(Semaphore::new(1)),
        erc4626_deposits_enabled: false,
        reset_allowance_tokens: Arc::new(HashMap::new()),
        native_sim_concurrency: 4,
        vm_sim_concurrency: 1,
    };
    let app = create_router(state);
    let request = RouteEncodeRequest {
        chain_id: 1,
        token_in: token_a.to_string(),
        token_out: token_d.to_string(),
        amount_in: "10".to_string(),
        min_amount_out: "8".to_string(),
        settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
        tycho_router_address: "0x0000000000000000000000000000000000000004".to_string(),
        swap_kind: SwapKind::MultiSwap,
        segments: vec![SegmentDraft {
            kind: SwapKind::MultiSwap,
            share_bps: 0,
            hops: vec![
                HopDraft {
                    token_in: token_a.to_string(),
                    token_out: token_b.to_string(),
                    swaps: vec![PoolSwapDraft {
                        pool: PoolRef {
                            protocol: "uniswap_v2".to_string(),
                            component_id: "pool-uniswap".to_string(),
                            pool_address: None,
                        },
                        token_in: token_a.to_string(),
                        token_out: token_b.to_string(),
                        split_bps: 0,
                    }],
                },
                HopDraft {
                    token_in: token_b.to_string(),
                    token_out: token_d.to_string(),
                    swaps: vec![PoolSwapDraft {
                        pool: PoolRef {
                            protocol: "erc4626".to_string(),
                            component_id: "pool-susde".to_string(),
                            pool_address: None,
                        },
                        token_in: token_b.to_string(),
                        token_out: token_d.to_string(),
                        split_bps: 0,
                    }],
                },
            ],
        }],
        request_id: Some("req-erc4626-mixed".to_string()),
    };

    let (status, body) = post_encode(app, &request).await?;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unexpected status {}: {}",
        status,
        String::from_utf8_lossy(&body)
    );
    let response: EncodeErrorResponse = serde_json::from_slice(&body)?;
    assert!(
        response.error.contains("ERC4626 direction"),
        "unexpected error: {}",
        response.error
    );
    Ok(())
}
