mod api;
mod config;
mod handlers;
mod models;
mod services;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{debug, error, info};

use tycho_simulation::{tycho_common::models::Chain, utils::load_all_tokens};

use crate::api::create_router;
use crate::config::{init_logging, load_config};
use crate::handlers::stream::process_stream;
use crate::models::rfq::RFQConfig;
use crate::models::state::{AppState, StateStore};
use crate::models::tokens::TokenStore;
use crate::services::stream_builder::build_merged_streams;

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
    let state_store = Arc::new(StateStore::new(Arc::clone(&tokens)));
    debug!("Created shared state");

    // Create app state
    let quote_timeout = Duration::from_millis(config.quote_timeout_ms);
    let pool_timeout_native = Duration::from_millis(config.pool_timeout_native_ms);
    let pool_timeout_vm = Duration::from_millis(config.pool_timeout_vm_ms);
    let pool_timeout_rfq = Duration::from_millis(config.pool_timeout_rfq_ms);
    let request_timeout = Duration::from_millis(config.request_timeout_ms);

    let native_sim_concurrency = config.global_native_sim_concurrency;
    let vm_sim_concurrency = config.global_vm_sim_concurrency;
    let rfq_sim_concurrency = config.global_rfq_sim_concurrency;
    let app_state = AppState {
        tokens: Arc::clone(&tokens),
        state_store: Arc::clone(&state_store),
        quote_timeout,
        pool_timeout_native,
        pool_timeout_vm,
        pool_timeout_rfq,
        request_timeout,
        native_sim_semaphore: Arc::new(Semaphore::new(native_sim_concurrency)),
        vm_sim_semaphore: Arc::new(Semaphore::new(vm_sim_concurrency)),
        rfq_sim_semaphore: Arc::new(Semaphore::new(rfq_sim_concurrency)),
    };

    info!(
        native_sim_concurrency,
        vm_sim_concurrency,
        rfq_sim_concurrency,
        enable_vm_pools = config.enable_vm_pools,
        enable_rfq_pools = config.enable_rfq_pools,
        "Initialized simulation concurrency limits"
    );

    // Build protocol stream in background and start processing
    {
        let cfg = config.clone();
        let tokens_bg = Arc::clone(&tokens);
        let state_store_bg = Arc::clone(&state_store);
        let rfq_config = RFQConfig {
            bebop_user: cfg.bebop_user,
            bebop_key: cfg.bebop_key,
            hashflow_user: cfg.hashflow_user,
            hashflow_key: cfg.hashflow_key,
        };
        tokio::spawn(async move {
            info!("Starting merged protocol streams in background...");
            match build_merged_streams(
                &cfg.tycho_url,
                &cfg.api_key,
                cfg.tvl_threshold,
                cfg.tvl_keep_threshold,
                tokens_bg,
                cfg.enable_vm_pools,
                cfg.enable_rfq_pools,
                rfq_config,
            )
            .await
            {
                Ok(stream) => {
                    info!("Merged protocol streams running (background)");
                    process_stream(stream, state_store_bg).await;
                }
                Err(e) => {
                    error!("Failed to initialize merged streams: {:?}", e);
                }
            }
        });
        debug!("Stream processing task spawned (background)");
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
