use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{debug, error, info};

use tycho_simulation::{tycho_common::models::Chain, utils::load_all_tokens};

use tycho_simulation_server::api::create_router;
use tycho_simulation_server::config::{init_logging, load_config};
use tycho_simulation_server::handlers::stream::{
    process_stream, run_vm_stream_manager, VmStreamManagerArgs,
};
use tycho_simulation_server::models::state::{AppState, StateStore, VmStreamStatus};
use tycho_simulation_server::models::tokens::TokenStore;
use tycho_simulation_server::services::stream_builder::build_native_stream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    init_logging();

    // Load configuration
    let config = load_config();
    info!("Initializing price service...");

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
    let tokens = Arc::new(TokenStore::new(
        all_tokens,
        config.tycho_url.clone(),
        config.api_key.clone(),
        Chain::Ethereum,
        Duration::from_millis(config.token_refresh_timeout_ms),
    ));

    let native_state_store = Arc::new(StateStore::new(Arc::clone(&tokens)));
    let vm_state_store = Arc::new(StateStore::new(Arc::clone(&tokens)));
    let vm_stream = Arc::new(VmStreamStatus::default());
    debug!("Created shared state");

    // Create app state
    let quote_timeout = Duration::from_millis(config.quote_timeout_ms);
    let pool_timeout_native = Duration::from_millis(config.pool_timeout_native_ms);
    let pool_timeout_vm = Duration::from_millis(config.pool_timeout_vm_ms);
    let request_timeout = Duration::from_millis(config.request_timeout_ms);

    let native_sim_concurrency = config.global_native_sim_concurrency;
    let vm_sim_concurrency = config.global_vm_sim_concurrency;
    let app_state = AppState {
        tokens: Arc::clone(&tokens),
        enable_vm_pools: config.enable_vm_pools,
        native_state_store: Arc::clone(&native_state_store),
        vm_state_store: Arc::clone(&vm_state_store),
        vm_stream: Arc::clone(&vm_stream),
        quote_timeout,
        pool_timeout_native,
        pool_timeout_vm,
        request_timeout,
        native_sim_semaphore: Arc::new(Semaphore::new(native_sim_concurrency)),
        vm_sim_semaphore: Arc::new(Semaphore::new(vm_sim_concurrency)),
    };

    info!(
        native_sim_concurrency,
        vm_sim_concurrency,
        enable_vm_pools = config.enable_vm_pools,
        "Initialized simulation concurrency limits"
    );

    // Build native protocol stream in background and start processing
    {
        let cfg = config.clone();
        let tokens_bg = Arc::clone(&tokens);
        let state_store_bg = Arc::clone(&native_state_store);
        tokio::spawn(async move {
            info!("Starting native protocol stream in background...");
            match build_native_stream(
                &cfg.tycho_url,
                &cfg.api_key,
                cfg.tvl_threshold,
                cfg.tvl_keep_threshold,
                tokens_bg,
            )
            .await
            {
                Ok(stream) => {
                    info!("Native protocol stream running (background)");
                    let _ = process_stream(stream, state_store_bg, "native", false).await;
                }
                Err(e) => {
                    error!("Failed to initialize native stream: {:?}", e);
                }
            }
        });
        debug!("Stream processing task spawned (background)");
    }

    if config.enable_vm_pools {
        let cfg = config.clone();
        let tokens_bg = Arc::clone(&tokens);
        let vm_store_bg = Arc::clone(&vm_state_store);
        let vm_stream_bg = Arc::clone(&vm_stream);
        let vm_semaphore_bg = app_state.vm_sim_semaphore();
        tokio::spawn(async move {
            info!("Starting VM protocol stream manager in background...");
            run_vm_stream_manager(VmStreamManagerArgs {
                tycho_url: cfg.tycho_url,
                api_key: cfg.api_key,
                tvl_add_threshold: cfg.tvl_threshold,
                tvl_keep_threshold: cfg.tvl_keep_threshold,
                tokens: tokens_bg,
                vm_state_store: vm_store_bg,
                vm_stream: vm_stream_bg,
                vm_sim_semaphore: vm_semaphore_bg,
                vm_sim_concurrency,
            })
            .await
        });
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
