use std::time::Instant;

use axum::{extract::State, Json};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn, debug};

use crate::{
    models::{
        messages::{
            AmountOutRequest, QuoteFailure, QuoteFailureKind, QuoteMeta, QuoteResult, QuoteStatus,
        },
        state::AppState,
    },
    services::quotes::get_amounts_out,
};

pub async fn simulate(
    State(state): State<AppState>,
    Json(request): Json<AmountOutRequest>,
) -> Json<QuoteResult> {
    let started_at = Instant::now();
    let auction_id = request.auction_id.as_deref();

    debug!(
        request_id = request.request_id.as_str(),
        auction_id,
        token_in = request.token_in.as_str(),
        token_out = request.token_out.as_str(),
        amounts = request.amounts.len(),
        "Received simulate request"
    );

    let cancel_token = CancellationToken::new();
    let mut cancel_guard = CancelOnDrop::new(cancel_token.clone());
    let request_timeout = state.request_timeout();

    let state_for_computation = state.clone();
    let request_for_computation = request.clone();
    let computation_future = get_amounts_out(
        state_for_computation,
        request_for_computation,
        Some(cancel_token.clone()),
    );

    let computation = match tokio::time::timeout(request_timeout, computation_future).await {
        Ok(result) => {
            cancel_guard.disarm();
            result
        }
        Err(_) => {
            // Rely on CancelOnDrop to cancel outstanding work
            let block_number = state.current_block().await;
            let total_pools = state.total_pools().await;
            let timeout_ms = request_timeout.as_millis() as u64;

            warn!(
                scope = "handler_timeout",
                request_id = request.request_id.as_str(),
                auction_id,
                latency_ms = started_at.elapsed().as_millis() as u64,
                timeout_ms,
                "Simulate request timed out at request-level guard"
            );

            let failure = QuoteFailure {
                kind: QuoteFailureKind::Timeout,
                message: format!("Simulate request timed out after {}ms", timeout_ms),
                pool: None,
                pool_name: None,
                pool_address: None,
                protocol: None,
            };

            let meta = QuoteMeta {
                status: QuoteStatus::PartialFailure,
                block_number,
                matching_pools: 0,
                candidate_pools: 0,
                total_pools: Some(total_pools),
                auction_id: request.auction_id.clone(),
                failures: vec![failure],
            };

            return Json(QuoteResult {
                request_id: request.request_id,
                data: Vec::new(),
                meta,
            });
        }
    };

    let timed_out = computation
        .meta
        .failures
        .iter()
        .any(|failure| matches!(failure.kind, QuoteFailureKind::Timeout));

    let latency_ms = started_at.elapsed().as_millis() as u64;

    if timed_out {
        warn!(
            scope = "handler_timeout",
            request_id = request.request_id.as_str(),
            auction_id,
            latency_ms,
            status = ?computation.meta.status,
            responses = computation.responses.len(),
            failures = computation.meta.failures.len(),
            scheduled_native_pools = computation.metrics.scheduled_native_pools,
            scheduled_vm_pools = computation.metrics.scheduled_vm_pools,
            skipped_native_concurrency = computation.metrics.skipped_native_concurrency,
            skipped_vm_concurrency = computation.metrics.skipped_vm_concurrency,
            skipped_native_deadline = computation.metrics.skipped_native_deadline,
            skipped_vm_deadline = computation.metrics.skipped_vm_deadline,
            "Simulate computation completed with timeout"
        );
    } else {
        debug!(
            scope = "handler_timeout",
            request_id = request.request_id.as_str(),
            auction_id,
            latency_ms,
            status = ?computation.meta.status,
            responses = computation.responses.len(),
            failures = computation.meta.failures.len(),
            scheduled_native_pools = computation.metrics.scheduled_native_pools,
            scheduled_vm_pools = computation.metrics.scheduled_vm_pools,
            skipped_native_concurrency = computation.metrics.skipped_native_concurrency,
            skipped_vm_concurrency = computation.metrics.skipped_vm_concurrency,
            skipped_native_deadline = computation.metrics.skipped_native_deadline,
            skipped_vm_deadline = computation.metrics.skipped_vm_deadline,
            "Simulate computation completed"
        );
    }

    Json(QuoteResult {
        request_id: request.request_id,
        data: computation.responses,
        meta: computation.meta,
    })
}

struct CancelOnDrop {
    token: CancellationToken,
    armed: bool,
}

impl CancelOnDrop {
    fn new(token: CancellationToken) -> Self {
        Self { token, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.token.cancel();
        }
    }
}

// Tests intentionally omitted in this branch per plan.
