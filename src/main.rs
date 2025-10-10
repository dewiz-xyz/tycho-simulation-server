mod api;
mod config;
mod handlers;
mod models;
mod services;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, error, info};

use tycho_simulation::{tycho_common::models::Chain, utils::load_all_tokens};

use crate::api::create_router;
use crate::config::{init_logging, load_config};
use crate::handlers::stream::process_stream;
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
        Chain::Ethereum,
        None,
        None,
    )
    .await;
    info!("Loaded {} tokens", all_tokens.len());

    // Create shared state
    let tokens = Arc::new(TokenStore::new(
        all_tokens,
        config.tycho_url.clone(),
        config.api_key.clone(),
        Chain::Ethereum,
    ));
    let state_store = Arc::new(StateStore::new());
    let (update_tx, _) = broadcast::channel(100);
    debug!("Created shared state and broadcast channel");

    // Create app state
    let quote_timeout = Duration::from_millis(config.quote_timeout_ms);
    let pool_timeout = Duration::from_millis(config.pool_timeout_ms);
    let cancellation_ttl = Duration::from_secs(config.cancellation_ttl_secs);

    let app_state = AppState {
        tokens: Arc::clone(&tokens),
        state_store: Arc::clone(&state_store),
        update_tx: update_tx.clone(),
        quote_timeout,
        pool_timeout,
        cancellation_ttl,
        auction_cancellation_enabled: config.auction_cancellation_enabled,
    };

    // Build protocol stream in background and start processing
    {
        let cfg = config.clone();
        let tokens_bg = Arc::clone(&tokens);
        let state_store_bg = Arc::clone(&state_store);
        let update_tx_bg = update_tx.clone();
        tokio::spawn(async move {
            info!("Starting merged protocol streams in background...");
            match build_merged_streams(
                &cfg.tycho_url,
                &cfg.api_key,
                cfg.tvl_threshold,
                cfg.tvl_keep_threshold,
                tokens_bg,
            )
            .await
            {
                Ok(stream) => {
                    info!("Merged protocol streams running (background)");
                    process_stream(stream, state_store_bg, update_tx_bg).await;
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

    info!("Starting WebSocket server on {}", addr);

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
