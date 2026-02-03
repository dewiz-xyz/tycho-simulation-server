use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use num_bigint::BigUint;
use num_traits::cast::ToPrimitive;
use tokio::sync::{OwnedSemaphorePermit, TryAcquireError};
use tokio::task::spawn_blocking;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use tycho_simulation::{
    protocol::models::ProtocolComponent,
    tycho_common::{models::token::Token, simulation::protocol_sim::ProtocolSim, Bytes},
};

use crate::models::messages::{
    AmountOutRequest, AmountOutResponse, QuoteFailure, QuoteFailureKind, QuoteMeta, QuoteStatus,
};
use crate::models::state::AppState;
use crate::models::tokens::TokenStoreError;

// Per-request scheduling metrics (logged once by the handler).
#[derive(Debug, Default, Clone, Copy)]
pub struct QuoteMetrics {
    pub scheduled_native_pools: usize,
    pub scheduled_vm_pools: usize,
    pub skipped_vm_unavailable: bool,
    pub skipped_native_concurrency: usize,
    pub skipped_vm_concurrency: usize,
    pub skipped_native_deadline: usize,
    pub skipped_vm_deadline: usize,
}

pub struct QuoteComputation {
    pub responses: Vec<AmountOutResponse>,
    pub meta: QuoteMeta,
    pub metrics: QuoteMetrics,
}

pub async fn get_amounts_out(
    state: AppState,
    request: AmountOutRequest,
    cancel: Option<CancellationToken>,
) -> QuoteComputation {
    let mut responses = Vec::new();
    let mut failures = Vec::new();
    let mut metrics = QuoteMetrics::default();

    let readiness_wait = Duration::from_secs(2);
    let quote_timeout = state.quote_timeout();

    let mut current_block = state.current_block().await;
    let mut current_vm_block = state.current_vm_block().await;
    let mut total_pools = state.total_pools().await;

    let mut meta = QuoteMeta {
        status: QuoteStatus::Ready,
        block_number: current_block,
        vm_block_number: current_vm_block,
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
        return QuoteComputation {
            responses,
            meta,
            metrics,
        };
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
            return QuoteComputation {
                responses,
                meta,
                metrics,
            };
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
            return QuoteComputation {
                responses,
                meta,
                metrics,
            };
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
            return QuoteComputation {
                responses,
                meta,
                metrics,
            };
        }
        current_block = state.current_block().await;
        current_vm_block = state.current_vm_block().await;
        total_pools = state.total_pools().await;
        meta.block_number = current_block;
        meta.vm_block_number = current_vm_block;
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
            return QuoteComputation {
                responses,
                meta,
                metrics,
            };
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
            return QuoteComputation {
                responses,
                meta,
                metrics,
            };
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
            return QuoteComputation {
                responses,
                meta,
                metrics,
            };
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
            return QuoteComputation {
                responses,
                meta,
                metrics,
            };
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
            return QuoteComputation {
                responses,
                meta,
                metrics,
            };
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
            return QuoteComputation {
                responses,
                meta,
                metrics,
            };
        }
    };

    debug!(
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
            return QuoteComputation {
                responses,
                meta,
                metrics,
            };
        }
    };

    let amounts_in = Arc::new(amounts_in);
    let quote_deadline = Instant::now() + quote_timeout;

    let native_candidates_raw = state
        .native_state_store
        .matching_pools_by_addresses(&token_in_bytes, &token_out_bytes)
        .await;

    let vm_ready = state.vm_ready().await;
    if !vm_ready {
        metrics.skipped_vm_unavailable = true;
    }
    let vm_candidates_raw = if vm_ready {
        state
            .vm_state_store
            .matching_pools_by_addresses(&token_in_bytes, &token_out_bytes)
            .await
    } else {
        Vec::new()
    };

    let mut native_candidates: Vec<(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> =
        native_candidates_raw
            .into_iter()
            .map(|(id, (pool_state, component))| (id, pool_state, component))
            .collect();

    let mut vm_candidates: Vec<(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> =
        vm_candidates_raw
            .into_iter()
            .map(|(id, (pool_state, component))| (id, pool_state, component))
            .collect();

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
        return QuoteComputation {
            responses,
            meta,
            metrics,
        };
    }

    debug!(
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

    let cancel_token = cancel
        .as_ref()
        .map(CancellationToken::child_token)
        .unwrap_or_default();
    let mut tasks = FuturesUnordered::new();
    let native_semaphore = state.native_sim_semaphore();
    let vm_semaphore = state.vm_sim_semaphore();
    // Stable scheduling to reduce jitter under contention
    native_candidates.sort_by(|a, b| a.0.cmp(&b.0));
    vm_candidates.sort_by(|a, b| a.0.cmp(&b.0));

    // Native first
    for (id, pool_state, component) in native_candidates.into_iter() {
        if cancel_token.is_cancelled() {
            break;
        }

        let now = Instant::now();
        let proposed_deadline = now + state.pool_timeout_native();
        let pool_deadline = if proposed_deadline <= quote_deadline {
            proposed_deadline
        } else {
            quote_deadline
        };

        if pool_deadline <= now {
            metrics.skipped_native_deadline += 1;
            continue;
        }

        let permit = match native_semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => {
                metrics.skipped_native_concurrency += 1;
                continue;
            }
            Err(TryAcquireError::Closed) => {
                failures.push(make_failure(
                    QuoteFailureKind::Internal,
                    "Native pool semaphore closed".to_string(),
                    None,
                ));
                meta.status = QuoteStatus::InternalError;
                break;
            }
        };

        let pool_address = component.id.to_string();
        let pool_protocol = component.protocol_system.clone();
        let pool_name = derive_pool_name(&component);

        let pool_cancel = cancel_token.child_token();
        metrics.scheduled_native_pools += 1;

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
            pool_deadline,
            pool_cancel,
            permit,
        ));
    }

    // VM second
    for (id, pool_state, component) in vm_candidates.into_iter() {
        if cancel_token.is_cancelled() {
            break;
        }

        let now = Instant::now();
        let proposed_deadline = now + state.pool_timeout_vm();
        let pool_deadline = if proposed_deadline <= quote_deadline {
            proposed_deadline
        } else {
            quote_deadline
        };

        if pool_deadline <= now {
            metrics.skipped_vm_deadline += 1;
            continue;
        }

        let permit = match vm_semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => {
                metrics.skipped_vm_concurrency += 1;
                continue;
            }
            Err(TryAcquireError::Closed) => {
                failures.push(make_failure(
                    QuoteFailureKind::Internal,
                    "VM pool semaphore closed".to_string(),
                    None,
                ));
                meta.status = QuoteStatus::InternalError;
                break;
            }
        };

        let pool_address = component.id.to_string();
        let pool_protocol = component.protocol_system.clone();
        let pool_name = derive_pool_name(&component);

        let pool_cancel = cancel_token.child_token();
        metrics.scheduled_vm_pools += 1;

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
            pool_deadline,
            pool_cancel,
            permit,
        ));
    }

    if !tasks.is_empty() {
        let remaining = quote_deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::from_millis(0));
        let quote_timeout_sleep = sleep(remaining);
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

    // Record scheduling skips as a single aggregated failure to avoid bloating responses
    if metrics.skipped_native_concurrency > 0
        || metrics.skipped_vm_concurrency > 0
        || metrics.skipped_native_deadline > 0
        || metrics.skipped_vm_deadline > 0
    {
        failures.push(make_failure(
            QuoteFailureKind::ConcurrencyLimit,
            format!(
                "Skipped pools due to scheduling limits: native_concurrency={} vm_concurrency={} native_deadline={} vm_deadline={}",
                metrics.skipped_native_concurrency,
                metrics.skipped_vm_concurrency,
                metrics.skipped_native_deadline,
                metrics.skipped_vm_deadline
            ),
            None,
        ));
        if matches!(meta.status, QuoteStatus::Ready) {
            meta.status = QuoteStatus::PartialFailure;
        }
    }

    if responses.is_empty() {
        if matches!(meta.status, QuoteStatus::Ready) {
            meta.status = QuoteStatus::PartialFailure;
        }
        if failures.is_empty() {
            failures.push(make_failure(
                QuoteFailureKind::Simulator,
                "All pools returned zero amounts".to_string(),
                None,
            ));
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
        debug!(
            "Quote response: total_results={} top_pool={} address={} first_amount_out={} block={} vm_block={:?}",
            responses.len(),
            top.pool_name,
            top.pool_address,
            top.amounts_out
                .first()
                .cloned()
                .unwrap_or_else(|| "0".to_string()),
            top.block_number,
            meta.vm_block_number
        );

        if !failures.is_empty() && matches!(meta.status, QuoteStatus::Ready) {
            meta.status = QuoteStatus::PartialFailure;
        }
    }

    meta.failures = failures;
    QuoteComputation {
        responses,
        meta,
        metrics,
    }
}

struct PoolQuoteResult {
    pool: String,
    pool_name: String,
    pool_address: String,
    protocol: String,
    amounts_out: Vec<String>,
    gas_used: Vec<u64>,
    errors: Vec<String>,
    timed_out: bool,
}

struct FailureContext<'a> {
    pool_id: &'a str,
    pool_name: Option<&'a str>,
    pool_address: Option<&'a str>,
    protocol: Option<&'a str>,
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
    deadline: Instant,
    cancel_token: CancellationToken,
    permit: OwnedSemaphorePermit,
) -> Result<PoolQuoteResult, QuoteFailure> {
    let pool_id_for_failure = pool_id.clone();
    let pool_addr_for_failure = pool_address.clone();
    let pool_name_for_failure = pool_name.clone();
    let pool_protocol_for_failure = pool_protocol.clone();
    let token_in_clone = Arc::clone(&token_in);
    let token_out_clone = Arc::clone(&token_out);
    let amounts_clone = Arc::clone(&amounts);

    let cancel_token_clone = cancel_token.clone();
    let sleep_duration = deadline
        .checked_duration_since(Instant::now())
        .unwrap_or(Duration::from_millis(0));
    let handle = spawn_blocking(move || {
        // Hold the permit until the blocking work exits.
        let _permit = permit;
        let mut amounts_out = Vec::with_capacity(expected_len);
        let mut gas_used = Vec::with_capacity(expected_len);
        let mut errors = Vec::new();
        let mut timed_out = false;

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
                    "Pool quote exceeded deadline before completion"
                );
                errors.push("Timed out".to_string());
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
                    "Pool quote deadline reached after ladder step"
                );
                if errors.is_empty() {
                    errors.push("Timed out".to_string());
                }
                timed_out = true;
                break;
            }
        }

        PoolQuoteResult {
            pool: pool_id,
            pool_name,
            pool_address,
            protocol: pool_protocol,
            amounts_out,
            gas_used,
            errors,
            timed_out,
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
            cancel_token.cancel();
            handle.as_mut().abort();
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
        _ = sleep(sleep_duration) => {
            cancel_token.cancel();
            handle.as_mut().abort();
            let context = FailureContext {
                pool_id: &pool_id_for_failure,
                pool_name: Some(pool_name_for_failure.as_str()),
                pool_address: Some(pool_addr_for_failure.as_str()),
                protocol: Some(pool_protocol_for_failure.as_str()),
            };
            let descriptor = format_pool_descriptor(&context);
            let message = format!("{}: Quote computation timed out", descriptor);
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
