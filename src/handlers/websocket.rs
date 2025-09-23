use axum::extract::{
    ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    State,
};
use axum::response::IntoResponse;
use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::models::{
    messages::{ClientMessage, QuoteFailure, UpdateMessage},
    state::AppState,
};
use crate::services::quotes::get_amounts_out;

struct PendingRequest {
    handle: JoinHandle<()>,
    auction_id: Option<String>,
}

struct AuctionCancellationTable {
    cancellation_ttl: Duration,
    pending_by_request: HashMap<String, PendingRequest>,
    pending_by_auction: HashMap<String, HashSet<String>>, // auction_id -> {request_ids}
    canceled_auctions: HashMap<String, Instant>,          // auction_id -> expires_at
    tokens_by_auction: HashMap<String, CancellationToken>,
}

impl AuctionCancellationTable {
    fn new(ttl: Duration) -> Self {
        Self {
            cancellation_ttl: ttl,
            pending_by_request: HashMap::new(),
            pending_by_auction: HashMap::new(),
            canceled_auctions: HashMap::new(),
            tokens_by_auction: HashMap::new(),
        }
    }

    fn ensure_token(&mut self, auction_id: &str) -> CancellationToken {
        // Prune any expired cancellations so we don't keep reusing a cancelled token
        self.prune_expired();

        match self.tokens_by_auction.entry(auction_id.to_string()) {
            Entry::Occupied(mut occ) => {
                // If the token is cancelled but the auction is no longer under an active
                // cancellation window, replace it with a fresh token.
                let token_is_cancelled = occ.get().is_cancelled();
                let still_cancelled = self.canceled_auctions.contains_key(auction_id);
                if token_is_cancelled && !still_cancelled {
                    let fresh = CancellationToken::new();
                    occ.insert(fresh.clone());
                    fresh
                } else {
                    occ.get().clone()
                }
            }
            Entry::Vacant(vac) => vac.insert(CancellationToken::new()).clone(),
        }
    }

    fn register(&mut self, request_id: String, auction_id: Option<String>, handle: JoinHandle<()>) {
        if let Some(aid) = auction_id.as_ref() {
            self.pending_by_auction
                .entry(aid.clone())
                .or_default()
                .insert(request_id.clone());
            // Ensure a token exists for this auction_id
            self.ensure_token(aid);
        }
        self.pending_by_request
            .insert(request_id, PendingRequest { handle, auction_id });
    }

    fn finish_request(&mut self, request_id: &str) {
        if let Some(pending) = self.pending_by_request.remove(request_id) {
            if let Some(aid) = pending.auction_id {
                if let Some(set) = self.pending_by_auction.get_mut(&aid) {
                    set.remove(request_id);
                    if set.is_empty() {
                        self.pending_by_auction.remove(&aid);
                        if !self.canceled_auctions.contains_key(&aid) {
                            self.tokens_by_auction.remove(&aid);
                        }
                    }
                }
            }
        }
    }

    fn is_cancelled(&mut self, auction_id: Option<&str>) -> bool {
        self.prune_expired();
        if let Some(aid) = auction_id {
            if let Some(expires_at) = self.canceled_auctions.get(aid) {
                return Instant::now() <= *expires_at;
            }
        }
        false
    }

    fn cancel_auction(&mut self, auction_id: &str) -> usize {
        // Mark canceled
        let expires_at = Instant::now() + self.cancellation_ttl;
        self.canceled_auctions
            .insert(auction_id.to_string(), expires_at);

        // Cancel token if exists
        if let Some(token) = self.tokens_by_auction.get(auction_id) {
            token.cancel();
        }

        // Abort all pending requests for this auction
        let mut canceled_count = 0usize;
        if let Some(requests) = self.pending_by_auction.remove(auction_id) {
            for req_id in requests {
                if let Some(pending) = self.pending_by_request.remove(&req_id) {
                    pending.handle.abort();
                    canceled_count += 1;
                }
            }
        }
        canceled_count
    }

    fn prune_expired(&mut self) {
        let now = Instant::now();
        // Collect expired auction_ids first
        let mut to_remove: Vec<String> = Vec::new();
        for (aid, expires_at) in self.canceled_auctions.iter() {
            if *expires_at <= now {
                to_remove.push(aid.clone());
            }
        }
        // Remove expired cancellations and reset their tokens so future requests get a fresh token
        for aid in to_remove {
            self.canceled_auctions.remove(&aid);
            self.tokens_by_auction.remove(&aid);
        }
    }

    fn cancel_all(&mut self) {
        for (_, token) in self.tokens_by_auction.iter() {
            token.cancel();
        }
        for (_, pending) in self.pending_by_request.drain() {
            pending.handle.abort();
        }
        self.pending_by_auction.clear();
        self.canceled_auctions.clear();
    }
}

pub async fn handle_ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws_connection(socket, state))
}

async fn handle_ws_connection(mut socket: WebSocket, app_state: AppState) {
    debug!("New WebSocket connection established");
    let mut rx = app_state.update_tx.subscribe();
    let (task_tx, mut task_rx) = mpsc::unbounded_channel::<UpdateMessage>();
    let mut cancel_table = AuctionCancellationTable::new(app_state.cancellation_ttl());

    // Send initial state
    let block = app_state.current_block().await;
    let pool_count = app_state.total_pools().await;
    debug!(
        "Sending initial state: block={}, states={}",
        block, pool_count
    );

    if socket
        .send(WsMessage::Text(
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
                    Some(Ok(WsMessage::Text(text))) => {
                        // Parse only the new tagged protocol; legacy payloads are no longer accepted
                        let parsed = serde_json::from_str::<ClientMessage>(&text);

                        match parsed {
                            Ok(ClientMessage::QuoteAmountOut(req)) => {
                                info!("Quote request: {} -> {} ({} amounts)",
                                    req.token_in,
                                    req.token_out,
                                    req.amounts.len()
                                );

                                let request_id = req.request_id.clone();
                                let auction_id = req.auction_id.clone();
                                let task_tx_cloned = task_tx.clone();
                                let app_state_cloned = app_state.clone();

                                let cancel_token = if app_state.auction_cancellation_enabled() {
                                    auction_id.as_deref().map(|aid| cancel_table.ensure_token(aid))
                                } else {
                                    None
                                };

                                // Spawn computation as a task
                                let req_id_for_task = request_id.clone();
                                let handle = tokio::spawn(async move {
                                    let result = get_amounts_out(app_state_cloned, req, cancel_token).await;
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
                                        request_id: req_id_for_task,
                                        data: result.responses,
                                        meta: result.meta,
                                    };
                                    let _ = task_tx_cloned.send(msg);
                                });

                                cancel_table.register(request_id, auction_id, handle);
                            }
                            Ok(ClientMessage::CancelAuctionRequests { auction_id }) => {
                                if app_state.auction_cancellation_enabled() {
                                    let canceled_count = cancel_table.cancel_auction(&auction_id);
                                    info!("CancelAuctionRequests: auction_id={} canceled_count={}", auction_id, canceled_count);
                                    let ack = UpdateMessage::CancelAck { auction_id, canceled_count };
                                    if socket.send(WsMessage::Text(serde_json::to_string(&ack).unwrap())).await.is_err() {
                                        error!("Failed to send CancelAck to client");
                                        break;
                                    }
                                } else {
                                    warn!("Received CancelAuctionRequests but cancellation feature is disabled");
                                }
                            }
                            Err(e) => {
                                warn!("Failed to parse client message: {}", e);
                            }
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) => {
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
                if socket.send(WsMessage::Text(
                    serde_json::to_string(&msg).unwrap()
                )).await.is_err() {
                    error!("Failed to send broadcast message to client");
                    break;
                }
            }
            Some(msg) = task_rx.recv() => {
                if let UpdateMessage::QuoteUpdate { request_id, meta, .. } = &msg {
                    cancel_table.finish_request(request_id);
                    let cancelled = app_state.auction_cancellation_enabled() && cancel_table.is_cancelled(meta.auction_id.as_deref());
                    if cancelled {
                        debug!("Dropping late QuoteUpdate due to auction cancel");
                        continue;
                    }
                }

                if socket.send(WsMessage::Text(serde_json::to_string(&msg).unwrap())).await.is_err() {
                    error!("Failed to send task message to client");
                    break;
                }
            }
        }
    }

    // Best-effort cleanup
    cancel_table.cancel_all();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_idempotent_and_ttl() {
        let mut table = AuctionCancellationTable::new(Duration::from_millis(50));

        // Register two requests under same auction
        let (h1_tx, mut h1_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let handle1 = tokio::spawn(async move {
            let _ = h1_rx.recv().await;
        });
        table.register("r1".into(), Some("a1".into()), handle1);

        let (h2_tx, mut h2_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let handle2 = tokio::spawn(async move {
            let _ = h2_rx.recv().await;
        });
        table.register("r2".into(), Some("a1".into()), handle2);

        let count = table.cancel_auction("a1");
        assert_eq!(count, 2);

        // Second cancel should be idempotent (no pending left)
        let count2 = table.cancel_auction("a1");
        assert_eq!(count2, 0);

        // During TTL, reports cancelled
        assert!(table.is_cancelled(Some("a1")));

        // After TTL, no longer cancelled
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(!table.is_cancelled(Some("a1")));

        // Avoid unused var warnings
        let _ = h1_tx;
        let _ = h2_tx;
    }

    #[tokio::test]
    async fn token_resets_after_ttl() {
        let mut table = AuctionCancellationTable::new(Duration::from_millis(40));

        // Before cancel, token is not cancelled
        let t0 = table.ensure_token("a2");
        assert!(!t0.is_cancelled());

        // Cancel auction; token should be cancelled for duration of TTL
        table.cancel_auction("a2");
        let t1 = table.ensure_token("a2");
        assert!(t1.is_cancelled());

        // After TTL, ensure_token should recreate a fresh token without needing
        // an explicit is_cancelled() probe
        tokio::time::sleep(Duration::from_millis(60)).await;
        let t2 = table.ensure_token("a2");
        assert!(!t2.is_cancelled());
    }

    #[tokio::test]
    async fn tokens_dropped_after_successful_requests() {
        let mut table = AuctionCancellationTable::new(Duration::from_millis(50));

        let handle = tokio::spawn(async {});
        table.register("req-1".into(), Some("auction-1".into()), handle);
        assert!(table.tokens_by_auction.contains_key("auction-1"));

        table.finish_request("req-1");

        assert!(!table.pending_by_auction.contains_key("auction-1"));
        assert!(!table.tokens_by_auction.contains_key("auction-1"));
    }
}
