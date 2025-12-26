use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use num_bigint::BigUint;
use num_traits::cast::ToPrimitive;
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::spawn_blocking;
use tokio::time::{sleep_until, Instant as TokioInstant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use tycho_simulation::{
    protocol::models::ProtocolComponent,
    tycho_common::{models::token::Token, simulation::protocol_sim::ProtocolSim, Bytes},
};

use crate::models::messages::{
    AmountOutRequest, AmountOutResponse, QuoteFailure, QuoteFailureKind, QuoteMeta, QuoteStatus,
};
use crate::models::state::AppState;
use crate::models::tokens::TokenStoreError;

pub struct QuoteComputation {
    pub responses: Vec<AmountOutResponse>,
    pub meta: QuoteMeta,
}

pub async fn get_amounts_out(
    state: AppState,
    request: AmountOutRequest,
    cancel: Option<CancellationToken>,
) -> QuoteComputation {
    let mut responses = Vec::new();
    let mut failures = Vec::new();

    let readiness_wait = Duration::from_secs(2);
    let quote_timeout = state.quote_timeout();

    let mut current_block = state.current_block().await;
    let mut total_pools = state.total_pools().await;

    let mut meta = QuoteMeta {
        status: QuoteStatus::Ready,
        block_number: current_block,
        matching_pools: 0,
        candidate_pools: 0,
        total_pools: Some(total_pools),
        auction_id: request.auction_id.clone(),
        failures: Vec::new(),
    };

    if cancel
        .as_ref()
        .map(|token| token.is_cancelled())
        .unwrap_or(false)
    {
        meta.status = QuoteStatus::PartialFailure;
        return QuoteComputation { responses, meta };
    }

    let token_in_address = request.token_in.trim_start_matches("0x").to_lowercase();
    let token_out_address = request.token_out.trim_start_matches("0x").to_lowercase();

    let token_in_bytes = match Bytes::from_str(&token_in_address) {
        Ok(bytes) => bytes,
        Err(e) => {
            failures.push(make_failure(
                QuoteFailureKind::TokenValidation,
                format!("Invalid token_in address: {}", e),
                None,
            ));
            meta.status = QuoteStatus::InvalidRequest;
            meta.failures = failures;
            return QuoteComputation { responses, meta };
        }
    };

    let token_out_bytes = match Bytes::from_str(&token_out_address) {
        Ok(bytes) => bytes,
        Err(e) => {
            failures.push(make_failure(
                QuoteFailureKind::TokenValidation,
                format!("Invalid token_out address: {}", e),
                None,
            ));
            meta.status = QuoteStatus::InvalidRequest;
            meta.failures = failures;
            return QuoteComputation { responses, meta };
        }
    };

    if !state.is_ready() {
        let ready = state.wait_for_readiness(readiness_wait).await;
        if !ready {
            failures.push(make_failure(
                QuoteFailureKind::WarmUp,
                format!(
                    "Service warming up: block={}, pools={}",
                    current_block, total_pools
                ),
                None,
            ));
            meta.status = QuoteStatus::WarmingUp;
            meta.failures = failures;
            return QuoteComputation { responses, meta };
        }
        current_block = state.current_block().await;
        total_pools = state.total_pools().await;
        meta.block_number = current_block;
        meta.total_pools = Some(total_pools);
    }

    // Concurrently fetch token_in and token_out metadata
    let (token_in_res, token_out_res) = tokio::join!(
        state.tokens.ensure(&token_in_bytes),
        state.tokens.ensure(&token_out_bytes)
    );

    let token_in_ref = match token_in_res {
        Ok(Some(token)) => token,
        Ok(None) => {
            failures.push(make_failure(
                QuoteFailureKind::TokenCoverage,
                format!("Token not found: {}", token_in_address),
                None,
            ));
            meta.status = QuoteStatus::TokenMissing;
            meta.failures = failures;
            return QuoteComputation { responses, meta };
        }
        Err(TokenStoreError::FetchTimeout(duration)) => {
            let timeout_ms = duration.as_millis() as u64;
            warn!(
                request_id = request.request_id.as_str(),
                auction_id = request.auction_id.as_deref(),
                token = ?token_in_address,
                timeout_ms,
                "Token metadata fetch timed out"
            );
            failures.push(make_failure(
                QuoteFailureKind::TokenCoverage,
                format!(
                    "Token metadata fetch timed out after {}ms: {}",
                    timeout_ms, token_in_address
                ),
                None,
            ));
            meta.status = QuoteStatus::TokenMissing;
            meta.failures = failures;
            return QuoteComputation { responses, meta };
        }
        Err(TokenStoreError::RequestFailed(message)) => {
            warn!(
                request_id = request.request_id.as_str(),
                auction_id = request.auction_id.as_deref(),
                token = ?token_in_address,
                error = %message,
                "Token metadata fetch failed"
            );
            failures.push(make_failure(
                QuoteFailureKind::TokenCoverage,
                format!("Token metadata fetch failed: {}", message),
                None,
            ));
            meta.status = QuoteStatus::TokenMissing;
            meta.failures = failures;
            return QuoteComputation { responses, meta };
        }
    };

    let token_out_ref = match token_out_res {
        Ok(Some(token)) => token,
        Ok(None) => {
            failures.push(make_failure(
                QuoteFailureKind::TokenCoverage,
                format!("Token not found: {}", token_out_address),
                None,
            ));
            meta.status = QuoteStatus::TokenMissing;
            meta.failures = failures;
            return QuoteComputation { responses, meta };
        }
        Err(TokenStoreError::FetchTimeout(duration)) => {
            let timeout_ms = duration.as_millis() as u64;
            warn!(
                request_id = request.request_id.as_str(),
                auction_id = request.auction_id.as_deref(),
                token = ?token_out_address,
                timeout_ms,
                "Token metadata fetch timed out"
            );
            failures.push(make_failure(
                QuoteFailureKind::TokenCoverage,
                format!(
                    "Token metadata fetch timed out after {}ms: {}",
                    timeout_ms, token_out_address
                ),
                None,
            ));
            meta.status = QuoteStatus::TokenMissing;
            meta.failures = failures;
            return QuoteComputation { responses, meta };
        }
        Err(TokenStoreError::RequestFailed(message)) => {
            warn!(
                request_id = request.request_id.as_str(),
                auction_id = request.auction_id.as_deref(),
                token = ?token_out_address,
                error = %message,
                "Token metadata fetch failed"
            );
            failures.push(make_failure(
                QuoteFailureKind::TokenCoverage,
                format!("Token metadata fetch failed: {}", message),
                None,
            ));
            meta.status = QuoteStatus::TokenMissing;
            meta.failures = failures;
            return QuoteComputation { responses, meta };
        }
    };

    info!(
        "Processing quote: {} ({}) -> {} ({})",
        token_in_ref.symbol, token_in_address, token_out_ref.symbol, token_out_address
    );

    let amounts_in: Vec<BigUint> = match request
        .amounts
        .iter()
        .map(|amount| BigUint::from_str(amount))
        .collect()
    {
        Ok(vec) => vec,
        Err(e) => {
            failures.push(make_failure(
                QuoteFailureKind::InvalidRequest,
                format!("Invalid amount: {}", e),
                None,
            ));
            meta.status = QuoteStatus::InvalidRequest;
            meta.failures = failures;
            return QuoteComputation { responses, meta };
        }
    };

    let amounts_in = Arc::new(amounts_in);
    let quote_deadline = Instant::now() + quote_timeout;

    let candidates_raw = state
        .state_store
        .matching_pools_by_addresses(&token_in_bytes, &token_out_bytes)
        .await;

    let mut native_candidates = Vec::new();
    let mut vm_candidates = Vec::new();
    for (id, (pool_state, component)) in candidates_raw.into_iter() {
        if component.protocol_system.starts_with("vm:") {
            vm_candidates.push((id, pool_state, component));
        } else {
            native_candidates.push((id, pool_state, component));
        }
    }

    let total_candidates = native_candidates.len() + vm_candidates.len();
    meta.matching_pools = total_candidates;
    meta.candidate_pools = total_candidates;

    // Reserve capacity for failures to avoid repeated reallocations under heavy error scenarios
    failures.reserve(total_candidates);

    if total_candidates == 0 {
        failures.push(make_failure(
            QuoteFailureKind::NoPools,
            format!(
                "No matching pools found for pair {}-{}",
                token_in_address, token_out_address
            ),
            None,
        ));
        meta.status = QuoteStatus::NoLiquidity;
        meta.failures = failures;
        return QuoteComputation { responses, meta };
    }

    info!(
        "Quote candidates prepared: matching_pools={} amounts_per_pool={}, {} ({}) -> {} ({})",
        meta.matching_pools,
        amounts_in.len(),
        token_in_ref.symbol,
        token_in_address,
        token_out_ref.symbol,
        token_out_address
    );

    let token_in = Arc::new(token_in_ref);
    let token_out = Arc::new(token_out_ref);
    let expected_len = amounts_in.len();
    let metrics = state.metrics();
    let mut skipped_native = 0u64;
    let mut skipped_vm = 0u64;
    let mut pool_timeout_native = 0u64;
    let mut pool_timeout_vm = 0u64;
    let mut quote_timed_out = false;
    let mut quote_deadline_reached = false;
    let mut native_queue_delay = TimingStats::default();
    let mut vm_queue_delay = TimingStats::default();
    let mut native_compute = TimingStats::default();
    let mut vm_compute = TimingStats::default();

    let cancel_token = cancel
        .as_ref()
        .map(CancellationToken::child_token)
        .unwrap_or_default();
    let mut tasks = FuturesUnordered::new();
    let native_semaphore = state.native_sim_semaphore();
    let vm_semaphore = state.vm_sim_semaphore();
    for (id, pool_state, component) in native_candidates.into_iter() {
        if cancel_token.is_cancelled() {
            break;
        }
        if Instant::now() >= quote_deadline {
            quote_deadline_reached = true;
            break;
        }

        let permit = match native_semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                skipped_native += 1;
                metrics.record_skip(false);
                continue;
            }
        };

        let scheduled_at = Instant::now();
        let pool_address = component.id.to_string();
        let pool_protocol = component.protocol_system.clone();
        let pool_name = derive_pool_name(&component);
        let timeout_for_pool = state.pool_timeout_native();
        tasks.push(simulate_pool(
            id,
            pool_state,
            pool_address,
            pool_name,
            pool_protocol,
            Arc::clone(&token_in),
            Arc::clone(&token_out),
            Arc::clone(&amounts_in),
            expected_len,
            timeout_for_pool,
            scheduled_at,
            cancel_token.clone(),
            permit,
            false,
        ));
    }

    if !quote_deadline_reached {
        for (id, pool_state, component) in vm_candidates.into_iter() {
            if cancel_token.is_cancelled() {
                break;
            }
            if Instant::now() >= quote_deadline {
                quote_deadline_reached = true;
                break;
            }

            let permit = match vm_semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    skipped_vm += 1;
                    metrics.record_skip(true);
                    continue;
                }
            };

            let scheduled_at = Instant::now();
            let pool_address = component.id.to_string();
            let pool_protocol = component.protocol_system.clone();
            let pool_name = derive_pool_name(&component);
            let timeout_for_pool = state.pool_timeout_vm();
            tasks.push(simulate_pool(
                id,
                pool_state,
                pool_address,
                pool_name,
                pool_protocol,
                Arc::clone(&token_in),
                Arc::clone(&token_out),
                Arc::clone(&amounts_in),
                expected_len,
                timeout_for_pool,
                scheduled_at,
                cancel_token.clone(),
                permit,
                true,
            ));
        }
    }

    if !tasks.is_empty() {
        let quote_timeout_sleep = sleep_until(TokioInstant::from_std(quote_deadline));
        tokio::pin!(quote_timeout_sleep);

        while !tasks.is_empty() {
            tokio::select! {
                _ = &mut quote_timeout_sleep => {
                    cancel_token.cancel();
                    let message = format!(
                        "Quote computation timed out after {}ms",
                        quote_timeout.as_millis()
                    );
                    failures.push(make_failure(QuoteFailureKind::Timeout, message, None));
                    quote_timed_out = true;
                    metrics.record_quote_timeout();
                    meta.status = QuoteStatus::PartialFailure;
                    break;
                }
                _ = cancel_token.cancelled() => {
                    meta.status = QuoteStatus::PartialFailure;
                    break;
                }
                maybe_outcome = tasks.next() => {
                    match maybe_outcome {
                        Some(outcome) => {
                            match outcome {
                                Ok(result) => {
                                    if result.timed_out {
                                        if result.is_vm {
                                            pool_timeout_vm += 1;
                                            metrics.record_pool_timeout(true);
                                        } else {
                                            pool_timeout_native += 1;
                                            metrics.record_pool_timeout(false);
                                        }
                                    }
                                    if result.is_vm {
                                        vm_queue_delay.record(result.queue_delay_ms);
                                        vm_compute.record(result.compute_ms);
                                    } else {
                                        native_queue_delay.record(result.queue_delay_ms);
                                        native_compute.record(result.compute_ms);
                                    }
                                    // Classify before moving fields to avoid clones
                                    let had_timeout = result.timed_out;
                                    let is_partial = result.amounts_out.len() < expected_len;
                                    if !result.errors.is_empty() || is_partial {
                                        let context = FailureContext {
                                            pool_id: &result.pool,
                                            pool_name: Some(&result.pool_name),
                                            pool_address: Some(&result.pool_address),
                                            protocol: Some(&result.protocol),
                                        };
                                        let descriptor = format_pool_descriptor(&context);
                                        let message = if had_timeout {
                                            format!(
                                                "{}: Quote computation timed out (partial ladder {} of {} steps)",
                                                descriptor,
                                                result.amounts_out.len(),
                                                expected_len
                                            )
                                        } else if result.amounts_out.is_empty() {
                                            let base_error = result
                                                .errors
                                                .first()
                                                .cloned()
                                                .unwrap_or_else(|| "Pool returned no quotes".to_string());
                                            format!("{}: {}", descriptor, base_error)
                                        } else {
                                            format!(
                                                "{} produced partial ladder ({} of {} steps)",
                                                descriptor,
                                                result.amounts_out.len(),
                                                expected_len
                                            )
                                        };
                                        let kind = if had_timeout {
                                            QuoteFailureKind::Timeout
                                        } else if result.amounts_out.is_empty() {
                                            classify_failure(&message, true)
                                        } else {
                                            QuoteFailureKind::InconsistentResult
                                        };
                                        failures.push(make_failure(kind, message, Some(context)));
                                    }
                                    if !result.amounts_out.is_empty() {
                                        responses.push(AmountOutResponse {
                                            pool: result.pool,
                                            pool_name: result.pool_name,
                                            pool_address: result.pool_address,
                                            amounts_out: result.amounts_out,
                                            gas_used: result.gas_used,
                                            block_number: current_block,
                                        });
                                    }
                                }
                                Err(failure) => {
                                    if matches!(failure.kind, QuoteFailureKind::Internal) {
                                        meta.status = QuoteStatus::InternalError;
                                    }
                                    failures.push(failure);
                                }
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    }

    drop(tasks);

    if quote_deadline_reached && !quote_timed_out {
        let message = format!(
            "Quote scheduling stopped at deadline after {}ms",
            quote_timeout.as_millis()
        );
        failures.push(make_failure(QuoteFailureKind::Timeout, message, None));
        if matches!(meta.status, QuoteStatus::Ready) {
            meta.status = QuoteStatus::PartialFailure;
        }
    }

    if responses.is_empty() {
        if matches!(meta.status, QuoteStatus::Ready) {
            meta.status = QuoteStatus::PartialFailure;
        }
        if failures.is_empty() {
            let skipped_total = skipped_native + skipped_vm;
            if skipped_total > 0 {
                failures.push(make_failure(
                    QuoteFailureKind::Timeout,
                    format!(
                        "All pools skipped due to saturation (native_skipped={}, vm_skipped={})",
                        skipped_native, skipped_vm
                    ),
                    None,
                ));
            } else {
                failures.push(make_failure(
                    QuoteFailureKind::Simulator,
                    "All pools returned zero amounts".to_string(),
                    None,
                ));
            }
        }
    } else {
        responses.sort_by(|a, b| {
            let a_amount = a
                .amounts_out
                .first()
                .and_then(|v| BigUint::from_str(v).ok())
                .unwrap_or_default();
            let b_amount = b
                .amounts_out
                .first()
                .and_then(|v| BigUint::from_str(v).ok())
                .unwrap_or_default();
            b_amount.cmp(&a_amount)
        });

        let top = &responses[0];
        info!(
            "Quote response: total_results={} top_pool={} address={} first_amount_out={} block={}",
            responses.len(),
            top.pool_name,
            top.pool_address,
            top.amounts_out
                .first()
                .cloned()
                .unwrap_or_else(|| "0".to_string()),
            top.block_number
        );

        if !failures.is_empty() && matches!(meta.status, QuoteStatus::Ready) {
            meta.status = QuoteStatus::PartialFailure;
        }
    }

    if (skipped_native + skipped_vm) > 0 && matches!(meta.status, QuoteStatus::Ready) {
        meta.status = QuoteStatus::PartialFailure;
    }

    let native_queue_summary = native_queue_delay.summarize();
    let vm_queue_summary = vm_queue_delay.summarize();
    let native_compute_summary = native_compute.summarize();
    let vm_compute_summary = vm_compute.summarize();
    let native_in_flight = state
        .native_sim_concurrency()
        .saturating_sub(native_semaphore.available_permits());
    let vm_in_flight = state
        .vm_sim_concurrency()
        .saturating_sub(vm_semaphore.available_permits());
    let metrics_snapshot = metrics.snapshot();

    info!(
        scope = "quote_metrics",
        request_id = request.request_id.as_str(),
        auction_id = request.auction_id.as_deref(),
        status = ?meta.status,
        responses = responses.len(),
        failures = failures.len(),
        skipped_native,
        skipped_vm,
        pool_timeout_native,
        pool_timeout_vm,
        quote_timeout = quote_timed_out,
        quote_deadline_reached,
        native_in_flight,
        vm_in_flight,
        native_queue_count = native_queue_summary.as_ref().map(|summary| summary.count).unwrap_or(0),
        native_queue_p50_ms = native_queue_summary.as_ref().map(|summary| summary.p50).unwrap_or(0),
        native_queue_p95_ms = native_queue_summary.as_ref().map(|summary| summary.p95).unwrap_or(0),
        native_queue_min_ms = native_queue_summary.as_ref().map(|summary| summary.min).unwrap_or(0),
        native_queue_max_ms = native_queue_summary.as_ref().map(|summary| summary.max).unwrap_or(0),
        native_queue_avg_ms = native_queue_summary.as_ref().map(|summary| summary.avg).unwrap_or(0),
        native_compute_count = native_compute_summary.as_ref().map(|summary| summary.count).unwrap_or(0),
        native_compute_p50_ms = native_compute_summary.as_ref().map(|summary| summary.p50).unwrap_or(0),
        native_compute_p95_ms = native_compute_summary.as_ref().map(|summary| summary.p95).unwrap_or(0),
        native_compute_min_ms = native_compute_summary.as_ref().map(|summary| summary.min).unwrap_or(0),
        native_compute_max_ms = native_compute_summary.as_ref().map(|summary| summary.max).unwrap_or(0),
        native_compute_avg_ms = native_compute_summary.as_ref().map(|summary| summary.avg).unwrap_or(0),
        vm_queue_count = vm_queue_summary.as_ref().map(|summary| summary.count).unwrap_or(0),
        vm_queue_p50_ms = vm_queue_summary.as_ref().map(|summary| summary.p50).unwrap_or(0),
        vm_queue_p95_ms = vm_queue_summary.as_ref().map(|summary| summary.p95).unwrap_or(0),
        vm_queue_min_ms = vm_queue_summary.as_ref().map(|summary| summary.min).unwrap_or(0),
        vm_queue_max_ms = vm_queue_summary.as_ref().map(|summary| summary.max).unwrap_or(0),
        vm_queue_avg_ms = vm_queue_summary.as_ref().map(|summary| summary.avg).unwrap_or(0),
        vm_compute_count = vm_compute_summary.as_ref().map(|summary| summary.count).unwrap_or(0),
        vm_compute_p50_ms = vm_compute_summary.as_ref().map(|summary| summary.p50).unwrap_or(0),
        vm_compute_p95_ms = vm_compute_summary.as_ref().map(|summary| summary.p95).unwrap_or(0),
        vm_compute_min_ms = vm_compute_summary.as_ref().map(|summary| summary.min).unwrap_or(0),
        vm_compute_max_ms = vm_compute_summary.as_ref().map(|summary| summary.max).unwrap_or(0),
        vm_compute_avg_ms = vm_compute_summary.as_ref().map(|summary| summary.avg).unwrap_or(0),
        total_skipped_native = metrics_snapshot.skipped_native,
        total_skipped_vm = metrics_snapshot.skipped_vm,
        total_pool_timeout_native = metrics_snapshot.pool_timeout_native,
        total_pool_timeout_vm = metrics_snapshot.pool_timeout_vm,
        total_quote_timeout = metrics_snapshot.quote_timeout,
        "Quote metrics"
    );

    meta.failures = failures;
    QuoteComputation { responses, meta }
}

struct PoolQuoteResult {
    pool: String,
    pool_name: String,
    pool_address: String,
    protocol: String,
    is_vm: bool,
    amounts_out: Vec<String>,
    gas_used: Vec<u64>,
    errors: Vec<String>,
    timed_out: bool,
    queue_delay_ms: u64,
    compute_ms: u64,
}

struct FailureContext<'a> {
    pool_id: &'a str,
    pool_name: Option<&'a str>,
    pool_address: Option<&'a str>,
    protocol: Option<&'a str>,
}

#[derive(Default)]
struct TimingStats {
    values: Vec<u64>,
}

struct TimingSummary {
    count: usize,
    min: u64,
    max: u64,
    avg: u64,
    p50: u64,
    p95: u64,
}

impl TimingStats {
    fn record(&mut self, value: u64) {
        self.values.push(value);
    }

    fn summarize(&mut self) -> Option<TimingSummary> {
        if self.values.is_empty() {
            return None;
        }

        self.values.sort_unstable();
        let count = self.values.len();
        let min = self.values[0];
        let max = self.values[count - 1];
        let sum: u128 = self.values.iter().map(|value| *value as u128).sum();
        let avg = (sum / count as u128) as u64;
        let p50 = percentile(&self.values, 50);
        let p95 = percentile(&self.values, 95);

        Some(TimingSummary {
            count,
            min,
            max,
            avg,
            p50,
            p95,
        })
    }
}

fn percentile(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (sorted.len() - 1) * pct / 100;
    sorted[idx]
}

#[allow(clippy::too_many_arguments)]
async fn simulate_pool(
    pool_id: String,
    pool_state: Arc<dyn ProtocolSim>,
    pool_address: String,
    pool_name: String,
    pool_protocol: String,
    token_in: Arc<Token>,
    token_out: Arc<Token>,
    amounts: Arc<Vec<BigUint>>,
    expected_len: usize,
    timeout: Duration,
    scheduled_at: Instant,
    cancel_token: CancellationToken,
    permit: OwnedSemaphorePermit,
    is_vm: bool,
) -> Result<PoolQuoteResult, QuoteFailure> {
    let pool_id_for_failure = pool_id.clone();
    let pool_addr_for_failure = pool_address.clone();
    let pool_name_for_failure = pool_name.clone();
    let pool_protocol_for_failure = pool_protocol.clone();
    let token_in_clone = Arc::clone(&token_in);
    let token_out_clone = Arc::clone(&token_out);
    let amounts_clone = Arc::clone(&amounts);

    let cancel_token_clone = cancel_token.clone();
    let handle = spawn_blocking(move || {
        // Hold the permit until the blocking work exits.
        let _permit = permit;
        let compute_started_at = Instant::now();
        let queue_delay_ms = compute_started_at
            .saturating_duration_since(scheduled_at)
            .as_millis() as u64;
        let mut amounts_out = Vec::with_capacity(expected_len);
        let mut gas_used = Vec::with_capacity(expected_len);
        let mut errors = Vec::new();
        let mut timed_out = false;
        let deadline = scheduled_at + timeout;

        for amount_in in amounts_clone.iter() {
            if cancel_token_clone.is_cancelled() {
                debug!(
                    scope = "pool_timeout",
                    pool_id = %pool_id,
                    protocol = %pool_protocol,
                    pool_name = %pool_name,
                    pool_address = %pool_address,
                    "Pool quote cancelled before completion"
                );
                errors.push("Cancelled".to_string());
                break;
            }
            if Instant::now() >= deadline {
                debug!(
                    scope = "pool_timeout",
                    pool_id = %pool_id,
                    protocol = %pool_protocol,
                    pool_name = %pool_name,
                    pool_address = %pool_address,
                    timeout_ms = timeout.as_millis() as u64,
                    "Pool quote exceeded deadline before completion"
                );
                errors.push(format!("Timed out after {}ms", timeout.as_millis()));
                timed_out = true;
                break;
            }
            match pool_state.get_amount_out(amount_in.clone(), &token_in_clone, &token_out_clone) {
                Ok(result) => {
                    if let Some(gas_u64) = result.gas.to_u64() {
                        amounts_out.push(result.amount.to_string());
                        gas_used.push(gas_u64);
                    } else {
                        let msg = "no gas reported".to_string();
                        debug!(
                            scope = "pool_timeout",
                            pool_id = %pool_id,
                            protocol = %pool_protocol,
                            pool_name = %pool_name,
                            pool_address = %pool_address,
                            "Pool quote error: {}",
                            msg
                        );
                        // Do not return results with no gas: mark as error and discard
                        errors.push(msg);
                        amounts_out.clear();
                        gas_used.clear();
                        break;
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    debug!(
                        scope = "pool_timeout",
                        pool_id = %pool_id,
                        protocol = %pool_protocol,
                        pool_name = %pool_name,
                        pool_address = %pool_address,
                        "Pool quote error: {}",
                        msg
                    );
                    errors.push(msg);
                }
            }

            if Instant::now() >= deadline {
                debug!(
                    scope = "pool_timeout",
                    pool_id = %pool_id,
                    protocol = %pool_protocol,
                    pool_name = %pool_name,
                    pool_address = %pool_address,
                    timeout_ms = timeout.as_millis() as u64,
                    "Pool quote deadline reached after ladder step"
                );
                if errors.is_empty() {
                    errors.push(format!("Timed out after {}ms", timeout.as_millis()));
                }
                timed_out = true;
                break;
            }
        }

        let compute_ms = compute_started_at.elapsed().as_millis() as u64;
        PoolQuoteResult {
            pool: pool_id,
            pool_name,
            pool_address,
            protocol: pool_protocol,
            is_vm,
            amounts_out,
            gas_used,
            errors,
            timed_out,
            queue_delay_ms,
            compute_ms,
        }
    });
    tokio::pin!(handle);

    tokio::select! {
        res = handle.as_mut() => {
            match res {
                Ok(result) => Ok(result),
                Err(join_err) => {
                    let context = FailureContext {
                        pool_id: &pool_id_for_failure,
                        pool_name: Some(pool_name_for_failure.as_str()),
                        pool_address: Some(pool_addr_for_failure.as_str()),
                        protocol: Some(pool_protocol_for_failure.as_str()),
                    };
                    let descriptor = format_pool_descriptor(&context);
                    let message = format!(
                        "{}: Quote computation panicked: {}",
                        descriptor, join_err
                    );
                    Err(make_failure(QuoteFailureKind::Internal, message, Some(context)))
                }
            }
        }
        _ = cancel_token.cancelled() => {
            let context = FailureContext {
                pool_id: &pool_id_for_failure,
                pool_name: Some(pool_name_for_failure.as_str()),
                pool_address: Some(pool_addr_for_failure.as_str()),
                protocol: Some(pool_protocol_for_failure.as_str()),
            };
            let descriptor = format_pool_descriptor(&context);
            let message = format!("{}: Quote computation cancelled", descriptor);
            Err(make_failure(QuoteFailureKind::Timeout, message, Some(context)))
        }
    }
}

fn classify_failure(message: &str, from_pool: bool) -> QuoteFailureKind {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("cancelled") || lowered.contains("canceled") {
        // Treat cancellations as timeout-equivalent for downstream semantics
        QuoteFailureKind::Timeout
    } else if lowered.contains("warm") {
        QuoteFailureKind::WarmUp
    } else if lowered.contains("overflow") {
        QuoteFailureKind::Overflow
    } else if lowered.contains("timeout") {
        QuoteFailureKind::Timeout
    } else if !from_pool && lowered.contains("token") {
        QuoteFailureKind::TokenCoverage
    } else {
        QuoteFailureKind::Simulator
    }
}

fn format_pool_descriptor(context: &FailureContext<'_>) -> String {
    let mut head = Vec::new();
    if let Some(protocol) = context.protocol {
        if !protocol.is_empty() {
            head.push(protocol);
        }
    }
    if let Some(name) = context.pool_name {
        if !name.is_empty() {
            head.push(name);
        }
    }

    let mut label = if !head.is_empty() {
        head.join("::")
    } else {
        context.pool_id.to_string()
    };

    if let Some(address) = context.pool_address {
        if !address.is_empty() {
            label = format!("{} [{}]", label, address);
        }
    }

    label
}

fn decode_attribute(value: &Bytes) -> Option<String> {
    if value.is_empty() {
        return None;
    }

    let mut bytes = value.to_vec();
    while matches!(bytes.last(), Some(&0)) {
        bytes.pop();
    }

    std::str::from_utf8(&bytes)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn derive_pool_name(component: &ProtocolComponent) -> String {
    for key in ["pool_name", "name", "label"] {
        if let Some(label) = component
            .static_attributes
            .get(key)
            .and_then(decode_attribute)
        {
            if !label.is_empty() {
                return label;
            }
        }
    }

    let symbols: Vec<&str> = component
        .tokens
        .iter()
        .map(|token| token.symbol.as_str())
        .filter(|symbol| !symbol.is_empty())
        .collect();

    if !symbols.is_empty() {
        return format!("{}::{}", component.protocol_system, symbols.join("/"));
    }

    if !component.protocol_type_name.is_empty() {
        return format!(
            "{}::{}",
            component.protocol_system, component.protocol_type_name
        );
    }

    component.protocol_system.clone()
}

fn make_failure(
    kind: QuoteFailureKind,
    message: String,
    pool: Option<FailureContext<'_>>,
) -> QuoteFailure {
    let (pool, pool_name, pool_address, protocol) = pool.map_or_else(
        || (None, None, None, None),
        |context| {
            (
                Some(context.pool_id.to_string()),
                context.pool_name.map(|value| value.to_string()),
                context.pool_address.map(|value| value.to_string()),
                context.protocol.map(|value| value.to_string()),
            )
        },
    );

    QuoteFailure {
        kind,
        message,
        pool,
        pool_name,
        pool_address,
        protocol,
    }
}
