use axum::extract::{
    ws::{WebSocket, WebSocketUpgrade},
    State,
};
use axum::response::IntoResponse;
use tracing::{debug, error, info};

use crate::models::{
    messages::{AmountOutRequest, UpdateMessage},
    state::AppState,
};
use crate::services::quotes::get_amounts_out;

pub async fn handle_ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws_connection(socket, state))
}

async fn handle_ws_connection(mut socket: WebSocket, app_state: AppState) {
    debug!("New WebSocket connection established");
    let mut rx = app_state.update_tx.subscribe();

    // Send initial state
    if let Ok(block) = app_state.current_block.read().await.to_string().parse() {
        let states = app_state.states.read().await;
        debug!(
            "Sending initial state: block={}, states={}",
            block,
            states.len()
        );

        let result = socket
            .send(axum::extract::ws::Message::Text(
                serde_json::to_string(&UpdateMessage::BlockUpdate {
                    block_number: block,
                    states_count: states.len(),
                    new_pairs_count: 0,
                    removed_pairs_count: 0,
                })
                .unwrap(),
            ))
            .await;

        if result.is_err() {
            error!("Failed to send initial state");
            return;
        }
    }

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(axum::extract::ws::Message::Text(text))) => {
                        if let Ok(req) = serde_json::from_str::<AmountOutRequest>(&text) {
                            info!("Quote request: {} -> {} ({} amounts)",
                                req.token_in,
                                req.token_out,
                                req.amounts.len()
                            );

                            // Process quote request
                            match get_amounts_out(app_state.clone(), req.clone()).await {
                                Ok(quotes) => {
                                    info!("Found {} quotes", quotes.len());
                                    let msg = UpdateMessage::QuoteUpdate {
                                        request_id: req.request_id.unwrap_or_default(),
                                        data: quotes,
                                    };
                                    if socket.send(axum::extract::ws::Message::Text(
                                        serde_json::to_string(&msg).unwrap()
                                    )).await.is_err() {
                                        error!("Failed to send quote response");
                                        break;
                                    }
                                    debug!("Quote response sent successfully");
                                }
                                Err(e) => {
                                    error!("Error getting quotes: {}", e);
                                }
                            }
                        }
                    }
                    Some(Ok(axum::extract::ws::Message::Close(_))) => {
                        info!("WebSocket connection closed by client");
                        break;
                    },
                    None => {
                        info!("WebSocket connection closed unexpectedly");
                        break;
                    },
                    _ => {
                        debug!("Received non-text message, ignoring");
                    }
                }
            }
            Ok(msg) = rx.recv() => {
                debug!("Received broadcast update, forwarding to client");
                if socket.send(axum::extract::ws::Message::Text(
                    serde_json::to_string(&msg).unwrap()
                )).await.is_err() {
                    error!("Failed to send broadcast message to client");
                    break;
                }
            }
        }
    }
}
