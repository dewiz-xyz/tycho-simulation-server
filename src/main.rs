use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{debug, error, info};

use tycho_simulation::{tycho_common::models::Chain, utils::load_all_tokens};

use tycho_simulation_server::api::create_router;
use tycho_simulation_server::config::{init_logging, load_config};
use tycho_simulation_server::handlers::stream::{
    supervise_native_stream, supervise_vm_stream, StreamSupervisorConfig, VmStreamControls,
};
use tycho_simulation_server::memory::maybe_log_memory_snapshot;
use tycho_simulation_server::models::state::{AppState, StateStore, VmStreamStatus};
use tycho_simulation_server::models::stream_health::StreamHealth;
use tycho_simulation_server::models::tokens::TokenStore;
use tycho_simulation_server::services::stream_builder::{build_native_stream, build_vm_stream};

#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    init_logging();

    // Load configuration
    let config = load_config();
    info!("Initializing price service...");

    info!(
        event = "memory_config",
        purge_enabled = config.memory.purge_enabled,
        snapshots_enabled = config.memory.snapshots_enabled,
        min_interval_secs = config.memory.snapshots_min_interval_secs,
        min_new_pairs = config.memory.snapshots_min_new_pairs,
        emf_enabled = config.memory.snapshots_emit_emf,
        "Memory config loaded"
    );
    maybe_log_memory_snapshot("service", "startup", None, config.memory, true);

    if config.memory.snapshots_enabled {
        let memory_cfg = config.memory;
        tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval(Duration::from_secs(memory_cfg.snapshots_min_interval_secs));
            // tokio::time::interval ticks immediately on first await; skip it so "startup"
            // remains the first snapshot by default.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                maybe_log_memory_snapshot("service", "periodic", None, memory_cfg, false);
            }
        });
    }

    // Load tokens
    let all_tokens = load_all_tokens(
        &config.tycho_url,
        false,
        Some(&config.api_key),
        true,
        Chain::Ethereum,
        Some(10),
        None,
    )
    .await?;
    info!("Loaded {} tokens", all_tokens.len());

    // Create shared state
    // Shared token cache across native + VM stores to avoid duplicate fetches and keep metadata consistent.
    let tokens = Arc::new(TokenStore::new(
        all_tokens,
        config.tycho_url.clone(),
        config.api_key.clone(),
        Chain::Ethereum,
        Duration::from_millis(config.token_refresh_timeout_ms),
    ));
    let native_state_store = Arc::new(StateStore::new(Arc::clone(&tokens)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(&tokens)));
    let native_stream_health = Arc::new(StreamHealth::new());
    let vm_stream_health = Arc::new(StreamHealth::new());
    let vm_stream = Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default()));
    debug!("Created shared state");

    // Create app state
    let quote_timeout = Duration::from_millis(config.quote_timeout_ms);
    let pool_timeout_native = Duration::from_millis(config.pool_timeout_native_ms);
    let pool_timeout_vm = Duration::from_millis(config.pool_timeout_vm_ms);
    let request_timeout = Duration::from_millis(config.request_timeout_ms);

    let native_sim_concurrency = config.global_native_sim_concurrency;
    let vm_sim_concurrency = config.global_vm_sim_concurrency;
    let readiness_stale = Duration::from_secs(config.readiness_stale_secs);
    let app_state = AppState {
        tokens: Arc::clone(&tokens),
        native_state_store: Arc::clone(&native_state_store),
        vm_state_store: Arc::clone(&vm_state_store),
        native_stream_health: Arc::clone(&native_stream_health),
        vm_stream_health: Arc::clone(&vm_stream_health),
        vm_stream: Arc::clone(&vm_stream),
        enable_vm_pools: config.enable_vm_pools,
        readiness_stale,
        quote_timeout,
        pool_timeout_native,
        pool_timeout_vm,
        request_timeout,
        native_sim_semaphore: Arc::new(Semaphore::new(native_sim_concurrency)),
        vm_sim_semaphore: Arc::new(Semaphore::new(vm_sim_concurrency)),
        reset_allowance_tokens: Arc::new(config.reset_allowance_tokens.clone()),
        native_sim_concurrency,
        vm_sim_concurrency,
    };

    info!(
        native_sim_concurrency,
        vm_sim_concurrency,
        enable_vm_pools = config.enable_vm_pools,
        "Initialized simulation concurrency limits"
    );

    let supervisor_cfg = StreamSupervisorConfig {
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
    };

    // Build protocol streams in background and start processing
    {
        let cfg = config.clone();
        let tokens_bg = Arc::clone(&tokens);
        let state_store_bg = Arc::clone(&native_state_store);
        let health_bg = Arc::clone(&native_stream_health);
        let tycho_url = cfg.tycho_url.clone();
        let api_key = cfg.api_key.clone();
        let tvl_threshold = cfg.tvl_threshold;
        let tvl_keep_threshold = cfg.tvl_keep_threshold;
        tokio::spawn(async move {
            info!("Starting native protocol stream supervisor...");
            supervise_native_stream(
                move || {
                    let tokens = Arc::clone(&tokens_bg);
                    let tycho_url = tycho_url.clone();
                    let api_key = api_key.clone();
                    async move {
                        build_native_stream(
                            &tycho_url,
                            &api_key,
                            tvl_threshold,
                            tvl_keep_threshold,
                            tokens,
                        )
                        .await
                    }
                },
                state_store_bg,
                health_bg,
                supervisor_cfg,
            )
            .await;
        });
        debug!("Native stream supervisor task spawned");
    }

    if config.enable_vm_pools {
        let cfg = config.clone();
        let tokens_bg = Arc::clone(&tokens);
        let state_store_bg = Arc::clone(&vm_state_store);
        let health_bg = Arc::clone(&vm_stream_health);
        let vm_stream_bg = Arc::clone(&vm_stream);
        let vm_semaphore_bg = app_state.vm_sim_semaphore();
        let tycho_url = cfg.tycho_url.clone();
        let api_key = cfg.api_key.clone();
        let tvl_threshold = cfg.tvl_threshold;
        let tvl_keep_threshold = cfg.tvl_keep_threshold;
        let vm_sim_concurrency =
            u32::try_from(vm_sim_concurrency).expect("VM simulation concurrency exceeds u32 range");

        tokio::spawn(async move {
            info!("Starting VM protocol stream supervisor...");
            supervise_vm_stream(
                move || {
                    let tokens = Arc::clone(&tokens_bg);
                    let tycho_url = tycho_url.clone();
                    let api_key = api_key.clone();
                    async move {
                        build_vm_stream(
                            &tycho_url,
                            &api_key,
                            tvl_threshold,
                            tvl_keep_threshold,
                            tokens,
                        )
                        .await
                    }
                },
                state_store_bg,
                health_bg,
                supervisor_cfg,
                VmStreamControls {
                    vm_stream: vm_stream_bg,
                    vm_sim_semaphore: vm_semaphore_bg,
                    vm_sim_concurrency,
                },
            )
            .await;
        });
        debug!("VM stream supervisor task spawned");
    } else {
        info!("VM pool feeds disabled");
    }

    // Create router and start server
    let app = create_router(app_state);

    // Parse the host into IpAddr
    let ip_addr: IpAddr = config.host.parse().expect("Invalid host address");
    let addr = SocketAddr::from((ip_addr, config.port));

    info!("Starting HTTP server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        error!("Failed to bind to address: {}", e);
        e
    })?;

    info!("Server listening on {}", addr);

    axum::serve(listener, app.into_make_service())
        .await
        .map_err(|e| {
            error!("Server error: {}", e);
            anyhow::anyhow!("Failed to start server: {}", e)
        })?;

    Ok(())
}
