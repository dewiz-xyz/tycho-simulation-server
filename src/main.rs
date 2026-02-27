use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tracing::{debug, error, info};

use tycho_simulation::utils::load_all_tokens;

use tycho_simulation_server::api::create_router;
use tycho_simulation_server::config::{init_logging, load_config};
use tycho_simulation_server::handlers::stream::{
    supervise_native_stream, supervise_vm_stream, StreamSupervisorConfig, VmStreamControls,
};
use tycho_simulation_server::memory::maybe_log_memory_snapshot;
use tycho_simulation_server::models::state::{AppState, StateStore, VmStreamStatus};
use tycho_simulation_server::models::stream_health::StreamHealth;
use tycho_simulation_server::models::tokens::TokenStore;
use tycho_simulation_server::services::gas_price::{
    ensure_rpc_chain_matches, fetch_rpc_chain_id, run_native_gas_price_refresh_loop,
};
use tycho_simulation_server::services::stream_builder::{build_native_stream, build_vm_stream};

#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();
    let config = load_config();
    let chain = config.chain_profile.chain;
    info!(chain_id = chain.id(), chain = %chain, "Initializing price service...");
    log_memory_config(config.memory);
    log_erc4626_capability(&config);
    spawn_memory_snapshot_task(config.memory);

    let tokens = load_token_store(&config).await?;
    let stream_resources = create_stream_resources(&tokens);
    let app_state = build_app_state(&config, &tokens, &stream_resources);
    let supervisor_cfg = build_supervisor_config(&config);

    log_concurrency_config(&config);
    spawn_gas_price_refresh(&config, &app_state);
    spawn_native_stream_task(&config, &supervisor_cfg, &tokens, &stream_resources);
    spawn_vm_stream_task(
        &config,
        &supervisor_cfg,
        &tokens,
        &stream_resources,
        &app_state,
    );

    let app = create_router(app_state);
    serve(app, &config).await?;

    Ok(())
}

struct StreamResources {
    native_state_store: Arc<StateStore>,
    vm_state_store: Arc<StateStore>,
    native_stream_health: Arc<StreamHealth>,
    vm_stream_health: Arc<StreamHealth>,
    vm_stream: Arc<tokio::sync::RwLock<VmStreamStatus>>,
}

fn log_memory_config(memory: tycho_simulation_server::config::MemoryConfig) {
    info!(
        event = "memory_config",
        purge_enabled = memory.purge_enabled,
        snapshots_enabled = memory.snapshots_enabled,
        min_interval_secs = memory.snapshots_min_interval_secs,
        min_new_pairs = memory.snapshots_min_new_pairs,
        emf_enabled = memory.snapshots_emit_emf,
        "Memory config loaded"
    );
    maybe_log_memory_snapshot("service", "startup", None, memory, true);
}

fn spawn_memory_snapshot_task(memory_cfg: tycho_simulation_server::config::MemoryConfig) {
    if !memory_cfg.snapshots_enabled {
        return;
    }

    tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(Duration::from_secs(memory_cfg.snapshots_min_interval_secs));
        // `interval` ticks immediately on first await; skip it so "startup" remains first.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            maybe_log_memory_snapshot("service", "periodic", None, memory_cfg, false);
        }
    });
}

async fn load_token_store(
    config: &tycho_simulation_server::config::AppConfig,
) -> anyhow::Result<Arc<TokenStore>> {
    let chain = config.chain_profile.chain;
    let all_tokens = load_all_tokens(
        &config.tycho_url,
        false,
        Some(&config.api_key),
        true,
        chain,
        Some(10),
        None,
    )
    .await?;
    info!("Loaded {} tokens", all_tokens.len());

    Ok(Arc::new(TokenStore::new(
        all_tokens,
        config.tycho_url.clone(),
        config.api_key.clone(),
        chain,
        Duration::from_millis(config.token_refresh_timeout_ms),
    )))
}

fn create_stream_resources(tokens: &Arc<TokenStore>) -> StreamResources {
    let native_state_store = Arc::new(StateStore::new(Arc::clone(tokens)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(tokens)));
    let native_stream_health = Arc::new(StreamHealth::new());
    let vm_stream_health = Arc::new(StreamHealth::new());
    let vm_stream = Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default()));
    debug!("Created shared state");

    StreamResources {
        native_state_store,
        vm_state_store,
        native_stream_health,
        vm_stream_health,
        vm_stream,
    }
}

fn build_app_state(
    config: &tycho_simulation_server::config::AppConfig,
    tokens: &Arc<TokenStore>,
    resources: &StreamResources,
) -> AppState {
    let chain = config.chain_profile.chain;
    let native_sim_concurrency = config.global_native_sim_concurrency;
    let vm_sim_concurrency = config.global_vm_sim_concurrency;
    let readiness_stale = Duration::from_secs(config.readiness_stale_secs);
    let quote_timeout = Duration::from_millis(config.quote_timeout_ms);
    let pool_timeout_native = Duration::from_millis(config.pool_timeout_native_ms);
    let pool_timeout_vm = Duration::from_millis(config.pool_timeout_vm_ms);
    let request_timeout = Duration::from_millis(config.request_timeout_ms);
    // VM is only effective when enabled and the selected chain exposes VM protocols.
    let effective_vm_enabled =
        config.enable_vm_pools && !config.chain_profile.vm_protocols.is_empty();

    AppState {
        chain,
        native_token_protocol_allowlist: Arc::new(
            config.chain_profile.native_token_protocol_allowlist.clone(),
        ),
        tokens: Arc::clone(tokens),
        native_state_store: Arc::clone(&resources.native_state_store),
        vm_state_store: Arc::clone(&resources.vm_state_store),
        native_stream_health: Arc::clone(&resources.native_stream_health),
        vm_stream_health: Arc::clone(&resources.vm_stream_health),
        vm_stream: Arc::clone(&resources.vm_stream),
        latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
        native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
        enable_vm_pools: effective_vm_enabled,
        readiness_stale,
        quote_timeout,
        pool_timeout_native,
        pool_timeout_vm,
        request_timeout,
        native_sim_semaphore: Arc::new(Semaphore::new(native_sim_concurrency)),
        vm_sim_semaphore: Arc::new(Semaphore::new(vm_sim_concurrency)),
        erc4626_deposits_enabled: config.rpc_url.is_some(),
        reset_allowance_tokens: Arc::clone(&config.reset_allowance_tokens),
        native_sim_concurrency,
        vm_sim_concurrency,
    }
}

fn log_erc4626_capability(config: &tycho_simulation_server::config::AppConfig) {
    if config.rpc_url.is_some() {
        info!("ERC4626 deposits enabled: RPC_URL is configured");
    } else {
        info!("ERC4626 deposits disabled: RPC_URL is not configured; redeems remain enabled");
    }
}

fn build_supervisor_config(
    config: &tycho_simulation_server::config::AppConfig,
) -> StreamSupervisorConfig {
    StreamSupervisorConfig {
        stream_stale: Duration::from_secs(config.stream_stale_secs),
        missing_block_burst: config.stream_missing_block_burst,
        missing_block_window: Duration::from_secs(config.stream_missing_block_window_secs),
        error_burst: config.stream_error_burst,
        error_window: Duration::from_secs(config.stream_error_window_secs),
        resync_grace: Duration::from_secs(config.resync_grace_secs),
        restart_backoff_min: Duration::from_millis(config.stream_restart_backoff_min_ms),
        restart_backoff_max: Duration::from_millis(config.stream_restart_backoff_max_ms),
        restart_backoff_jitter_pct: config.stream_restart_backoff_jitter_pct,
        memory: config.memory,
    }
}

fn log_concurrency_config(config: &tycho_simulation_server::config::AppConfig) {
    let effective_vm_enabled =
        config.enable_vm_pools && !config.chain_profile.vm_protocols.is_empty();
    info!(
        native_sim_concurrency = config.global_native_sim_concurrency,
        vm_sim_concurrency = config.global_vm_sim_concurrency,
        enable_vm_pools = effective_vm_enabled,
        requested_vm_pools = config.enable_vm_pools,
        "Initialized simulation concurrency limits"
    );
}

fn spawn_gas_price_refresh(
    config: &tycho_simulation_server::config::AppConfig,
    app_state: &AppState,
) {
    let Some(rpc_url) = config.rpc_url.clone() else {
        info!("RPC_URL is not configured; gas-in-sell reporting remains disabled");
        return;
    };
    let app_state_bg = app_state.clone();
    let chain = config.chain_profile.chain;
    let refresh_interval = Duration::from_millis(config.gas_price_refresh_interval_ms);
    let failure_tolerance = config.gas_price_failure_tolerance;
    let client = reqwest::Client::new();

    tokio::spawn(async move {
        match fetch_rpc_chain_id(&rpc_url, &client).await {
            Ok(rpc_chain_id) => match ensure_rpc_chain_matches(chain, rpc_chain_id) {
                Ok(()) => {
                    let client_bg = client.clone();
                    info!(
                        refresh_interval_ms = refresh_interval.as_millis() as u64,
                        failure_tolerance, rpc_chain_id, "Starting native gas price refresh loop"
                    );
                    run_native_gas_price_refresh_loop(
                        app_state_bg,
                        rpc_url,
                        refresh_interval,
                        failure_tolerance,
                        client_bg,
                    )
                    .await;
                }
                Err(error) => {
                    error!(
                        %error,
                        expected_chain_id = chain.id(),
                        rpc_chain_id,
                        "RPC_URL chain validation failed; gas-in-sell reporting remains disabled"
                    );
                }
            },
            Err(error) => {
                error!(
                    %error,
                    expected_chain_id = chain.id(),
                    "Failed to validate RPC_URL chain via eth_chainId; gas-in-sell reporting remains disabled"
                );
            }
        }
    });
}

fn spawn_native_stream_task(
    config: &tycho_simulation_server::config::AppConfig,
    supervisor_cfg: &StreamSupervisorConfig,
    tokens: &Arc<TokenStore>,
    resources: &StreamResources,
) {
    let chain = config.chain_profile.chain;
    let native_supervisor_cfg = supervisor_cfg.clone();
    let tokens_bg = Arc::clone(tokens);
    let state_store_bg = Arc::clone(&resources.native_state_store);
    let health_bg = Arc::clone(&resources.native_stream_health);
    let tycho_url = config.tycho_url.clone();
    let api_key = config.api_key.clone();
    let tvl_threshold = config.tvl_threshold;
    let tvl_keep_threshold = config.tvl_keep_threshold;
    let native_protocols = config.chain_profile.native_protocols.clone();

    tokio::spawn(async move {
        info!("Starting native protocol stream supervisor...");
        supervise_native_stream(
            move || {
                let tokens = Arc::clone(&tokens_bg);
                let tycho_url = tycho_url.clone();
                let api_key = api_key.clone();
                let protocols = native_protocols.clone();
                async move {
                    build_native_stream(
                        &tycho_url,
                        &api_key,
                        tvl_threshold,
                        tvl_keep_threshold,
                        tokens,
                        chain,
                        &protocols,
                    )
                    .await
                }
            },
            state_store_bg,
            health_bg,
            native_supervisor_cfg,
        )
        .await;
    });
    debug!("Native stream supervisor task spawned");
}

fn spawn_vm_stream_task(
    config: &tycho_simulation_server::config::AppConfig,
    supervisor_cfg: &StreamSupervisorConfig,
    tokens: &Arc<TokenStore>,
    resources: &StreamResources,
    app_state: &AppState,
) {
    let chain = config.chain_profile.chain;
    let effective_vm_enabled =
        config.enable_vm_pools && !config.chain_profile.vm_protocols.is_empty();
    if !effective_vm_enabled {
        if !config.enable_vm_pools {
            info!("VM pool feeds disabled");
        } else {
            info!(
                chain = %chain,
                "VM pool feeds enabled but no VM protocols configured for this chain; skipping VM stream"
            );
        }
        return;
    }

    let vm_supervisor_cfg = supervisor_cfg.clone();
    let tokens_bg = Arc::clone(tokens);
    let state_store_bg = Arc::clone(&resources.vm_state_store);
    let health_bg = Arc::clone(&resources.vm_stream_health);
    let vm_stream_bg = Arc::clone(&resources.vm_stream);
    let vm_semaphore_bg = app_state.vm_sim_semaphore();
    let tycho_url = config.tycho_url.clone();
    let api_key = config.api_key.clone();
    let tvl_threshold = config.tvl_threshold;
    let tvl_keep_threshold = config.tvl_keep_threshold;
    let vm_protocols = config.chain_profile.vm_protocols.clone();
    let vm_sim_concurrency = vm_sim_concurrency_u32(config.global_vm_sim_concurrency);

    tokio::spawn(async move {
        info!("Starting VM protocol stream supervisor...");
        supervise_vm_stream(
            move || {
                let tokens = Arc::clone(&tokens_bg);
                let tycho_url = tycho_url.clone();
                let api_key = api_key.clone();
                let protocols = vm_protocols.clone();
                async move {
                    build_vm_stream(
                        &tycho_url,
                        &api_key,
                        tvl_threshold,
                        tvl_keep_threshold,
                        tokens,
                        chain,
                        &protocols,
                    )
                    .await
                }
            },
            state_store_bg,
            health_bg,
            vm_supervisor_cfg,
            VmStreamControls {
                vm_stream: vm_stream_bg,
                vm_sim_semaphore: vm_semaphore_bg,
                vm_sim_concurrency,
            },
        )
        .await;
    });
    debug!("VM stream supervisor task spawned");
}

async fn serve(
    app: axum::Router,
    config: &tycho_simulation_server::config::AppConfig,
) -> anyhow::Result<()> {
    let addr = SocketAddr::from((config.host, config.port));
    info!("Starting HTTP server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|error| {
            error!("Failed to bind to address: {}", error);
            error
        })?;
    info!("Server listening on {}", addr);

    axum::serve(listener, app.into_make_service())
        .await
        .map_err(|error| {
            error!("Server error: {}", error);
            anyhow::anyhow!("Failed to start server: {}", error)
        })
}

#[expect(
    clippy::panic,
    reason = "invalid startup concurrency is a hard configuration invariant"
)]
fn vm_sim_concurrency_u32(value: usize) -> u32 {
    match u32::try_from(value) {
        Ok(concurrency) => concurrency,
        Err(_) => panic!("VM simulation concurrency exceeds u32 range"),
    }
}
