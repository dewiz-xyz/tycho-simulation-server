use axum::{
    extract::State,
    routing::get,
    Router,
    response::IntoResponse,
};
use futures::stream::StreamExt;
use num_bigint::BigUint;
use num_traits::cast::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::broadcast;
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use tycho_simulation::{
    evm::{
        protocol::{
            filters::{curve_pool_filter, balancer_pool_filter},
            uniswap_v2::state::UniswapV2State,
            uniswap_v3::state::UniswapV3State,
            uniswap_v4::state::UniswapV4State,
            vm::state::EVMPoolState,
        },
        stream::ProtocolStreamBuilder,
        engine_db::tycho_db::PreCachedDB,
    },
    models::Token,
    protocol::{
        models::{BlockUpdate, ProtocolComponent},
        state::ProtocolSim,
    },
    tycho_client::feed::component_tracker::ComponentFilter,
    tycho_core::{dto::Chain, Bytes},
    utils::load_all_tokens,
};

#[derive(Clone)]
struct AppState {
    tokens: Arc<HashMap<Bytes, Token>>,
    states: Arc<RwLock<HashMap<String, (Box<dyn ProtocolSim>, ProtocolComponent)>>>,
    current_block: Arc<RwLock<u64>>,
    update_tx: broadcast::Sender<UpdateMessage>,
}

#[derive(Debug, Deserialize, Clone)]
struct AmountOutRequest {
    request_id: Option<String>,
    token_in: String,
    token_out: String,
    amounts: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
struct AmountOutResponse {
    pool: String,
    pool_name: String,
    pool_address: String,
    amounts_out: Vec<String>,
    gas_used: Vec<u64>,
    block_number: u64,
}

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", content = "data")]
enum UpdateMessage {
    BlockUpdate {
        block_number: u64,
        states_count: usize,
        new_pairs_count: usize,
        removed_pairs_count: usize,
    },
    QuoteUpdate {
        request_id: String,
        data: Vec<AmountOutResponse>,
    },
}

async fn process_stream(
    mut stream: impl futures::Stream<Item = Result<BlockUpdate, impl std::error::Error>> + Unpin + Send,
    states: Arc<RwLock<HashMap<String, (Box<dyn ProtocolSim>, ProtocolComponent)>>>,
    current_block: Arc<RwLock<u64>>,
    update_tx: broadcast::Sender<UpdateMessage>,
) {
    println!("Starting stream processing...");
    
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(update) => {
                let mut states_map = states.write().await;
                let mut block = current_block.write().await;
                let prev_block = *block;
                *block = update.block_number;
                
                let new_pairs_count = update.new_pairs.len();
                
                println!(
                    "Block update: {} -> {}, states: {}",
                    prev_block,
                    update.block_number,
                    update.states.len(),
                );
                
                // Update existing states
                for (id, state) in &update.states {
                    if let Some((existing_state, _)) = states_map.get_mut(id) {
                        *existing_state = state.clone();
                    }
                }

                // Add new pairs
                for (id, comp) in update.new_pairs {
                    if let Some(state) = update.states.get(&id) {
                        states_map.insert(id, (state.clone(), comp));
                    }
                }

                // Broadcast update
                let _ = update_tx.send(UpdateMessage::BlockUpdate {
                    block_number: update.block_number,
                    states_count: update.states.len(),
                    new_pairs_count,
                    removed_pairs_count: update.removed_pairs.len(),
                });
            }
            Err(e) => {
                println!("Stream error: {:?}", e);
            }
        }
    }
    println!("Stream ended");
}

async fn handle_ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws_connection(socket, state))
}

async fn handle_ws_connection(mut socket: WebSocket, app_state: AppState) {
    let mut rx = app_state.update_tx.subscribe();

    // Send initial state
    if let Ok(block) = app_state.current_block.read().await.to_string().parse() {
        let states = app_state.states.read().await;
        let _ = socket.send(axum::extract::ws::Message::Text(
            serde_json::to_string(&UpdateMessage::BlockUpdate {
                block_number: block,
                states_count: states.len(),
                new_pairs_count: 0,
                removed_pairs_count: 0,
            }).unwrap()
        )).await;
    }

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(axum::extract::ws::Message::Text(text))) => {
                        if let Ok(req) = serde_json::from_str::<AmountOutRequest>(&text) {
                            println!("Quote request: {} -> {} ({} amounts)", 
                                req.token_in, 
                                req.token_out, 
                                req.amounts.len()
                            );
                            
                            // Process quote request
                            match get_amounts_out(app_state.clone(), req.clone()).await {
                                Ok(quotes) => {
                                    println!("Found {} quotes", quotes.len());
                                    let msg = UpdateMessage::QuoteUpdate {
                                        request_id: req.request_id.unwrap_or_default(),
                                        data: quotes,
                                    };
                                    if socket.send(axum::extract::ws::Message::Text(
                                        serde_json::to_string(&msg).unwrap()
                                    )).await.is_err() {
                                        println!("Failed to send quote response");
                                        break;
                                    }
                                }
                                Err(e) => {
                                    println!("Error getting quotes: {}", e);
                                }
                            }
                        }
                    }
                    Some(Ok(axum::extract::ws::Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            Ok(msg) = rx.recv() => {
                if socket.send(axum::extract::ws::Message::Text(
                    serde_json::to_string(&msg).unwrap()
                )).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn get_amounts_out(
    state: AppState,
    request: AmountOutRequest,
) -> Result<Vec<AmountOutResponse>, String> {
    let token_in_address = request.token_in.trim_start_matches("0x").to_lowercase();
    let token_out_address = request.token_out.trim_start_matches("0x").to_lowercase();

    let token_in_bytes = Bytes::from_str(&token_in_address)
        .map_err(|e| format!("Invalid token_in address: {}", e))?;
    let token_out_bytes = Bytes::from_str(&token_out_address)
        .map_err(|e| format!("Invalid token_out address: {}", e))?;

    let token_in = state.tokens
        .get(&token_in_bytes)
        .ok_or_else(|| format!("Token not found: {}", token_in_address))?;

    let token_out = state.tokens
        .get(&token_out_bytes)
        .ok_or_else(|| format!("Token not found: {}", token_out_address))?;

    println!("Processing quote: {} ({}) -> {} ({})", 
        token_in.symbol, token_in_address,
        token_out.symbol, token_out_address
    );

    let amounts_in: Result<Vec<BigUint>, String> = request.amounts
        .iter()
        .map(|amount| {
            BigUint::from_str(amount)
                .map_err(|e| format!("Invalid amount {}: {}", amount, e))
        })
        .collect();
    let amounts_in = amounts_in?;

    let states = state.states.read().await;
    let current_block = *state.current_block.read().await;
    let mut results = Vec::new();
    let mut matching_pools = 0;
    let mut pools_with_quotes = 0;

    for (id, (state, comp)) in states.iter() {
        let pool_tokens: Vec<String> = comp.tokens.iter()
            .map(|t| t.address.to_string().trim_start_matches("0x").to_lowercase())
            .collect();
        
        if pool_tokens.contains(&token_in_address) && pool_tokens.contains(&token_out_address) {
            matching_pools += 1;
            let mut amounts_out = Vec::new();
            let mut gas_used = Vec::new();

            for amount_in in amounts_in.iter() {
                match state.get_amount_out(amount_in.clone(), token_in, token_out) {
                    Ok(result) => {
                        amounts_out.push(result.amount.to_string());
                        gas_used.push(result.gas.to_u64().unwrap_or(0));
                    }
                    Err(_) => continue,
                }
            }

            if !amounts_out.is_empty() {
                pools_with_quotes += 1;
                let pool_name = format!("{:?}", state);
                let pool_name = pool_name
                    .split_whitespace()
                    .next()
                    .unwrap_or("Unknown")
                    .to_string();

                results.push(AmountOutResponse {
                    pool: id.clone(),
                    pool_name,
                    pool_address: comp.id.to_string(),
                    amounts_out,
                    gas_used,
                    block_number: current_block,
                });
            }
        }
    }

    println!("Found {} matching pools, {} with valid quotes", matching_pools, pools_with_quotes);

    if results.is_empty() {
        return Err(format!("No pools found for pair {}-{}", token_in_address, token_out_address));
    }

    // Sort results by first amount_out (best to worst)
    results.sort_by(|a, b| {
        let a_amount = BigUint::from_str(&a.amounts_out[0]).unwrap_or_default();
        let b_amount = BigUint::from_str(&b.amounts_out[0]).unwrap_or_default();
        b_amount.cmp(&a_amount)
    });

    Ok(results)
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();

    let tycho_url = std::env::var("TYCHO_URL")
        .unwrap_or_else(|_| "tycho-beta.propellerheads.xyz".to_string());
    let api_key = std::env::var("TYCHO_API_KEY")
        .expect("TYCHO_API_KEY must be set");
    let tvl_threshold: f64 = std::env::var("TVL_THRESHOLD")
        .unwrap_or_else(|_| "1000".to_string())
        .parse()
        .expect("Invalid TVL_THRESHOLD");

    println!("Initializing price service...");
    
    let tvl_filter = ComponentFilter::with_tvl_range(tvl_threshold, tvl_threshold);
    let all_tokens = load_all_tokens(&tycho_url, false, Some(&api_key), Chain::Ethereum.into(), None, None).await;
    println!("Loaded {} tokens", all_tokens.len());

    let raw_stream = ProtocolStreamBuilder::new(&tycho_url, Chain::Ethereum.into())
        .exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None)
        .exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None)
        .exchange::<UniswapV4State>("uniswap_v4", tvl_filter.clone(), None)
        .exchange::<EVMPoolState<PreCachedDB>>("vm:curve", tvl_filter.clone(), Some(curve_pool_filter))
        .exchange::<EVMPoolState<PreCachedDB>>(
            "vm:balancer_v2",
            tvl_filter.clone(),
            Some(balancer_pool_filter),
        )
        .auth_key(Some(api_key))
        .skip_state_decode_failures(true)
        .set_tokens(all_tokens.clone())
        .await
        .build()
        .await
        .expect("Failed to build protocol stream");

    println!("Protocol stream built successfully");

    let states = Arc::new(RwLock::new(HashMap::new()));
    let current_block = Arc::new(RwLock::new(0));
    let (update_tx, _) = broadcast::channel(100);

    // Create app state
    let app_state = AppState {
        tokens: Arc::new(all_tokens),
        states: Arc::clone(&states),
        current_block: Arc::clone(&current_block),
        update_tx: update_tx.clone(),
    };

    // Spawn stream processing task
    let states_clone = Arc::clone(&states);
    let current_block_clone = Arc::clone(&current_block);
    tokio::spawn(async move {
        process_stream(raw_stream, states_clone, current_block_clone, update_tx).await;
    });

    // Build API router
    let app = Router::new()
        .route("/ws", get(handle_ws_upgrade))
        .with_state(app_state);

    // Start server
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("Starting WebSocket server on {}", addr);
    axum::serve(
        tokio::net::TcpListener::bind(&addr)
            .await
            .expect("Failed to bind to address"),
        app.into_make_service(),
    )
    .await
    .expect("Failed to start server");
} 