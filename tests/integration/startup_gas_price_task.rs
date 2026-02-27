use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Json, State},
    response::IntoResponse,
    routing::post,
    Router,
};
use reqwest::Client;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, Semaphore};
use tokio::time::{sleep, timeout, Instant};
use tycho_simulation::tycho_common::{models::Chain, Bytes};
use tycho_simulation_server::models::{
    state::{AppState, StateStore, VmStreamStatus},
    stream_health::StreamHealth,
    tokens::TokenStore,
};
use tycho_simulation_server::services::gas_price::spawn_gas_price_startup_task;

#[derive(Clone, Copy)]
enum RpcScenario {
    RetryThenSuccess,
    ChainMismatch,
}

#[derive(Default)]
struct RpcCounters {
    chain_id_calls: AtomicUsize,
    gas_price_calls: AtomicUsize,
}

struct RpcMockState {
    scenario: RpcScenario,
    counters: RpcCounters,
}

fn build_test_app_state() -> AppState {
    let token_store = Arc::new(TokenStore::new(
        HashMap::new(),
        "http://localhost".to_string(),
        "test".to_string(),
        Chain::Ethereum,
        Duration::from_millis(10),
    ));

    AppState {
        chain: Chain::Ethereum,
        native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
        tokens: Arc::clone(&token_store),
        native_state_store: Arc::new(StateStore::new(Arc::clone(&token_store))),
        vm_state_store: Arc::new(StateStore::new(token_store)),
        native_stream_health: Arc::new(StreamHealth::new()),
        vm_stream_health: Arc::new(StreamHealth::new()),
        vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
        latest_native_gas_price_wei: Arc::new(RwLock::new(None)),
        native_gas_price_reporting_enabled: Arc::new(RwLock::new(false)),
        enable_vm_pools: false,
        readiness_stale: Duration::from_secs(120),
        quote_timeout: Duration::from_millis(100),
        pool_timeout_native: Duration::from_millis(50),
        pool_timeout_vm: Duration::from_millis(50),
        request_timeout: Duration::from_millis(1000),
        native_sim_semaphore: Arc::new(Semaphore::new(1)),
        vm_sim_semaphore: Arc::new(Semaphore::new(1)),
        reset_allowance_tokens: Arc::new(HashMap::<u64, HashSet<Bytes>>::new()),
        native_sim_concurrency: 1,
        vm_sim_concurrency: 1,
    }
}

async fn mock_rpc(
    State(state): State<Arc<RpcMockState>>,
    Json(payload): Json<serde_json::Value>,
) -> axum::response::Response {
    let Some(method) = payload.get("method").and_then(serde_json::Value::as_str) else {
        return (axum::http::StatusCode::BAD_REQUEST, "missing method").into_response();
    };

    match method {
        "eth_chainId" => {
            let call = state.counters.chain_id_calls.fetch_add(1, Ordering::SeqCst);
            match state.scenario {
                RpcScenario::RetryThenSuccess if call == 0 => {
                    (axum::http::StatusCode::SERVICE_UNAVAILABLE, "try again").into_response()
                }
                RpcScenario::RetryThenSuccess => {
                    Json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x1"})).into_response()
                }
                RpcScenario::ChainMismatch => {
                    Json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x2105"}))
                        .into_response()
                }
            }
        }
        "eth_gasPrice" => {
            state
                .counters
                .gas_price_calls
                .fetch_add(1, Ordering::SeqCst);
            Json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x3b9aca00"})).into_response()
        }
        _ => (axum::http::StatusCode::BAD_REQUEST, "unexpected method").into_response(),
    }
}

async fn spawn_mock_rpc_server(
    scenario: RpcScenario,
) -> (String, Arc<RpcMockState>, tokio::task::JoinHandle<()>) {
    let state = Arc::new(RpcMockState {
        scenario,
        counters: RpcCounters::default(),
    });
    let app = Router::new()
        .route("/", post(mock_rpc))
        .with_state(Arc::clone(&state));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (format!("http://{}", addr), state, server)
}

#[tokio::test]
async fn startup_task_retries_chain_validation_then_enables_reporting() {
    let (rpc_url, rpc_state, server_handle) =
        spawn_mock_rpc_server(RpcScenario::RetryThenSuccess).await;
    let app_state = build_test_app_state();

    let startup_handle = spawn_gas_price_startup_task(
        app_state.clone(),
        Chain::Ethereum,
        rpc_url,
        Duration::from_millis(5),
        5,
        Client::new(),
    );

    let start = Instant::now();
    timeout(Duration::from_secs(1), async {
        loop {
            if app_state.native_gas_price_reporting_enabled().await {
                break;
            }
            if start.elapsed() > Duration::from_secs(1) {
                panic!("reporting did not become enabled in time");
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("timed out waiting for gas reporting to become enabled");

    assert_eq!(
        app_state.latest_native_gas_price_wei().await,
        Some(1_000_000_000)
    );
    assert!(rpc_state.counters.chain_id_calls.load(Ordering::SeqCst) >= 2);
    assert!(rpc_state.counters.gas_price_calls.load(Ordering::SeqCst) >= 1);

    startup_handle.abort();
    let _ = startup_handle.await;
    server_handle.abort();
    let _ = server_handle.await;
}

#[tokio::test]
async fn startup_task_fails_fast_on_chain_mismatch() {
    let (rpc_url, rpc_state, server_handle) =
        spawn_mock_rpc_server(RpcScenario::ChainMismatch).await;
    let app_state = build_test_app_state();

    let startup_handle = spawn_gas_price_startup_task(
        app_state.clone(),
        Chain::Ethereum,
        rpc_url,
        Duration::from_millis(5),
        5,
        Client::new(),
    );

    timeout(Duration::from_secs(1), startup_handle)
        .await
        .expect("startup task should exit on chain mismatch")
        .expect("startup task join should not panic");

    assert!(!app_state.native_gas_price_reporting_enabled().await);
    assert_eq!(app_state.latest_native_gas_price_wei().await, None);
    assert_eq!(rpc_state.counters.gas_price_calls.load(Ordering::SeqCst), 0);

    server_handle.abort();
    let _ = server_handle.await;
}
