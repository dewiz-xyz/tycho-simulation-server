use std::time::Instant;

use axum::{extract::State, Json};
use tracing::{info, warn};

use crate::models::messages::{
    EncodeMeta, EncodeRequest, EncodeResult, QuoteFailure, QuoteFailureKind, QuoteStatus,
};
use crate::models::state::AppState;
use crate::services::encode::encode_routes;

pub async fn encode(
    State(state): State<AppState>,
    Json(request): Json<EncodeRequest>,
) -> Json<EncodeResult> {
    let started_at = Instant::now();
    let auction_id = request.auction_id.as_deref();
    let total_ladder_steps: usize = request.routes.iter().map(|route| route.amounts.len()).sum();

    info!(
        request_id = request.request_id.as_str(),
        auction_id,
        routes = request.routes.len(),
        total_ladder_steps,
        "Received encode request"
    );

    let request_timeout = state.request_timeout();
    let state_for_computation = state.clone();
    let request_for_computation = request.clone();

    let computation_future = encode_routes(state_for_computation, request_for_computation);

    let computation = match tokio::time::timeout(request_timeout, computation_future).await {
        Ok(result) => result,
        Err(_) => {
            let timeout_ms = request_timeout.as_millis() as u64;
            warn!(
                scope = "handler_timeout",
                request_id = request.request_id.as_str(),
                auction_id,
                timeout_ms,
                latency_ms = started_at.elapsed().as_millis() as u64,
                "Encode request timed out at request-level guard"
            );

            let failure = QuoteFailure {
                kind: QuoteFailureKind::Timeout,
                message: format!("Encode request timed out after {}ms", timeout_ms),
                pool: None,
                pool_name: None,
                pool_address: None,
                protocol: None,
            };

            let meta = EncodeMeta {
                status: QuoteStatus::PartialFailure,
                auction_id: request.auction_id.clone(),
                failures: vec![failure],
            };

            return Json(EncodeResult {
                request_id: request.request_id,
                data: Vec::new(),
                meta,
            });
        }
    };

    let latency_ms = started_at.elapsed().as_millis() as u64;
    if computation.meta.failures.is_empty() {
        info!(
            request_id = request.request_id.as_str(),
            auction_id,
            latency_ms,
            status = ?computation.meta.status,
            routes = computation.data.len(),
            "Encode request completed"
        );
    } else {
        warn!(
            request_id = request.request_id.as_str(),
            auction_id,
            latency_ms,
            status = ?computation.meta.status,
            routes = computation.data.len(),
            failures = computation.meta.failures.len(),
            "Encode request completed with failures"
        );
    }

    Json(computation)
}
