use std::collections::BTreeMap;
use std::time::Instant;

use axum::{extract::State, Json};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    metrics::{emit_simulate_completion, emit_simulate_timeout, TimeoutKind},
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
            let vm_block_number = state.current_vm_block().await;
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
                vm_block_number,
                matching_pools: 0,
                candidate_pools: 0,
                total_pools: Some(total_pools),
                auction_id: request.auction_id.clone(),
                failures: vec![failure],
            };

            emit_simulate_completion(QuoteStatus::PartialFailure, true);
            emit_simulate_timeout(TimeoutKind::RequestGuard);

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

    let top_response = computation.responses.first();
    let top_pool = top_response.map(|response| response.pool.as_str());
    let top_pool_name = top_response.map(|response| response.pool_name.as_str());
    let top_pool_address = top_response.map(|response| response.pool_address.as_str());
    let top_amount_out =
        top_response.and_then(|response| response.amounts_out.first().map(String::as_str));
    let top_gas_used = top_response.and_then(|response| response.gas_used.first().copied());

    let failure_summary = summarize_failures(&computation.meta.failures);

    macro_rules! log_completion {
        ($level:expr, $scope:expr, $msg:expr) => {
            tracing::event!(
                $level,
                scope = $scope,
                request_id = request.request_id.as_str(),
                auction_id,
                latency_ms,
                status = ?computation.meta.status,
                responses = computation.responses.len(),
                failures = computation.meta.failures.len(),
                scheduled_native_pools = computation.metrics.scheduled_native_pools,
                scheduled_vm_pools = computation.metrics.scheduled_vm_pools,
                simulation_runs = computation.metrics.scheduled_native_pools + computation.metrics.scheduled_vm_pools,
                skipped_vm_unavailable = computation.metrics.skipped_vm_unavailable,
                skipped_native_concurrency = computation.metrics.skipped_native_concurrency,
                skipped_vm_concurrency = computation.metrics.skipped_vm_concurrency,
                skipped_native_deadline = computation.metrics.skipped_native_deadline,
                skipped_vm_deadline = computation.metrics.skipped_vm_deadline,
                token_in = request.token_in.as_str(),
                token_out = request.token_out.as_str(),
                amounts = request.amounts.len(),
                top_pool,
                top_pool_name,
                top_pool_address,
                top_amount_out,
                top_gas_used,
                failure_kinds = ?failure_summary.kind_counts,
                failure_protocols = ?failure_summary.protocol_counts,
                failure_pool_kinds = ?failure_summary.pool_kind_counts,
                failure_samples = ?failure_summary.samples,
                $msg
            );
        };
    }

    if timed_out {
        log_completion!(
            tracing::Level::WARN,
            "handler_timeout",
            "Simulate computation completed with timeout"
        );
    } else {
        log_completion!(
            tracing::Level::INFO,
            "handler_complete",
            "Simulate computation completed"
        );
    }

    emit_simulate_completion(computation.meta.status, timed_out);

    Json(QuoteResult {
        request_id: request.request_id,
        data: computation.responses,
        meta: computation.meta,
    })
}

#[derive(Debug, Default)]
struct FailureSummary {
    kind_counts: Vec<(String, usize)>,
    protocol_counts: Vec<(String, usize)>,
    pool_kind_counts: Vec<(String, usize)>,
    samples: Vec<String>,
}

// Aggregate failure details for a single summary log entry per request.
fn summarize_failures(failures: &[QuoteFailure]) -> FailureSummary {
    if failures.is_empty() {
        return FailureSummary::default();
    }

    let mut kind_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut protocol_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut pool_kind_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples = Vec::new();

    for failure in failures {
        let kind_label = failure.kind.label();
        *kind_counts.entry(kind_label.to_string()).or_insert(0) += 1;

        if let Some(protocol) = failure.protocol.as_deref() {
            *protocol_counts.entry(protocol.to_string()).or_insert(0) += 1;
            let pool_kind = if protocol.starts_with("vm:") {
                "vm"
            } else {
                "native"
            };
            *pool_kind_counts.entry(pool_kind.to_string()).or_insert(0) += 1;
        } else {
            *pool_kind_counts.entry("unknown".to_string()).or_insert(0) += 1;
        }

        if samples.len() < 5 {
            let sample = format!(
                "kind={} protocol={:?} pool={:?} pool_name={:?} pool_address={:?} message={}",
                kind_label,
                failure.protocol,
                failure.pool,
                failure.pool_name,
                failure.pool_address,
                failure.message
            );
            samples.push(sample);
        }
    }

    FailureSummary {
        kind_counts: kind_counts.into_iter().collect(),
        protocol_counts: protocol_counts.into_iter().collect(),
        pool_kind_counts: pool_kind_counts.into_iter().collect(),
        samples,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::messages::{QuoteFailure, QuoteFailureKind};

    fn make_failure(kind: QuoteFailureKind, protocol: Option<&str>) -> QuoteFailure {
        QuoteFailure {
            kind,
            message: format!("{} error", kind.label()),
            pool: Some("pool_id".to_string()),
            pool_name: Some("pool_name".to_string()),
            pool_address: Some("0xabc".to_string()),
            protocol: protocol.map(String::from),
        }
    }

    #[test]
    fn empty_failures_returns_default() {
        let summary = summarize_failures(&[]);
        assert!(summary.kind_counts.is_empty());
        assert!(summary.protocol_counts.is_empty());
        assert!(summary.pool_kind_counts.is_empty());
        assert!(summary.samples.is_empty());
    }

    #[test]
    fn single_failure_without_protocol() {
        let failures = vec![make_failure(QuoteFailureKind::Timeout, None)];
        let summary = summarize_failures(&failures);

        assert_eq!(summary.kind_counts, vec![("timeout".to_string(), 1)]);
        assert!(summary.protocol_counts.is_empty());
        assert_eq!(summary.pool_kind_counts, vec![("unknown".to_string(), 1)]);
        assert_eq!(summary.samples.len(), 1);
    }

    #[test]
    fn mixed_protocols_counted_correctly() {
        let failures = vec![
            make_failure(QuoteFailureKind::Simulator, Some("vm:uniswap_v2")),
            make_failure(QuoteFailureKind::Simulator, Some("vm:uniswap_v2")),
            make_failure(QuoteFailureKind::Timeout, Some("balancer_v2")),
            make_failure(QuoteFailureKind::Overflow, Some("balancer_v2")),
        ];
        let summary = summarize_failures(&failures);

        assert_eq!(
            summary.kind_counts,
            vec![
                ("overflow".to_string(), 1),
                ("simulator".to_string(), 2),
                ("timeout".to_string(), 1),
            ]
        );
        assert_eq!(
            summary.protocol_counts,
            vec![
                ("balancer_v2".to_string(), 2),
                ("vm:uniswap_v2".to_string(), 2),
            ]
        );
        assert_eq!(
            summary.pool_kind_counts,
            vec![("native".to_string(), 2), ("vm".to_string(), 2)]
        );
        assert_eq!(summary.samples.len(), 4);
    }

    #[test]
    fn samples_capped_at_five() {
        let failures: Vec<_> = (0..8)
            .map(|_| make_failure(QuoteFailureKind::Simulator, Some("uniswap_v3")))
            .collect();
        let summary = summarize_failures(&failures);

        assert_eq!(summary.kind_counts, vec![("simulator".to_string(), 8)]);
        assert_eq!(summary.samples.len(), 5);
    }

    #[test]
    fn counts_are_alphabetically_sorted() {
        let failures = vec![
            make_failure(QuoteFailureKind::Overflow, Some("vm:curve")),
            make_failure(QuoteFailureKind::Simulator, Some("aave_v3")),
            make_failure(QuoteFailureKind::Timeout, Some("balancer_v2")),
        ];
        let summary = summarize_failures(&failures);

        let kind_keys: Vec<_> = summary.kind_counts.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(kind_keys, vec!["overflow", "simulator", "timeout"]);

        let protocol_keys: Vec<_> = summary
            .protocol_counts
            .iter()
            .map(|(k, _)| k.clone())
            .collect();
        assert_eq!(protocol_keys, vec!["aave_v3", "balancer_v2", "vm:curve"]);
    }
}
