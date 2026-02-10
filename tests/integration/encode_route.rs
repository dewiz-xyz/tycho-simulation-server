use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

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

#[derive(Debug, Clone)]
struct EchoAmountSim;

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
        other.as_any().is::<EchoAmountSim>()
    }
}

fn make_token(address: &Bytes, symbol: &str) -> Token {
    Token::new(address, symbol, 18, 0, &[], Chain::Ethereum, 100)
}

fn hex_to_bytes(value: &str) -> Vec<u8> {
    let stripped = value.strip_prefix("0x").unwrap_or(value);
    alloy_primitives::hex::decode(stripped).expect("valid hex")
}

async fn setup_app_state_and_request(min_amount_out: &str) -> (axum::Router, RouteEncodeRequest) {
    let token_in_hex = "0x0000000000000000000000000000000000000001";
    let token_out_hex = "0x0000000000000000000000000000000000000002";
    let token_in = Bytes::from_str(token_in_hex).unwrap();
    let token_out = Bytes::from_str(token_out_hex).unwrap();
    let settlement = "0x0000000000000000000000000000000000000003".to_string();
    let router = "0x0000000000000000000000000000000000000004".to_string();
    let pool_id = "pool-1".to_string();

    let token_in_meta = make_token(&token_in, "TK1");
    let token_out_meta = make_token(&token_out, "TK2");

    let mut initial_tokens = HashMap::new();
    initial_tokens.insert(token_in.clone(), token_in_meta.clone());
    initial_tokens.insert(token_out.clone(), token_out_meta.clone());

    let tokens_store = Arc::new(TokenStore::new(
        initial_tokens,
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_millis(10),
    ));

    let native_state_store = Arc::new(StateStore::new(Arc::clone(&tokens_store)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(&tokens_store)));

    let component = ProtocolComponent::new(
        Bytes::from_str("0x0000000000000000000000000000000000000009").unwrap(),
        "uniswap_v2".to_string(),
        "uniswap_v2".to_string(),
        Chain::Ethereum,
        vec![token_in_meta.clone(), token_out_meta.clone()],
        Vec::new(),
        HashMap::new(),
        Bytes::default(),
        NaiveDateTime::default(),
    );

    let mut states = HashMap::new();
    states.insert(
        pool_id.clone(),
        Box::new(EchoAmountSim) as Box<dyn ProtocolSim>,
    );
    let mut new_pairs = HashMap::new();
    new_pairs.insert(pool_id.clone(), component);
    native_state_store
        .apply_update(Update::new(42, states, new_pairs))
        .await;

    let mut reset_allowance_tokens: HashMap<u64, HashSet<Bytes>> = HashMap::new();
    reset_allowance_tokens
        .entry(1)
        .or_default()
        .insert(token_in.clone());

    let state = AppState {
        tokens: Arc::clone(&tokens_store),
        native_state_store: Arc::clone(&native_state_store),
        vm_state_store: Arc::clone(&vm_state_store),
        native_stream_health: Arc::new(StreamHealth::new()),
        vm_stream_health: Arc::new(StreamHealth::new()),
        vm_stream: Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
        enable_vm_pools: false,
        readiness_stale: Duration::from_secs(120),
        quote_timeout: Duration::from_secs(1),
        pool_timeout_native: Duration::from_secs(1),
        pool_timeout_vm: Duration::from_secs(1),
        request_timeout: Duration::from_secs(2),
        native_sim_semaphore: Arc::new(Semaphore::new(4)),
        vm_sim_semaphore: Arc::new(Semaphore::new(1)),
        reset_allowance_tokens: Arc::new(reset_allowance_tokens),
        native_sim_concurrency: 4,
        vm_sim_concurrency: 1,
    };

    let request = RouteEncodeRequest {
        chain_id: 1,
        token_in: token_in_hex.to_string(),
        token_out: token_out_hex.to_string(),
        amount_in: "10".to_string(),
        min_amount_out: min_amount_out.to_string(),
        settlement_address: settlement,
        tycho_router_address: router,
        swap_kind: SwapKind::SimpleSwap,
        segments: vec![SegmentDraft {
            kind: SwapKind::SimpleSwap,
            // A single segment must have `share_bps=0` to take the remainder.
            share_bps: 0,
            hops: vec![HopDraft {
                token_in: token_in_hex.to_string(),
                token_out: token_out_hex.to_string(),
                swaps: vec![PoolSwapDraft {
                    pool: PoolRef {
                        protocol: "uniswap_v2".to_string(),
                        component_id: pool_id,
                        pool_address: None,
                    },
                    token_in: token_in_hex.to_string(),
                    token_out: token_out_hex.to_string(),
                    // A single swap must have `split_bps=0` to take the remainder.
                    split_bps: 0,
                }],
            }],
        }],
        request_id: Some("req-1".to_string()),
    };

    let router = create_router(state);
    (router, request)
}

#[tokio::test]
async fn encode_route_end_to_end_returns_interactions_and_debug() {
    let (app, request) = setup_app_state_and_request("8").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/encode")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let response: RouteEncodeResponse = serde_json::from_slice(&body).unwrap();

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
    let reset_calldata = hex_to_bytes(&response.interactions[0].calldata);
    let approve_calldata = hex_to_bytes(&response.interactions[1].calldata);
    assert_eq!(reset_calldata.len(), 4 + 32 + 32);
    assert_eq!(approve_calldata.len(), 4 + 32 + 32);

    let router_bytes = hex_to_bytes("0x0000000000000000000000000000000000000004");
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
        .expect("debug should be present with requestId");
    assert_eq!(debug.request_id.as_deref(), Some("req-1"));
    assert_eq!(
        debug
            .resimulation
            .as_ref()
            .and_then(|resim| resim.block_number),
        Some(42)
    );
}

#[tokio::test]
async fn encode_route_rejects_when_min_amount_out_exceeds_expected() {
    let (app, request) = setup_app_state_and_request("11").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/encode")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let response: EncodeErrorResponse = serde_json::from_slice(&body).unwrap();
    assert!(
        response.error.contains("below minAmountOut"),
        "unexpected error: {}",
        response.error
    );
    assert_eq!(response.request_id.as_deref(), Some("req-1"));
}
