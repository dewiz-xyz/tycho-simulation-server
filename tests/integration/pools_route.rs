use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::NaiveDateTime;
use num_bigint::BigUint;
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
use tycho_simulation_server::models::state::{AppState, StateStore, VmStreamStatus};
use tycho_simulation_server::models::stream_health::StreamHealth;
use tycho_simulation_server::models::tokens::TokenStore;

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
        other.as_any().is::<DummySim>()
    }
}

fn token(seed: u8, symbol: &str) -> Token {
    Token::new(&Bytes::from([seed; 20]), symbol, 18, 0, &[], Chain::Ethereum, 100)
}

fn component(
    address: &str,
    protocol_system: &str,
    protocol_type_name: &str,
    tokens: Vec<Token>,
) -> ProtocolComponent {
    ProtocolComponent::new(
        Bytes::from_str(address).expect("valid component address"),
        protocol_system.to_string(),
        protocol_type_name.to_string(),
        Chain::Ethereum,
        tokens,
        Vec::new(),
        HashMap::new(),
        Bytes::default(),
        NaiveDateTime::default(),
    )
}

async fn build_router() -> axum::Router {
    let token_a = token(1, "TK1");
    let token_b = token(2, "TK2");
    let token_c = token(3, "TK3");

    let mut known_tokens = HashMap::new();
    for item in [&token_a, &token_b, &token_c] {
        known_tokens.insert(item.address.clone(), item.clone());
    }

    let token_store = Arc::new(TokenStore::new(
        known_tokens,
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_millis(10),
    ));

    let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

    native_state_store
        .apply_update(Update::new(
            42,
            HashMap::from([(
                "uni-pool".to_string(),
                Box::new(DummySim) as Box<dyn ProtocolSim>,
            )]),
            HashMap::from([(
                "uni-pool".to_string(),
                component(
                    "0x0000000000000000000000000000000000000011",
                    "uniswap_v2",
                    "uniswap_v2_pool",
                    vec![token_a.clone(), token_b.clone()],
                ),
            )]),
        ))
        .await;

    vm_state_store
        .apply_update(Update::new(
            42,
            HashMap::from([(
                "curve-pool".to_string(),
                Box::new(DummySim) as Box<dyn ProtocolSim>,
            )]),
            HashMap::from([(
                "curve-pool".to_string(),
                component(
                    "0x0000000000000000000000000000000000000022",
                    "vm:curve",
                    "curve_pool",
                    vec![token_b.clone(), token_c.clone()],
                ),
            )]),
        ))
        .await;

    let state = AppState {
        tokens: Arc::clone(&token_store),
        native_state_store,
        vm_state_store,
        native_stream_health: Arc::new(StreamHealth::new()),
        vm_stream_health: Arc::new(StreamHealth::new()),
        vm_stream: Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
        latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
        native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
        enable_vm_pools: true,
        readiness_stale: Duration::from_secs(120),
        quote_timeout: Duration::from_secs(1),
        pool_timeout_native: Duration::from_secs(1),
        pool_timeout_vm: Duration::from_secs(1),
        request_timeout: Duration::from_secs(2),
        native_sim_semaphore: Arc::new(Semaphore::new(4)),
        vm_sim_semaphore: Arc::new(Semaphore::new(1)),
        reset_allowance_tokens: Arc::new(HashMap::<u64, HashSet<Bytes>>::new()),
        native_sim_concurrency: 4,
        vm_sim_concurrency: 1,
    };

    create_router(state)
}

#[tokio::test]
async fn pools_route_returns_grouped_pools_for_all_protocols() {
    let app = build_router().await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/pools")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

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

    let payload: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let protocols = payload["protocols"].as_array().expect("protocols array");
    assert_eq!(protocols.len(), 2);

    let curve = protocols
        .iter()
        .find(|entry| entry["protocol"] == "vm:curve")
        .expect("curve protocol group");
    assert_eq!(curve["count"], 1);
    assert_eq!(curve["pools"][0]["id"], "curve-pool");
    assert_eq!(
        curve["pools"][0]["address"],
        "0x0000000000000000000000000000000000000022"
    );
    assert_eq!(curve["pools"][0]["tokens"][0]["symbol"], "TK2");
}

#[tokio::test]
async fn pools_route_filters_to_requested_protocol() {
    let app = build_router().await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/pools?protocol=vm:curve")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

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

    let payload: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(payload["protocol"], "vm:curve");
    assert_eq!(payload["count"], 1);
    assert_eq!(payload["pools"][0]["id"], "curve-pool");
    assert_eq!(
        payload["pools"][0]["address"],
        "0x0000000000000000000000000000000000000022"
    );
}
