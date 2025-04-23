use axum::extract::{
    ws::{WebSocket, WebSocketUpgrade},
    State,
};
use axum::response::IntoResponse;

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
    let mut rx = app_state.update_tx.subscribe();

    // Send initial state
    if let Ok(block) = app_state.current_block.read().await.to_string().parse() {
        let states = app_state.states.read().await;
        let _ = socket
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
