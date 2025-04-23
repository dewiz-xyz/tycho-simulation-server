mod api;
mod config;
mod handlers;
mod models;
mod services;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use tycho_simulation::{tycho_core::dto::Chain, utils::load_all_tokens};

use crate::api::create_router;
use crate::config::load_config;
use crate::handlers::stream::process_stream;
use crate::models::state::AppState;
use crate::services::stream_builder::build_protocol_stream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load configuration
    let config = load_config();
    println!("Initializing price service...");

    // Load tokens
    let all_tokens = load_all_tokens(
        &config.tycho_url,
        false,
        Some(&config.api_key),
        Chain::Ethereum.into(),
        None,
        None,
    )
    .await;
    println!("Loaded {} tokens", all_tokens.len());

    // Create shared state
    let states = Arc::new(RwLock::new(HashMap::new()));
    let current_block = Arc::new(RwLock::new(0));
    let (update_tx, _) = broadcast::channel(100);

    // Create app state
    let app_state = AppState {
        tokens: Arc::new(all_tokens.clone()),
        states: Arc::clone(&states),
        current_block: Arc::clone(&current_block),
        update_tx: update_tx.clone(),
    };

    // Build protocol stream
    let raw_stream = build_protocol_stream(
        &config.tycho_url,
        &config.api_key,
        config.tvl_threshold,
        all_tokens,
    )
    .await?;
    println!("Protocol stream built successfully");

    // Spawn stream processing task
    let states_clone = Arc::clone(&states);
    let current_block_clone = Arc::clone(&current_block);
    tokio::spawn(async move {
        process_stream(raw_stream, states_clone, current_block_clone, update_tx).await;
    });

    // Create router and start server
    let app = create_router(app_state);

    // Parse the host into IpAddr
    let ip_addr: IpAddr = config.host.parse().expect("Invalid host address");
    let addr = SocketAddr::from((ip_addr, config.port));

    println!("Starting WebSocket server on {}", addr);

    axum::serve(
        tokio::net::TcpListener::bind(&addr)
            .await
            .expect("Failed to bind to address"),
        app.into_make_service(),
    )
    .await
    .expect("Failed to start server");

    Ok(())
}
