use axum::extract::{
    ws::{WebSocket, WebSocketUpgrade},
    State,
};
use axum::response::IntoResponse;
use tracing::{debug, error, info};

use crate::models::{
    messages::{AmountOutRequest, QuoteFailure, UpdateMessage},
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
    let block = app_state.current_block().await;
    let pool_count = app_state.total_pools().await;
    debug!(
        "Sending initial state: block={}, states={}",
        block, pool_count
    );

    if socket
        .send(axum::extract::ws::Message::Text(
            serde_json::to_string(&UpdateMessage::BlockUpdate {
                block_number: block,
                states_count: pool_count,
                new_pairs_count: 0,
                removed_pairs_count: 0,
            })
            .unwrap(),
        ))
        .await
        .is_err()
    {
        error!("Failed to send initial state");
        return;
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
                            let result = get_amounts_out(app_state.clone(), req.clone()).await;
                            let failure_summaries: Vec<String> = result
                                .meta
                                .failures
                                .iter()
                                .map(format_failure_summary)
                                .collect();

                            if failure_summaries.is_empty() {
                                info!(
                                    "Quote status={:?} pools={} responses={}",
                                    result.meta.status,
                                    result.meta.matching_pools,
                                    result.responses.len()
                                );
                            } else {
                                info!(
                                    failures = %failure_summaries.join("; "),
                                    "Quote status={:?} pools={} responses={}",
                                    result.meta.status,
                                    result.meta.matching_pools,
                                    result.responses.len()
                                );
                            }
                            let msg = UpdateMessage::QuoteUpdate {
                                request_id: req.request_id.unwrap_or_default(),
                                data: result.responses,
                                meta: result.meta,
                            };
                            if socket
                                .send(axum::extract::ws::Message::Text(
                                    serde_json::to_string(&msg).unwrap(),
                                ))
                                .await
                                .is_err()
                            {
                                error!("Failed to send quote response");
                                break;
                            }
                            debug!("Quote response sent successfully");
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

fn format_failure_summary(failure: &QuoteFailure) -> String {
    let label = render_pool_label(
        failure.protocol.as_deref(),
        failure.pool_name.as_deref(),
        failure.pool_address.as_deref(),
        failure.pool.as_deref(),
    );
    format!("{} => {}", label, failure.message)
}

fn render_pool_label(
    protocol: Option<&str>,
    name: Option<&str>,
    address: Option<&str>,
    fallback: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(protocol) = protocol {
        if !protocol.is_empty() {
            parts.push(protocol);
        }
    }
    if let Some(name) = name {
        if !name.is_empty() {
            parts.push(name);
        }
    }

    let mut label = if !parts.is_empty() {
        parts.join("::")
    } else if let Some(fallback) = fallback {
        fallback.to_string()
    } else {
        "unknown_pool".to_string()
    };

    if let Some(address) = address {
        if !address.is_empty() {
            label = format!("{} [{}]", label, address);
        }
    }

    label
}
