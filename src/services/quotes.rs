use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use num_bigint::BigUint;
use num_traits::{cast::ToPrimitive, Zero};
use tokio::sync::{OwnedSemaphorePermit, TryAcquireError};
use tokio::task::spawn_blocking;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use tycho_simulation::{
    protocol::models::ProtocolComponent,
    tycho_common::{
        models::token::Token,
        simulation::{errors::SimulationError, protocol_sim::ProtocolSim},
        Bytes,
    },
};

use crate::models::messages::{
    AmountOutRequest, AmountOutResponse, PoolOutcomeKind, PoolSimulationOutcome, QuoteFailure,
    QuoteFailureKind, QuoteMeta, QuoteResultQuality, QuoteStatus,
};
use crate::models::state::AppState;
use crate::models::tokens::TokenStoreError;

const VM_LOW_FIRST_GAS_THRESHOLD: u64 = 600_000;
const VM_LOW_FIRST_GAS_SAMPLE_CAP: usize = 3;
const SPOT_PRICE_SCALE: u128 = 1_000_000_000;
const WEI_PER_ETH: u128 = 1_000_000_000_000_000_000;
const MIN_REASONABLE_ETH_TO_SELL_SPOT: f64 = 1e-12;
const MAX_REASONABLE_ETH_TO_SELL_SPOT: f64 = 1e12;

// Per-request scheduling metrics (logged once by the handler).
#[derive(Debug, Default, Clone)]
pub struct QuoteMetrics {
    pub scheduled_native_pools: usize,
    pub scheduled_vm_pools: usize,
    pub skipped_vm_unavailable: bool,
    pub skipped_native_concurrency: usize,
    pub skipped_vm_concurrency: usize,
    pub skipped_native_deadline: usize,
    pub skipped_vm_deadline: usize,
    pub skipped_native_limits: usize,
    pub skipped_vm_limits: usize,
    pub vm_completed_pools: usize,
    pub vm_median_first_gas: Option<u64>,
    pub vm_low_first_gas_count: usize,
    pub vm_low_first_gas_ratio: Option<f64>,
    pub vm_low_first_gas_samples: Vec<String>,
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
    let mut pool_results = Vec::new();
    let mut metrics = QuoteMetrics::default();

    let readiness_wait = Duration::from_secs(2);
    let quote_timeout = state.quote_timeout();

    let mut current_block = state.current_block().await;
    let mut current_vm_block = state.current_vm_block().await;
    let mut total_pools = state.total_pools().await;

    let mut meta = QuoteMeta {
        status: QuoteStatus::Ready,
        result_quality: QuoteResultQuality::RequestLevelFailure,
        block_number: current_block,
        vm_block_number: current_vm_block,
        matching_pools: 0,
        candidate_pools: 0,
        total_pools: Some(total_pools),
        auction_id: request.auction_id.clone(),
        pool_results: Vec::new(),
        vm_unavailable: false,
        failures: Vec::new(),
    };

    if cancel
        .as_ref()
        .map(|token| token.is_cancelled())
        .unwrap_or(false)
    {
        meta.status = QuoteStatus::PartialSuccess;
        meta.result_quality = QuoteResultQuality::RequestLevelFailure;
        meta.pool_results = pool_results;
        meta.vm_unavailable = metrics.skipped_vm_unavailable;
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
            meta.result_quality = QuoteResultQuality::RequestLevelFailure;
            meta.pool_results = pool_results;
            meta.vm_unavailable = metrics.skipped_vm_unavailable;
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
            meta.result_quality = QuoteResultQuality::RequestLevelFailure;
            meta.pool_results = pool_results;
            meta.vm_unavailable = metrics.skipped_vm_unavailable;
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
            meta.result_quality = QuoteResultQuality::RequestLevelFailure;
            meta.pool_results = pool_results;
            meta.vm_unavailable = metrics.skipped_vm_unavailable;
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
            meta.result_quality = QuoteResultQuality::RequestLevelFailure;
            meta.pool_results = pool_results;
            meta.vm_unavailable = metrics.skipped_vm_unavailable;
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
            meta.result_quality = QuoteResultQuality::RequestLevelFailure;
            meta.pool_results = pool_results;
            meta.vm_unavailable = metrics.skipped_vm_unavailable;
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
            meta.result_quality = QuoteResultQuality::RequestLevelFailure;
            meta.pool_results = pool_results;
            meta.vm_unavailable = metrics.skipped_vm_unavailable;
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
            meta.result_quality = QuoteResultQuality::RequestLevelFailure;
            meta.pool_results = pool_results;
            meta.vm_unavailable = metrics.skipped_vm_unavailable;
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
            meta.result_quality = QuoteResultQuality::RequestLevelFailure;
            meta.pool_results = pool_results;
            meta.vm_unavailable = metrics.skipped_vm_unavailable;
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
            meta.result_quality = QuoteResultQuality::RequestLevelFailure;
            meta.pool_results = pool_results;
            meta.vm_unavailable = metrics.skipped_vm_unavailable;
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
            meta.result_quality = QuoteResultQuality::RequestLevelFailure;
            meta.pool_results = pool_results;
            meta.vm_unavailable = metrics.skipped_vm_unavailable;
            meta.failures = failures;
            return QuoteComputation {
                responses,
                meta,
                metrics,
            };
        }
    };

    let requested_max_in = amounts_in.iter().max().cloned().unwrap_or_default();
    let amounts_in = Arc::new(amounts_in);
    let quote_deadline = Instant::now() + quote_timeout;

    let native_candidates_raw = state
        .native_state_store
        .matching_pools_by_addresses(&token_in_bytes, &token_out_bytes)
        .await;

    let vm_ready = state.vm_ready().await;
    if state.enable_vm_pools && !vm_ready {
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
        meta.result_quality = QuoteResultQuality::NoResults;
        meta.pool_results = pool_results;
        meta.vm_unavailable = metrics.skipped_vm_unavailable;
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

    let sell_token_decimals = token_in_ref.decimals;
    // Stable ordering ensures deterministic spot source and scheduling under contention.
    native_candidates.sort_by(|a, b| a.0.cmp(&b.0));
    vm_candidates.sort_by(|a, b| a.0.cmp(&b.0));

    let native = token_in_ref.chain.native_token();
    let wrapped_native = token_in_ref.chain.wrapped_native_token();

    let mut spot_native_candidates_raw = state
        .native_state_store
        .matching_pools_by_addresses(&token_in_bytes, &wrapped_native.address)
        .await;
    if native.address != wrapped_native.address {
        spot_native_candidates_raw.extend(
            state
                .native_state_store
                .matching_pools_by_addresses(&token_in_bytes, &native.address)
                .await,
        );
    }

    let mut seen_spot_ids = HashSet::new();
    let mut spot_native_candidates: Vec<(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> =
        Vec::new();
    for (id, (pool_state, component)) in spot_native_candidates_raw.drain(..) {
        if seen_spot_ids.insert(id.clone()) {
            spot_native_candidates.push((id, pool_state, component));
        }
    }
    spot_native_candidates.sort_by(|a, b| a.0.cmp(&b.0));

    let eth_to_sell_spot_price =
        resolve_request_eth_to_sell_spot_price(&token_in_ref, &spot_native_candidates, &[]);
    let gas_price_wei = state.effective_native_gas_price_wei_for_quotes().await;

    let token_in = Arc::new(token_in_ref);
    let token_out = Arc::new(token_out_ref);
    let expected_len = amounts_in.len();

    let cancel_token = cancel
        .as_ref()
        .map(CancellationToken::child_token)
        .unwrap_or_default();
    let mut tasks = FuturesUnordered::new();
    let mut vm_first_gases = Vec::new();
    let native_semaphore = state.native_sim_semaphore();
    let vm_semaphore = state.vm_sim_semaphore();

    // Native first
    for (id, pool_state, component) in native_candidates.into_iter() {
        if cancel_token.is_cancelled() {
            break;
        }

        let pool_address = component.id.to_string();
        let pool_protocol = component.protocol_system.clone();
        let pool_name = derive_pool_name(&component);

        let now = Instant::now();
        let proposed_deadline = now + state.pool_timeout_native();
        let pool_deadline = if proposed_deadline <= quote_deadline {
            proposed_deadline
        } else {
            quote_deadline
        };

        if pool_deadline <= now {
            metrics.skipped_native_deadline += 1;
            pool_results.push(make_pool_outcome(
                id.clone(),
                pool_name.clone(),
                pool_address.clone(),
                pool_protocol.clone(),
                PoolOutcomeKind::SkippedDeadline,
                0,
                expected_len,
                Some("Pool scheduling skipped because request deadline was reached".to_string()),
            ));
            continue;
        }

        let permit = match native_semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => {
                metrics.skipped_native_concurrency += 1;
                pool_results.push(make_pool_outcome(
                    id.clone(),
                    pool_name.clone(),
                    pool_address.clone(),
                    pool_protocol.clone(),
                    PoolOutcomeKind::SkippedConcurrency,
                    0,
                    expected_len,
                    Some(
                        "Pool scheduling skipped because native concurrency permits were exhausted"
                            .to_string(),
                    ),
                ));
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

        let pool_cancel = cancel_token.child_token();
        metrics.scheduled_native_pools += 1;
        let (sim_token_in, sim_token_out) =
            simulation_tokens_for_pool(token_in.as_ref(), token_out.as_ref(), component.as_ref());
        if sim_token_in.address != token_in.address || sim_token_out.address != token_out.address {
            debug!(
                scope = "native_wrapped_remap",
                pool_id = %id,
                protocol = %pool_protocol,
                requested_token_in = %token_in.address,
                simulation_token_in = %sim_token_in.address,
                requested_token_out = %token_out.address,
                simulation_token_out = %sim_token_out.address,
                "Remapped request tokens for pool simulation"
            );
        }

        tasks.push(simulate_pool(
            id,
            pool_state,
            pool_address,
            pool_name,
            pool_protocol,
            Arc::new(sim_token_in),
            Arc::new(sim_token_out),
            Arc::clone(&amounts_in),
            requested_max_in.clone(),
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

        let pool_address = component.id.to_string();
        let pool_protocol = component.protocol_system.clone();
        let pool_name = derive_pool_name(&component);

        let now = Instant::now();
        let proposed_deadline = now + state.pool_timeout_vm();
        let pool_deadline = if proposed_deadline <= quote_deadline {
            proposed_deadline
        } else {
            quote_deadline
        };

        if pool_deadline <= now {
            metrics.skipped_vm_deadline += 1;
            pool_results.push(make_pool_outcome(
                id.clone(),
                pool_name.clone(),
                pool_address.clone(),
                pool_protocol.clone(),
                PoolOutcomeKind::SkippedDeadline,
                0,
                expected_len,
                Some("Pool scheduling skipped because request deadline was reached".to_string()),
            ));
            continue;
        }

        let permit = match vm_semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => {
                metrics.skipped_vm_concurrency += 1;
                pool_results.push(make_pool_outcome(
                    id.clone(),
                    pool_name.clone(),
                    pool_address.clone(),
                    pool_protocol.clone(),
                    PoolOutcomeKind::SkippedConcurrency,
                    0,
                    expected_len,
                    Some(
                        "Pool scheduling skipped because VM concurrency permits were exhausted"
                            .to_string(),
                    ),
                ));
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

        let pool_cancel = cancel_token.child_token();
        metrics.scheduled_vm_pools += 1;
        let (sim_token_in, sim_token_out) =
            simulation_tokens_for_pool(token_in.as_ref(), token_out.as_ref(), component.as_ref());
        if sim_token_in.address != token_in.address || sim_token_out.address != token_out.address {
            debug!(
                scope = "native_wrapped_remap",
                pool_id = %id,
                protocol = %pool_protocol,
                requested_token_in = %token_in.address,
                simulation_token_in = %sim_token_in.address,
                requested_token_out = %token_out.address,
                simulation_token_out = %sim_token_out.address,
                "Remapped request tokens for pool simulation"
            );
        }

        tasks.push(simulate_pool(
            id,
            pool_state,
            pool_address,
            pool_name,
            pool_protocol,
            Arc::new(sim_token_in),
            Arc::new(sim_token_out),
            Arc::clone(&amounts_in),
            requested_max_in.clone(),
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
                    meta.status = QuoteStatus::PartialSuccess;
                    break;
                }
                _ = cancel_token.cancelled() => {
                    meta.status = QuoteStatus::PartialSuccess;
                    break;
                }
                maybe_outcome = tasks.next() => {
                    match maybe_outcome {
                        Some(outcome) => {
                            match outcome {
                                Ok(outcome) => match outcome {
                                    PoolSimOutcome::Simulated(result) => {
                                        let had_timeout = is_timeout_like_outcome(&result);
                                        let is_partial = result.amounts_out.len() < expected_len;
                                        record_vm_first_gas_metrics(
                                            &mut metrics,
                                            &mut vm_first_gases,
                                            result.protocol.as_str(),
                                            result.pool.as_str(),
                                            result.gas_used.first().copied(),
                                        );

                                        if let Some((outcome_kind, reason)) =
                                            classify_pool_outcome(&result, expected_len)
                                        {
                                            pool_results.push(make_pool_outcome(
                                                result.pool.clone(),
                                                result.pool_name.clone(),
                                                result.pool_address.clone(),
                                                result.protocol.clone(),
                                                outcome_kind,
                                                result.amounts_out.len(),
                                                expected_len,
                                                reason,
                                            ));
                                        }

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
                                            let gas_in_sell = compute_gas_in_sell_base_units(
                                                gas_price_wei,
                                                result.gas_used.last().copied(),
                                                eth_to_sell_spot_price,
                                                sell_token_decimals,
                                            );
                                            responses.push(AmountOutResponse {
                                                pool: result.pool,
                                                pool_name: result.pool_name,
                                                pool_address: result.pool_address,
                                                amounts_out: result.amounts_out,
                                                gas_used: result.gas_used,
                                                gas_in_sell,
                                                block_number: current_block,
                                            });
                                        }
                                    }
                                    PoolSimOutcome::SkippedDueToLimits {
                                        pool,
                                        pool_name,
                                        pool_address,
                                        protocol,
                                        reason,
                                    } => {
                                        if protocol.starts_with("vm:") {
                                            metrics.skipped_vm_limits += 1;
                                        } else {
                                            metrics.skipped_native_limits += 1;
                                        }
                                        pool_results.push(make_pool_outcome(
                                            pool,
                                            pool_name,
                                            pool_address,
                                            protocol,
                                            PoolOutcomeKind::SkippedPrecheck,
                                            0,
                                            expected_len,
                                            reason,
                                        ));
                                    }
                                },
                                Err(failure) => {
                                    if let Some(pool_outcome) =
                                        pool_outcome_from_failure(&failure, expected_len)
                                    {
                                        pool_results.push(pool_outcome);
                                    }
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
    finalize_vm_first_gas_metrics(&mut metrics, &mut vm_first_gases);

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
            meta.status = QuoteStatus::PartialSuccess;
        }
    }

    if responses.is_empty() {
        if failures.is_empty()
            && matches!(meta.status, QuoteStatus::Ready)
            && (metrics.skipped_native_limits > 0 || metrics.skipped_vm_limits > 0)
        {
            meta.status = QuoteStatus::NoLiquidity;
            failures.push(make_failure(
                QuoteFailureKind::NoPools,
                "All matching pools exceed liquidity limits for requested amount; check get_limits before quoting"
                    .to_string(),
                None,
            ));
        } else {
            if matches!(meta.status, QuoteStatus::Ready) {
                meta.status = QuoteStatus::PartialSuccess;
            }
            if failures.is_empty() {
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
            meta.status = QuoteStatus::PartialSuccess;
        }
    }

    pool_results.sort_by(|a, b| {
        a.protocol
            .cmp(&b.protocol)
            .then(a.pool.cmp(&b.pool))
            .then(a.pool_address.cmp(&b.pool_address))
            .then(outcome_kind_label(a.outcome).cmp(outcome_kind_label(b.outcome)))
    });

    meta.result_quality = if responses.is_empty() {
        QuoteResultQuality::NoResults
    } else if pool_results.is_empty() && failures.is_empty() {
        QuoteResultQuality::Complete
    } else {
        QuoteResultQuality::Partial
    };
    meta.pool_results = pool_results;
    meta.vm_unavailable = metrics.skipped_vm_unavailable;
    meta.failures = failures;
    QuoteComputation {
        responses,
        meta,
        metrics,
    }
}

fn simulation_tokens_for_pool(
    request_token_in: &Token,
    request_token_out: &Token,
    component: &ProtocolComponent,
) -> (Token, Token) {
    (
        remap_request_token_for_pool(request_token_in, component),
        remap_request_token_for_pool(request_token_out, component),
    )
}

fn remap_request_token_for_pool(request_token: &Token, component: &ProtocolComponent) -> Token {
    let native = request_token.chain.native_token();
    let wrapped_native = request_token.chain.wrapped_native_token();

    if native.address == wrapped_native.address {
        return request_token.clone();
    }

    let pool_has_native = component
        .tokens
        .iter()
        .any(|token| token.address == native.address);
    let pool_has_wrapped = component
        .tokens
        .iter()
        .any(|token| token.address == wrapped_native.address);

    if request_token.address == wrapped_native.address && pool_has_native && !pool_has_wrapped {
        return native;
    }

    if request_token.address == native.address && pool_has_wrapped && !pool_has_native {
        return wrapped_native;
    }

    request_token.clone()
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

enum PoolSimOutcome {
    Simulated(PoolQuoteResult),
    SkippedDueToLimits {
        pool: String,
        pool_name: String,
        pool_address: String,
        protocol: String,
        reason: Option<String>,
    },
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
    requested_max_in: BigUint,
    expected_len: usize,
    deadline: Instant,
    cancel_token: CancellationToken,
    permit: OwnedSemaphorePermit,
) -> Result<PoolSimOutcome, QuoteFailure> {
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

        // Abort quickly if this task starts after cancellation/deadline.
        if cancel_token_clone.is_cancelled() {
            debug!(
                scope = "pool_timeout",
                pool_id = %pool_id,
                protocol = %pool_protocol,
                pool_name = %pool_name,
                pool_address = %pool_address,
                "Pool quote cancelled before completion"
            );
            return PoolSimOutcome::Simulated(PoolQuoteResult {
                pool: pool_id,
                pool_name,
                pool_address,
                protocol: pool_protocol,
                amounts_out: Vec::new(),
                gas_used: Vec::new(),
                errors: vec!["Cancelled".to_string()],
                timed_out: false,
            });
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
            return PoolSimOutcome::Simulated(PoolQuoteResult {
                pool: pool_id,
                pool_name,
                pool_address,
                protocol: pool_protocol,
                amounts_out: Vec::new(),
                gas_used: Vec::new(),
                errors: vec!["Timed out".to_string()],
                timed_out: true,
            });
        }

        // Short-circuit when the request exceeds pool limits to avoid wasted compute.
        //
        // Note: `get_limits` can be a "soft" limit (advisory). When the request exceeds the
        // reported limit, we do a single probe before quoting the whole ladder:
        // - If some requested amounts are within the reported max, probe the max requested amount
        //   to avoid wasted compute for the largest step.
        // - If every requested amount exceeds the reported max, probe the smallest requested
        //   amount and only skip the whole pool if that also fails with a clear limits signal.
        let mut probed_amount_in: Option<BigUint> = None;
        let mut probed_amount: Option<(String, u64)> = None;
        let mut probed_amount_error: Option<String> = None;
        if !requested_max_in.is_zero() {
            match pool_state.get_limits(
                token_in_clone.address.clone(),
                token_out_clone.address.clone(),
            ) {
                Ok((max_in, _max_out)) => {
                    if requested_max_in > max_in {
                        let all_amounts_exceed_limit =
                            amounts_clone.iter().all(|amount| amount > &max_in);
                        let probe_amount_in = if all_amounts_exceed_limit {
                            // Prefer probing the smallest requested amount when every request is
                            // above the reported limit: `max_in` can be conservative.
                            amounts_clone
                                .iter()
                                .min()
                                .cloned()
                                .unwrap_or_else(|| requested_max_in.clone())
                        } else {
                            requested_max_in.clone()
                        };
                        match pool_state.get_amount_out(
                            probe_amount_in.clone(),
                            &token_in_clone,
                            &token_out_clone,
                        ) {
                            Ok(result) => {
                                if let Some(gas_u64) = result.gas.to_u64() {
                                    probed_amount_in = Some(probe_amount_in);
                                    probed_amount = Some((result.amount.to_string(), gas_u64));
                                } else {
                                    return PoolSimOutcome::Simulated(PoolQuoteResult {
                                        pool: pool_id,
                                        pool_name,
                                        pool_address,
                                        protocol: pool_protocol,
                                        amounts_out: Vec::new(),
                                        gas_used: Vec::new(),
                                        errors: vec![format!(
                                            "Probe quote returned gas that does not fit u64 (amount_in={}, max_in={})",
                                            probe_amount_in, max_in,
                                        )],
                                        timed_out: false,
                                    });
                                }
                            }
                            Err(SimulationError::InvalidInput(message, maybe_result)) => {
                                let probe_error = format!(
                                    "Probe quote failed (amount_in={}, max_in={}): Invalid input: {}",
                                    probe_amount_in, max_in, message,
                                );
                                if is_limits_exhaustion_probe_error(
                                    &message,
                                    maybe_result.is_some(),
                                ) {
                                    if all_amounts_exceed_limit {
                                        debug!(
                                            scope = "limits_precheck",
                                            pool_id = %pool_id,
                                            protocol = %pool_protocol,
                                            pool_name = %pool_name,
                                            pool_address = %pool_address,
                                            amount_in = %probe_amount_in,
                                            max_in = %max_in,
                                            "Skipping pool due to get_limits precheck: {}",
                                            message,
                                        );
                                        return PoolSimOutcome::SkippedDueToLimits {
                                            pool: pool_id.clone(),
                                            pool_name: pool_name.clone(),
                                            pool_address: pool_address.clone(),
                                            protocol: pool_protocol.clone(),
                                            reason: Some(format!(
                                                "Exceeded pool limits during precheck (amount_in={}, max_in={}): {}",
                                                probe_amount_in, max_in, message
                                            )),
                                        };
                                    }
                                    debug!(
                                        scope = "limits_precheck",
                                        pool_id = %pool_id,
                                        protocol = %pool_protocol,
                                        pool_name = %pool_name,
                                        pool_address = %pool_address,
                                        amount_in = %probe_amount_in,
                                        max_in = %max_in,
                                        "Probe indicates limit on max amount; continuing with lower amounts: {}",
                                        message,
                                    );
                                    probed_amount_in = Some(probe_amount_in);
                                    probed_amount_error = Some(probe_error);
                                } else {
                                    debug!(
                                        scope = "limits_precheck",
                                        pool_id = %pool_id,
                                        protocol = %pool_protocol,
                                        pool_name = %pool_name,
                                        pool_address = %pool_address,
                                        amount_in = %probe_amount_in,
                                        max_in = %max_in,
                                        "Probe invalid-input was not a limits signal; continuing ladder: {}",
                                        message,
                                    );
                                    probed_amount_in = Some(probe_amount_in);
                                    probed_amount_error = Some(probe_error);
                                }
                            }
                            Err(other) => {
                                return PoolSimOutcome::Simulated(PoolQuoteResult {
                                    pool: pool_id,
                                    pool_name,
                                    pool_address,
                                    protocol: pool_protocol,
                                    amounts_out: Vec::new(),
                                    gas_used: Vec::new(),
                                    errors: vec![format!(
                                        "Probe quote failed (amount_in={}, max_in={}): {}",
                                        probe_amount_in, max_in, other,
                                    )],
                                    timed_out: false,
                                });
                            }
                        }
                    }
                }
                Err(_) => {
                    // Best-effort: if limits are unavailable, proceed with the normal ladder.
                }
            }
        }

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

            if let Some(probed_amount_in_value) = probed_amount_in.as_ref() {
                if amount_in == probed_amount_in_value {
                    if let Some((amount_out, gas_u64)) = &probed_amount {
                        amounts_out.push(amount_out.clone());
                        gas_used.push(*gas_u64);
                        continue;
                    }
                    if let Some(probe_error) = &probed_amount_error {
                        errors.push(probe_error.clone());
                        continue;
                    }
                }
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

        PoolSimOutcome::Simulated(PoolQuoteResult {
            pool: pool_id,
            pool_name,
            pool_address,
            protocol: pool_protocol,
            amounts_out,
            gas_used,
            errors,
            timed_out,
        })
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

fn resolve_request_eth_to_sell_spot_price(
    sell_token: &Token,
    native_candidates: &[(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)],
    vm_candidates: &[(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)],
) -> Option<f64> {
    let native = sell_token.chain.native_token();
    let wrapped_native = sell_token.chain.wrapped_native_token();

    // Gas price is denominated in native token units (wei for ETH family).
    // Wrapped/native are interchangeable for conversion purposes.
    if sell_token.address == native.address || sell_token.address == wrapped_native.address {
        return Some(1.0);
    }

    let mut fallback_pool_state: Option<&Arc<dyn ProtocolSim>> = None;
    let mut selected_pool_state: Option<&Arc<dyn ProtocolSim>> = None;
    let mut best_liquidity_score: Option<BigUint> = None;

    for (_, pool_state, _) in native_candidates.iter().chain(vm_candidates.iter()) {
        if fallback_pool_state.is_none() {
            fallback_pool_state = Some(pool_state);
        }

        let Some(liquidity_score) = spot_liquidity_score_in_sell_token(
            pool_state.as_ref(),
            sell_token,
            &wrapped_native,
            &native,
        ) else {
            continue;
        };

        let should_replace = best_liquidity_score
            .as_ref()
            .map(|current| liquidity_score > *current)
            .unwrap_or(true);

        if should_replace {
            best_liquidity_score = Some(liquidity_score);
            selected_pool_state = Some(pool_state);
        }
    }

    let pool_state = selected_pool_state.or(fallback_pool_state)?;
    spot_price_eth_to_sell_from_pool(pool_state.as_ref(), sell_token, &wrapped_native, &native)
}

fn spot_liquidity_score_in_sell_token(
    pool_state: &dyn ProtocolSim,
    sell_token: &Token,
    wrapped_native: &Token,
    native: &Token,
) -> Option<BigUint> {
    let mut best_score: Option<BigUint> = None;

    let mut consider = |candidate: BigUint| {
        if candidate.is_zero() {
            return;
        }
        let should_replace = best_score
            .as_ref()
            .map(|current| candidate > *current)
            .unwrap_or(true);
        if should_replace {
            best_score = Some(candidate);
        }
    };

    if let Ok((max_in, _)) =
        pool_state.get_limits(sell_token.address.clone(), wrapped_native.address.clone())
    {
        consider(max_in);
    }
    if native.address != wrapped_native.address {
        if let Ok((max_in, _)) =
            pool_state.get_limits(sell_token.address.clone(), native.address.clone())
        {
            consider(max_in);
        }
    }

    if let Ok((_, max_out)) =
        pool_state.get_limits(wrapped_native.address.clone(), sell_token.address.clone())
    {
        consider(max_out);
    }
    if native.address != wrapped_native.address {
        if let Ok((_, max_out)) =
            pool_state.get_limits(native.address.clone(), sell_token.address.clone())
        {
            consider(max_out);
        }
    }

    best_score
}

fn spot_price_eth_to_sell_from_pool(
    pool_state: &dyn ProtocolSim,
    sell_token: &Token,
    wrapped_native: &Token,
    native: &Token,
) -> Option<f64> {
    pool_state
        .spot_price(wrapped_native, sell_token)
        .ok()
        .or_else(|| pool_state.spot_price(native, sell_token).ok())
        .or_else(|| {
            pool_state
                .spot_price(sell_token, wrapped_native)
                .ok()
                .and_then(|price| {
                    if price.is_finite() && price > 0.0 {
                        Some(1.0 / price)
                    } else {
                        None
                    }
                })
        })
        .or_else(|| {
            pool_state
                .spot_price(sell_token, native)
                .ok()
                .and_then(|price| {
                    if price.is_finite() && price > 0.0 {
                        Some(1.0 / price)
                    } else {
                        None
                    }
                })
        })
        .filter(|price| {
            price.is_finite()
                && *price > 0.0
                && *price >= MIN_REASONABLE_ETH_TO_SELL_SPOT
                && *price <= MAX_REASONABLE_ETH_TO_SELL_SPOT
        })
}

fn compute_gas_in_sell_base_units(
    gas_price_wei: Option<u128>,
    gas_used_last: Option<u64>,
    eth_to_sell_spot_price: Option<f64>,
    sell_token_decimals: u32,
) -> String {
    let Some(gas_price_wei) = gas_price_wei else {
        return "0".to_string();
    };
    let Some(gas_used_last) = gas_used_last else {
        return "0".to_string();
    };
    let Some(spot_price) = eth_to_sell_spot_price.filter(|price| price.is_finite() && *price > 0.0)
    else {
        return "0".to_string();
    };
    let Some(spot_price_scaled) = (spot_price * SPOT_PRICE_SCALE as f64).floor().to_u128() else {
        return "0".to_string();
    };
    if spot_price_scaled == 0 {
        return "0".to_string();
    }

    let decimals_scale = BigUint::from(10u32).pow(sell_token_decimals);
    let numerator = BigUint::from(gas_price_wei)
        * BigUint::from(spot_price_scaled)
        * decimals_scale
        * BigUint::from(gas_used_last);
    let denominator = BigUint::from(SPOT_PRICE_SCALE) * BigUint::from(WEI_PER_ETH);
    (numerator / denominator).to_string()
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

#[allow(clippy::too_many_arguments)]
fn make_pool_outcome(
    pool: String,
    pool_name: String,
    pool_address: String,
    protocol: String,
    outcome: PoolOutcomeKind,
    reported_steps: usize,
    expected_steps: usize,
    reason: Option<String>,
) -> PoolSimulationOutcome {
    PoolSimulationOutcome {
        pool,
        pool_name,
        pool_address,
        protocol,
        outcome,
        reported_steps,
        expected_steps,
        reason,
    }
}

fn outcome_kind_label(kind: PoolOutcomeKind) -> &'static str {
    match kind {
        PoolOutcomeKind::PartialOutput => "partial_output",
        PoolOutcomeKind::ZeroOutput => "zero_output",
        PoolOutcomeKind::SkippedConcurrency => "skipped_concurrency",
        PoolOutcomeKind::SkippedDeadline => "skipped_deadline",
        PoolOutcomeKind::SkippedPrecheck => "skipped_precheck",
        PoolOutcomeKind::TimedOut => "timed_out",
        PoolOutcomeKind::SimulatorError => "simulator_error",
        PoolOutcomeKind::InternalError => "internal_error",
    }
}

fn is_timeout_message(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("timeout")
        || lowered.contains("timed out")
        || lowered.contains("cancelled")
        || lowered.contains("canceled")
}

fn is_timeout_like_outcome(result: &PoolQuoteResult) -> bool {
    result.timed_out
        || result
            .errors
            .iter()
            .any(|message| is_timeout_message(message))
}

fn classify_pool_outcome(
    result: &PoolQuoteResult,
    expected_len: usize,
) -> Option<(PoolOutcomeKind, Option<String>)> {
    if is_timeout_like_outcome(result) {
        return Some((
            PoolOutcomeKind::TimedOut,
            result.errors.first().cloned().or_else(|| {
                Some(format!(
                    "Pool simulation timed out after {} reported step(s)",
                    result.amounts_out.len()
                ))
            }),
        ));
    }

    if !result.errors.is_empty() {
        return Some((
            PoolOutcomeKind::SimulatorError,
            result.errors.first().cloned(),
        ));
    }

    if result.amounts_out.is_empty() {
        return Some((PoolOutcomeKind::ZeroOutput, None));
    }

    if result.amounts_out.len() < expected_len {
        return Some((
            PoolOutcomeKind::PartialOutput,
            Some(format!(
                "Pool returned {} of {} ladder steps",
                result.amounts_out.len(),
                expected_len
            )),
        ));
    }

    None
}

fn pool_outcome_from_failure(
    failure: &QuoteFailure,
    expected_len: usize,
) -> Option<PoolSimulationOutcome> {
    let pool = failure.pool.clone()?;
    let pool_name = failure
        .pool_name
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let pool_address = failure
        .pool_address
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let protocol = failure
        .protocol
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    let outcome = match failure.kind {
        QuoteFailureKind::Timeout => PoolOutcomeKind::TimedOut,
        QuoteFailureKind::Internal => PoolOutcomeKind::InternalError,
        QuoteFailureKind::Simulator
        | QuoteFailureKind::Overflow
        | QuoteFailureKind::InconsistentResult => PoolOutcomeKind::SimulatorError,
        _ => return None,
    };

    Some(make_pool_outcome(
        pool,
        pool_name,
        pool_address,
        protocol,
        outcome,
        0,
        expected_len,
        Some(failure.message.clone()),
    ))
}

fn is_limits_exhaustion_probe_error(message: &str, has_partial_result: bool) -> bool {
    let lowered = message.to_ascii_lowercase();
    let limit_signal = lowered.contains("max_in")
        || lowered.contains("get_limits")
        || lowered.contains("sell amount exceeds limit")
        || lowered.contains("exceeds limit")
        || lowered.contains("ticks exceeded")
        || lowered.contains("not enough liquidity")
        || lowered.contains("support complete swap");
    if has_partial_result {
        // Partial results are commonly attached to limit exhaustion errors, but are not a
        // sufficient signal on their own.
        return limit_signal;
    }
    limit_signal
}

fn record_vm_first_gas_metrics(
    metrics: &mut QuoteMetrics,
    vm_first_gases: &mut Vec<u64>,
    protocol: &str,
    pool_id: &str,
    first_gas: Option<u64>,
) {
    if !protocol.starts_with("vm:") {
        return;
    }

    let Some(first_gas) = first_gas else {
        return;
    };

    metrics.vm_completed_pools += 1;
    vm_first_gases.push(first_gas);
    if first_gas < VM_LOW_FIRST_GAS_THRESHOLD {
        metrics.vm_low_first_gas_count += 1;
        if metrics.vm_low_first_gas_samples.len() < VM_LOW_FIRST_GAS_SAMPLE_CAP {
            metrics
                .vm_low_first_gas_samples
                .push(format!("{protocol}:{pool_id}:{first_gas}"));
        }
    }
}

fn finalize_vm_first_gas_metrics(metrics: &mut QuoteMetrics, vm_first_gases: &mut [u64]) {
    let Some(median_gas) = median_u64(vm_first_gases) else {
        return;
    };

    metrics.vm_median_first_gas = Some(median_gas);
    metrics.vm_low_first_gas_ratio =
        Some(metrics.vm_low_first_gas_count as f64 / metrics.vm_completed_pools as f64);
}

fn median_u64(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }

    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[mid])
    } else {
        Some(((values[mid - 1] as u128 + values[mid] as u128) / 2) as u64)
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::any::Any;
    use std::collections::{HashMap, HashSet};
    use std::str::FromStr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use chrono::NaiveDateTime;
    use num_traits::Zero;
    use tokio::sync::{RwLock, Semaphore};
    use tycho_simulation::protocol::models::Update;
    use tycho_simulation::tycho_common::dto::ProtocolStateDelta;
    use tycho_simulation::tycho_common::models::{token::Token, Chain};
    use tycho_simulation::tycho_common::simulation::errors::{SimulationError, TransitionError};
    use tycho_simulation::tycho_common::simulation::protocol_sim::{
        Balances, GetAmountOutResult, ProtocolSim,
    };
    use tycho_simulation::tycho_common::Bytes;

    use crate::models::state::{StateStore, VmStreamStatus};
    use crate::models::stream_health::StreamHealth;
    use crate::models::tokens::TokenStore;

    fn default_calls() -> Arc<AtomicUsize> {
        Arc::new(AtomicUsize::new(0))
    }

    #[test]
    fn median_u64_handles_odd_and_even_inputs() {
        let mut odd = vec![9, 1, 3];
        assert_eq!(median_u64(&mut odd), Some(3));

        let mut even = vec![10, 2, 6, 4];
        assert_eq!(median_u64(&mut even), Some(5));
    }

    #[test]
    fn median_u64_returns_none_for_empty_input() {
        let mut values: Vec<u64> = Vec::new();
        assert_eq!(median_u64(&mut values), None);
    }

    #[test]
    fn remap_request_token_for_pool_maps_wrapped_to_native_for_native_only_pool() {
        let chain = Chain::Ethereum;
        let native = chain.native_token();
        let wrapped = chain.wrapped_native_token();
        let token_x = make_token(&Bytes::from([9u8; 20]), "TKX");
        let component = ProtocolComponent::new(
            Bytes::from([1u8; 20]),
            "rocketpool".to_string(),
            "rocketpool".to_string(),
            chain,
            vec![native.clone(), token_x],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let mapped = remap_request_token_for_pool(&wrapped, &component);

        assert_eq!(mapped.address, native.address);
    }

    #[test]
    fn remap_request_token_for_pool_maps_native_to_wrapped_for_wrapped_only_pool() {
        let chain = Chain::Ethereum;
        let native = chain.native_token();
        let wrapped = chain.wrapped_native_token();
        let token_x = make_token(&Bytes::from([10u8; 20]), "TKX");
        let component = ProtocolComponent::new(
            Bytes::from([2u8; 20]),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            chain,
            vec![wrapped.clone(), token_x],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let mapped = remap_request_token_for_pool(&native, &component);

        assert_eq!(mapped.address, wrapped.address);
    }

    #[test]
    fn remap_request_token_for_pool_keeps_non_native_tokens_unchanged() {
        let chain = Chain::Ethereum;
        let native = chain.native_token();
        let request_token = make_token(&Bytes::from([42u8; 20]), "USDC");
        let token_x = make_token(&Bytes::from([11u8; 20]), "TKX");
        let component = ProtocolComponent::new(
            Bytes::from([3u8; 20]),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            chain,
            vec![native, token_x],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let mapped = remap_request_token_for_pool(&request_token, &component);

        assert_eq!(mapped.address, request_token.address);
    }

    #[test]
    fn vm_first_gas_metrics_aggregate_median_ratio_and_samples() {
        let mut metrics = QuoteMetrics::default();
        let mut vm_first_gases = Vec::new();

        record_vm_first_gas_metrics(
            &mut metrics,
            &mut vm_first_gases,
            "vm:curve",
            "pool-a",
            Some(400_000),
        );
        record_vm_first_gas_metrics(
            &mut metrics,
            &mut vm_first_gases,
            "vm:curve",
            "pool-b",
            Some(900_000),
        );
        record_vm_first_gas_metrics(
            &mut metrics,
            &mut vm_first_gases,
            "vm:curve",
            "pool-c",
            Some(500_000),
        );
        record_vm_first_gas_metrics(
            &mut metrics,
            &mut vm_first_gases,
            "vm:curve",
            "pool-d",
            Some(550_000),
        );
        record_vm_first_gas_metrics(
            &mut metrics,
            &mut vm_first_gases,
            "vm:curve",
            "pool-e",
            Some(580_000),
        );

        finalize_vm_first_gas_metrics(&mut metrics, &mut vm_first_gases);

        assert_eq!(metrics.vm_completed_pools, 5);
        assert_eq!(metrics.vm_low_first_gas_count, 4);
        assert_eq!(metrics.vm_median_first_gas, Some(550_000));
        let ratio = metrics.vm_low_first_gas_ratio.expect("ratio should be set");
        assert!((ratio - 0.8).abs() < 1e-9);
        assert_eq!(
            metrics.vm_low_first_gas_samples.len(),
            VM_LOW_FIRST_GAS_SAMPLE_CAP
        );
        assert_eq!(
            metrics.vm_low_first_gas_samples[0],
            "vm:curve:pool-a:400000".to_string()
        );
    }

    #[test]
    fn vm_first_gas_metrics_ignore_non_vm_protocols() {
        let mut metrics = QuoteMetrics::default();
        let mut vm_first_gases = Vec::new();

        record_vm_first_gas_metrics(
            &mut metrics,
            &mut vm_first_gases,
            "uniswap_v3",
            "pool-native",
            Some(120_000),
        );
        finalize_vm_first_gas_metrics(&mut metrics, &mut vm_first_gases);

        assert_eq!(metrics.vm_completed_pools, 0);
        assert_eq!(metrics.vm_median_first_gas, None);
        assert_eq!(metrics.vm_low_first_gas_ratio, None);
        assert!(metrics.vm_low_first_gas_samples.is_empty());
    }

    #[test]
    fn compute_gas_in_sell_base_units_uses_floor_and_decimals() {
        assert_eq!(
            compute_gas_in_sell_base_units(Some(2_000_000_000_000_000_000), Some(1), Some(1.5), 6),
            "3000000"
        );
        assert_eq!(
            compute_gas_in_sell_base_units(
                Some(3_000_000_000_000_000_000),
                Some(5),
                Some(1.234567),
                2
            ),
            "1851"
        );
    }

    #[test]
    fn compute_gas_in_sell_base_units_returns_zero_for_invalid_inputs() {
        assert_eq!(
            compute_gas_in_sell_base_units(None, Some(1), Some(1.5), 6),
            "0"
        );
        assert_eq!(
            compute_gas_in_sell_base_units(Some(42), None, Some(1.5), 6),
            "0"
        );
        assert_eq!(
            compute_gas_in_sell_base_units(Some(42), Some(1), None, 6),
            "0"
        );
        assert_eq!(
            compute_gas_in_sell_base_units(Some(42), Some(1), Some(f64::NAN), 6),
            "0"
        );
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct LimitCountingSim {
        max_in: BigUint,
        #[serde(skip, default = "default_calls")]
        calls: Arc<AtomicUsize>,
    }

    #[typetag::serde]
    impl ProtocolSim for LimitCountingSim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(&self, _base: &Token, _quote: &Token) -> Result<f64, SimulationError> {
            Ok(0.0)
        }

        fn get_amount_out(
            &self,
            amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<GetAmountOutResult, SimulationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if amount_in > self.max_in {
                return Err(SimulationError::InvalidInput(
                    "amount_in exceeds get_limits max_in".to_string(),
                    None,
                ));
            }
            Ok(GetAmountOutResult::new(
                BigUint::zero(),
                BigUint::zero(),
                self.clone_box(),
            ))
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<(BigUint, BigUint), SimulationError> {
            Ok((self.max_in.clone(), BigUint::zero()))
        }

        fn delta_transition(
            &mut self,
            _delta: ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &Balances,
        ) -> Result<(), TransitionError<String>> {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(self.clone())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other.as_any().is::<Self>()
        }
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct ConservativeLimitSim {
        reported_max_in: BigUint,
        actual_max_in: BigUint,
        #[serde(skip, default = "default_calls")]
        calls: Arc<AtomicUsize>,
    }

    #[typetag::serde]
    impl ProtocolSim for ConservativeLimitSim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(&self, _base: &Token, _quote: &Token) -> Result<f64, SimulationError> {
            Ok(0.0)
        }

        fn get_amount_out(
            &self,
            amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<GetAmountOutResult, SimulationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if amount_in > self.actual_max_in {
                return Err(SimulationError::InvalidInput(
                    format!("Sell amount exceeds limit {}", self.actual_max_in),
                    None,
                ));
            }
            Ok(GetAmountOutResult::new(
                BigUint::zero(),
                BigUint::zero(),
                self.clone_box(),
            ))
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<(BigUint, BigUint), SimulationError> {
            Ok((self.reported_max_in.clone(), BigUint::zero()))
        }

        fn delta_transition(
            &mut self,
            _delta: ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &Balances,
        ) -> Result<(), TransitionError<String>> {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(self.clone())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other.as_any().is::<Self>()
        }
    }

    fn make_token(address: &Bytes, symbol: &str) -> Token {
        Token::new(address, symbol, 18, 0, &[], Chain::Ethereum, 100)
    }

    fn make_token_with_decimals(address: &Bytes, symbol: &str, decimals: u32) -> Token {
        Token::new(address, symbol, decimals, 0, &[], Chain::Ethereum, 100)
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct SpotPriceCountingSim {
        spot_price_value: Option<f64>,
        max_in: u64,
        #[serde(skip, default = "default_calls")]
        spot_price_calls: Arc<AtomicUsize>,
    }

    #[typetag::serde]
    impl ProtocolSim for SpotPriceCountingSim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(&self, _base: &Token, _quote: &Token) -> Result<f64, SimulationError> {
            self.spot_price_calls.fetch_add(1, Ordering::SeqCst);
            self.spot_price_value
                .ok_or_else(|| SimulationError::FatalError("spot unavailable".to_string()))
        }

        fn get_amount_out(
            &self,
            amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<GetAmountOutResult, SimulationError> {
            Ok(GetAmountOutResult::new(
                amount_in.clone(),
                amount_in,
                self.clone_box(),
            ))
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<(BigUint, BigUint), SimulationError> {
            Ok((BigUint::from(self.max_in), BigUint::zero()))
        }

        fn delta_transition(
            &mut self,
            _delta: ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &Balances,
        ) -> Result<(), TransitionError<String>> {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(self.clone())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other.as_any().is::<Self>()
        }
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct PairAwareSpotPriceSim {
        sell_token: Bytes,
        wrapped_native: Bytes,
        native_token: Bytes,
        wrapped_price: f64,
        native_price: f64,
    }

    #[typetag::serde]
    impl ProtocolSim for PairAwareSpotPriceSim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(&self, base: &Token, quote: &Token) -> Result<f64, SimulationError> {
            if quote.address == self.sell_token && base.address == self.wrapped_native {
                return Ok(self.wrapped_price);
            }
            if quote.address == self.sell_token && base.address == self.native_token {
                return Ok(self.native_price);
            }
            Err(SimulationError::FatalError(
                "pair not supported".to_string(),
            ))
        }

        fn get_amount_out(
            &self,
            amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<GetAmountOutResult, SimulationError> {
            Ok(GetAmountOutResult::new(
                amount_in.clone(),
                amount_in,
                self.clone_box(),
            ))
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<(BigUint, BigUint), SimulationError> {
            Ok((BigUint::from(1_000_000u64), BigUint::zero()))
        }

        fn delta_transition(
            &mut self,
            _delta: ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &Balances,
        ) -> Result<(), TransitionError<String>> {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(self.clone())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other.as_any().is::<Self>()
        }
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct StrictPairTokenSim {
        expected_sell: Bytes,
        expected_buy: Bytes,
        #[serde(skip, default = "default_calls")]
        limit_calls: Arc<AtomicUsize>,
        #[serde(skip, default = "default_calls")]
        quote_calls: Arc<AtomicUsize>,
    }

    #[typetag::serde]
    impl ProtocolSim for StrictPairTokenSim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(&self, _base: &Token, _quote: &Token) -> Result<f64, SimulationError> {
            Err(SimulationError::FatalError("spot unavailable".to_string()))
        }

        fn get_amount_out(
            &self,
            amount_in: BigUint,
            token_in: &Token,
            token_out: &Token,
        ) -> Result<GetAmountOutResult, SimulationError> {
            self.quote_calls.fetch_add(1, Ordering::SeqCst);
            if token_in.address != self.expected_sell || token_out.address != self.expected_buy {
                return Err(SimulationError::InvalidInput(
                    format!(
                        "unexpected token pair for quote: {} -> {}",
                        token_in.address, token_out.address
                    ),
                    None,
                ));
            }

            Ok(GetAmountOutResult::new(
                amount_in.clone(),
                BigUint::from(21_000u64),
                self.clone_box(),
            ))
        }

        fn get_limits(
            &self,
            sell_token: Bytes,
            buy_token: Bytes,
        ) -> Result<(BigUint, BigUint), SimulationError> {
            self.limit_calls.fetch_add(1, Ordering::SeqCst);
            if sell_token != self.expected_sell || buy_token != self.expected_buy {
                return Err(SimulationError::InvalidInput(
                    format!(
                        "unexpected token pair for limits: {} -> {}",
                        sell_token, buy_token
                    ),
                    None,
                ));
            }

            Ok((BigUint::from(1_000_000u64), BigUint::zero()))
        }

        fn delta_transition(
            &mut self,
            _delta: ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &Balances,
        ) -> Result<(), TransitionError<String>> {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(self.clone())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other.as_any().is::<Self>()
        }
    }

    #[test]
    fn resolve_request_eth_to_sell_spot_price_returns_one_for_wrapped_native_sell() {
        let sell_token = Chain::Ethereum.wrapped_native_token();
        let native_candidates: Vec<(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> =
            Vec::new();
        let vm_candidates: Vec<(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> = Vec::new();

        let price =
            resolve_request_eth_to_sell_spot_price(&sell_token, &native_candidates, &vm_candidates);

        assert_eq!(price, Some(1.0));
    }

    #[test]
    fn resolve_request_eth_to_sell_spot_price_prefers_wrapped_native_source() {
        let sell_token = make_token_with_decimals(
            &Bytes::from_str("0x00000000000000000000000000000000000000a1").expect("valid address"),
            "USDT",
            6,
        );
        let wrapped_native = Chain::Ethereum.wrapped_native_token();
        let native_token = Chain::Ethereum.native_token();

        let component = ProtocolComponent::new(
            Bytes::from_str("0x00000000000000000000000000000000000000b1")
                .expect("valid pool address"),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![wrapped_native.clone(), sell_token.clone()],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );
        let sim = PairAwareSpotPriceSim {
            sell_token: sell_token.address.clone(),
            wrapped_native: wrapped_native.address.clone(),
            native_token: native_token.address.clone(),
            wrapped_price: 2_500.0,
            native_price: 552_709_307.0,
        };

        let native_candidates: Vec<(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> =
            vec![(
                "pool-a".to_string(),
                Arc::new(sim) as Arc<dyn ProtocolSim>,
                Arc::new(component),
            )];
        let vm_candidates: Vec<(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> = Vec::new();

        let price =
            resolve_request_eth_to_sell_spot_price(&sell_token, &native_candidates, &vm_candidates)
                .expect("spot price should resolve");

        assert!((price - 2_500.0).abs() < f64::EPSILON);
    }

    #[test]
    fn resolve_request_eth_to_sell_spot_price_uses_most_liquid_pool() {
        let sell_token = make_token_with_decimals(
            &Bytes::from_str("0x00000000000000000000000000000000000000b2").expect("valid address"),
            "WBTC",
            8,
        );
        let wrapped_native = Chain::Ethereum.wrapped_native_token();
        let low_liquidity_calls = Arc::new(AtomicUsize::new(0));
        let high_liquidity_calls = Arc::new(AtomicUsize::new(0));

        let component_a = ProtocolComponent::new(
            Bytes::from_str("0x00000000000000000000000000000000000000c1")
                .expect("valid pool address"),
            "uniswap_v4".to_string(),
            "uniswap_v4".to_string(),
            Chain::Ethereum,
            vec![wrapped_native.clone(), sell_token.clone()],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );
        let component_b = ProtocolComponent::new(
            Bytes::from_str("0x00000000000000000000000000000000000000c2")
                .expect("valid pool address"),
            "uniswap_v3".to_string(),
            "uniswap_v3".to_string(),
            Chain::Ethereum,
            vec![wrapped_native.clone(), sell_token.clone()],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let outlier = SpotPriceCountingSim {
            spot_price_value: Some(1.0e18),
            max_in: 10,
            spot_price_calls: Arc::clone(&low_liquidity_calls),
        };
        let consensus = SpotPriceCountingSim {
            spot_price_value: Some(0.03125),
            max_in: 1_000_000_000,
            spot_price_calls: Arc::clone(&high_liquidity_calls),
        };

        // Keep ids ordered so the low-liquidity outlier candidate is visited first.
        let native_candidates: Vec<(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> = vec![
            (
                "a-outlier".to_string(),
                Arc::new(outlier) as Arc<dyn ProtocolSim>,
                Arc::new(component_a),
            ),
            (
                "b-liquid".to_string(),
                Arc::new(consensus) as Arc<dyn ProtocolSim>,
                Arc::new(component_b),
            ),
        ];
        let vm_candidates: Vec<(String, Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> = Vec::new();

        let price =
            resolve_request_eth_to_sell_spot_price(&sell_token, &native_candidates, &vm_candidates)
                .expect("spot price should resolve");

        assert!((price - 0.03125).abs() < f64::EPSILON);
        assert_eq!(low_liquidity_calls.load(Ordering::SeqCst), 0);
        assert_eq!(high_liquidity_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn get_amounts_out_uses_request_scoped_gas_snapshot_across_all_pools() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");
        let wrapped_native = Chain::Ethereum.wrapped_native_token().address;

        let token_in_meta = make_token_with_decimals(&token_in, "TK1", 2);
        let token_out_meta = make_token_with_decimals(&token_out, "TK2", 6);
        let wrapped_native_meta = make_token_with_decimals(&wrapped_native, "WETH", 18);

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(token_in.clone(), token_in_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());
        initial_tokens.insert(wrapped_native.clone(), wrapped_native_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let pair_spot_price_calls = Arc::new(AtomicUsize::new(0));
        let spot_source_calls = Arc::new(AtomicUsize::new(0));
        let mut states = HashMap::new();
        let mut new_pairs = HashMap::new();
        for (pool_id, pool_address_hex) in [
            ("pool-a", "0x0000000000000000000000000000000000000011"),
            ("pool-b", "0x0000000000000000000000000000000000000012"),
        ] {
            let component = ProtocolComponent::new(
                Bytes::from_str(pool_address_hex).expect("valid pool address"),
                "uniswap_v2".to_string(),
                "uniswap_v2".to_string(),
                Chain::Ethereum,
                vec![token_in_meta.clone(), token_out_meta.clone()],
                Vec::new(),
                HashMap::new(),
                Bytes::default(),
                NaiveDateTime::default(),
            );
            states.insert(
                pool_id.to_string(),
                Box::new(SpotPriceCountingSim {
                    spot_price_value: Some(1.5),
                    max_in: 1_000_000,
                    spot_price_calls: Arc::clone(&pair_spot_price_calls),
                }) as Box<dyn ProtocolSim>,
            );
            new_pairs.insert(pool_id.to_string(), component);
        }
        states.insert(
            "pool-spot".to_string(),
            Box::new(SpotPriceCountingSim {
                spot_price_value: Some(1.5),
                max_in: 10_000_000,
                spot_price_calls: Arc::clone(&spot_source_calls),
            }) as Box<dyn ProtocolSim>,
        );
        new_pairs.insert(
            "pool-spot".to_string(),
            ProtocolComponent::new(
                Bytes::from_str("0x0000000000000000000000000000000000000013")
                    .expect("valid pool address"),
                "uniswap_v2".to_string(),
                "uniswap_v2".to_string(),
                Chain::Ethereum,
                vec![token_in_meta.clone(), wrapped_native_meta.clone()],
                Vec::new(),
                HashMap::new(),
                Bytes::default(),
                NaiveDateTime::default(),
            ),
        );

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(Some(
                5_000_000_000_000_000_000,
            ))),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(true)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(2)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 2,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-spot-reuse".to_string(),
            auction_id: None,
            token_in: token_in_hex.to_string(),
            token_out: token_out_hex.to_string(),
            amounts: vec!["2".to_string(), "5".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;

        assert_eq!(pair_spot_price_calls.load(Ordering::SeqCst), 0);
        assert_eq!(spot_source_calls.load(Ordering::SeqCst), 1);
        assert_eq!(computation.responses.len(), 2);
        let unique_gas_in_sell: HashSet<&str> = computation
            .responses
            .iter()
            .map(|response| response.gas_in_sell.as_str())
            .collect();
        assert_eq!(unique_gas_in_sell.len(), 1);
        for response in &computation.responses {
            assert_eq!(response.gas_used, vec![2, 5]);
            assert_eq!(response.gas_in_sell, "3750");
        }
    }

    #[tokio::test]
    async fn get_amounts_out_remaps_wrapped_request_tokens_for_native_only_pool() {
        let wrapped_native = Chain::Ethereum.wrapped_native_token().address;
        let native = Chain::Ethereum.native_token().address;
        let token_out =
            Bytes::from_str("0x0000000000000000000000000000000000000002").expect("valid address");

        let wrapped_meta = make_token_with_decimals(&wrapped_native, "WETH", 18);
        let native_meta = make_token_with_decimals(&native, "ETH", 18);
        let token_out_meta = make_token_with_decimals(&token_out, "TK2", 6);

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(wrapped_native.clone(), wrapped_meta.clone());
        initial_tokens.insert(native.clone(), native_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let limit_calls = Arc::new(AtomicUsize::new(0));
        let quote_calls = Arc::new(AtomicUsize::new(0));
        let mut states = HashMap::new();
        let mut new_pairs = HashMap::new();
        states.insert(
            "pool-native".to_string(),
            Box::new(StrictPairTokenSim {
                expected_sell: native.clone(),
                expected_buy: token_out.clone(),
                limit_calls: Arc::clone(&limit_calls),
                quote_calls: Arc::clone(&quote_calls),
            }) as Box<dyn ProtocolSim>,
        );
        new_pairs.insert(
            "pool-native".to_string(),
            ProtocolComponent::new(
                Bytes::from_str("0x0000000000000000000000000000000000000011")
                    .expect("valid pool address"),
                "rocketpool".to_string(),
                "rocketpool".to_string(),
                Chain::Ethereum,
                vec![native_meta, token_out_meta],
                Vec::new(),
                HashMap::new(),
                Bytes::default(),
                NaiveDateTime::default(),
            ),
        );

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-remap-native-only".to_string(),
            auction_id: None,
            token_in: wrapped_native.to_string(),
            token_out: token_out.to_string(),
            amounts: vec!["2".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;

        assert_eq!(quote_calls.load(Ordering::SeqCst), 1);
        assert!(!computation.responses.is_empty());
        assert_eq!(computation.responses[0].amounts_out, vec!["2".to_string()]);
        assert!(computation.meta.failures.is_empty());
        assert!(matches!(computation.meta.status, QuoteStatus::Ready));
        assert!(limit_calls.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn get_amounts_out_keeps_wrapped_request_tokens_for_wrapped_only_pool() {
        let wrapped_native = Chain::Ethereum.wrapped_native_token().address;
        let token_out =
            Bytes::from_str("0x0000000000000000000000000000000000000003").expect("valid address");

        let wrapped_meta = make_token_with_decimals(&wrapped_native, "WETH", 18);
        let token_out_meta = make_token_with_decimals(&token_out, "TK3", 6);

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(wrapped_native.clone(), wrapped_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let quote_calls = Arc::new(AtomicUsize::new(0));
        let mut states = HashMap::new();
        let mut new_pairs = HashMap::new();
        states.insert(
            "pool-wrapped".to_string(),
            Box::new(StrictPairTokenSim {
                expected_sell: wrapped_native.clone(),
                expected_buy: token_out.clone(),
                limit_calls: Arc::new(AtomicUsize::new(0)),
                quote_calls: Arc::clone(&quote_calls),
            }) as Box<dyn ProtocolSim>,
        );
        new_pairs.insert(
            "pool-wrapped".to_string(),
            ProtocolComponent::new(
                Bytes::from_str("0x0000000000000000000000000000000000000012")
                    .expect("valid pool address"),
                "uniswap_v2".to_string(),
                "uniswap_v2".to_string(),
                Chain::Ethereum,
                vec![wrapped_meta, token_out_meta],
                Vec::new(),
                HashMap::new(),
                Bytes::default(),
                NaiveDateTime::default(),
            ),
        );

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-keep-wrapped".to_string(),
            auction_id: None,
            token_in: wrapped_native.to_string(),
            token_out: token_out.to_string(),
            amounts: vec!["3".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;

        assert_eq!(quote_calls.load(Ordering::SeqCst), 1);
        assert_eq!(computation.responses.len(), 1);
        assert_eq!(computation.responses[0].amounts_out, vec!["3".to_string()]);
        assert!(computation.meta.failures.is_empty());
    }

    #[tokio::test]
    async fn get_amounts_out_remaps_wrapped_request_tokens_for_native_only_vm_pool() {
        let wrapped_native = Chain::Ethereum.wrapped_native_token().address;
        let native = Chain::Ethereum.native_token().address;
        let token_out =
            Bytes::from_str("0x0000000000000000000000000000000000000004").expect("valid address");

        let wrapped_meta = make_token_with_decimals(&wrapped_native, "WETH", 18);
        let native_meta = make_token_with_decimals(&native, "ETH", 18);
        let token_out_meta = make_token_with_decimals(&token_out, "TK4", 6);

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(wrapped_native.clone(), wrapped_meta.clone());
        initial_tokens.insert(native.clone(), native_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        // Keep native store ready without creating any matching pools for the request pair.
        let native_dummy_token_a =
            Bytes::from_str("0x0000000000000000000000000000000000000010").expect("valid address");
        let native_dummy_token_b =
            Bytes::from_str("0x0000000000000000000000000000000000000011").expect("valid address");
        let native_dummy_meta_a = make_token_with_decimals(&native_dummy_token_a, "NTA", 18);
        let native_dummy_meta_b = make_token_with_decimals(&native_dummy_token_b, "NTB", 18);
        let mut native_states = HashMap::new();
        let mut native_pairs = HashMap::new();
        native_states.insert(
            "native-dummy".to_string(),
            Box::new(SpotPriceCountingSim {
                spot_price_value: Some(1.0),
                max_in: 1_000_000,
                spot_price_calls: Arc::new(AtomicUsize::new(0)),
            }) as Box<dyn ProtocolSim>,
        );
        native_pairs.insert(
            "native-dummy".to_string(),
            ProtocolComponent::new(
                Bytes::from_str("0x0000000000000000000000000000000000000013")
                    .expect("valid pool address"),
                "uniswap_v2".to_string(),
                "uniswap_v2".to_string(),
                Chain::Ethereum,
                vec![native_dummy_meta_a, native_dummy_meta_b],
                Vec::new(),
                HashMap::new(),
                Bytes::default(),
                NaiveDateTime::default(),
            ),
        );
        native_state_store
            .apply_update(Update::new(1, native_states, native_pairs))
            .await;

        let vm_limit_calls = Arc::new(AtomicUsize::new(0));
        let vm_quote_calls = Arc::new(AtomicUsize::new(0));
        let mut vm_states = HashMap::new();
        let mut vm_pairs = HashMap::new();
        vm_states.insert(
            "vm-native".to_string(),
            Box::new(StrictPairTokenSim {
                expected_sell: native.clone(),
                expected_buy: token_out.clone(),
                limit_calls: Arc::clone(&vm_limit_calls),
                quote_calls: Arc::clone(&vm_quote_calls),
            }) as Box<dyn ProtocolSim>,
        );
        vm_pairs.insert(
            "vm-native".to_string(),
            ProtocolComponent::new(
                Bytes::from_str("0x0000000000000000000000000000000000000014")
                    .expect("valid pool address"),
                "vm:curve".to_string(),
                "curve_pool".to_string(),
                Chain::Ethereum,
                vec![native_meta, token_out_meta],
                Vec::new(),
                HashMap::new(),
                Bytes::default(),
                NaiveDateTime::default(),
            ),
        );
        vm_state_store
            .apply_update(Update::new(2, vm_states, vm_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: true,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-remap-vm-native-only".to_string(),
            auction_id: None,
            token_in: wrapped_native.to_string(),
            token_out: token_out.to_string(),
            amounts: vec!["2".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;

        assert!(vm_limit_calls.load(Ordering::SeqCst) >= 1);
        assert_eq!(vm_quote_calls.load(Ordering::SeqCst), 1);
        assert_eq!(computation.metrics.scheduled_vm_pools, 1);
        assert_eq!(computation.responses.len(), 1);
        assert_eq!(computation.responses[0].amounts_out, vec!["2".to_string()]);
        assert!(computation.meta.failures.is_empty());
        assert!(matches!(computation.meta.status, QuoteStatus::Ready));
    }

    #[tokio::test]
    async fn get_amounts_out_sets_gas_in_sell_to_zero_when_reporting_disabled() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");
        let wrapped_native = Chain::Ethereum.wrapped_native_token().address;

        let token_in_meta = make_token_with_decimals(&token_in, "TK1", 6);
        let token_out_meta = make_token_with_decimals(&token_out, "TK2", 6);
        let wrapped_native_meta = make_token_with_decimals(&wrapped_native, "WETH", 18);

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(token_in.clone(), token_in_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());
        initial_tokens.insert(wrapped_native.clone(), wrapped_native_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let mut states = HashMap::new();
        let mut new_pairs = HashMap::new();
        states.insert(
            "pool-a".to_string(),
            Box::new(SpotPriceCountingSim {
                spot_price_value: Some(1.5),
                max_in: 1_000_000,
                spot_price_calls: Arc::new(AtomicUsize::new(0)),
            }) as Box<dyn ProtocolSim>,
        );
        new_pairs.insert(
            "pool-a".to_string(),
            ProtocolComponent::new(
                Bytes::from_str("0x0000000000000000000000000000000000000011")
                    .expect("valid pool address"),
                "uniswap_v2".to_string(),
                "uniswap_v2".to_string(),
                Chain::Ethereum,
                vec![token_in_meta.clone(), token_out_meta.clone()],
                Vec::new(),
                HashMap::new(),
                Bytes::default(),
                NaiveDateTime::default(),
            ),
        );
        states.insert(
            "pool-spot".to_string(),
            Box::new(SpotPriceCountingSim {
                spot_price_value: Some(1.5),
                max_in: 10_000_000,
                spot_price_calls: Arc::new(AtomicUsize::new(0)),
            }) as Box<dyn ProtocolSim>,
        );
        new_pairs.insert(
            "pool-spot".to_string(),
            ProtocolComponent::new(
                Bytes::from_str("0x0000000000000000000000000000000000000013")
                    .expect("valid pool address"),
                "uniswap_v2".to_string(),
                "uniswap_v2".to_string(),
                Chain::Ethereum,
                vec![token_in_meta.clone(), wrapped_native_meta.clone()],
                Vec::new(),
                HashMap::new(),
                Bytes::default(),
                NaiveDateTime::default(),
            ),
        );

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(Some(
                5_000_000_000_000_000_000,
            ))),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-reporting-disabled".to_string(),
            auction_id: None,
            token_in: token_in_hex.to_string(),
            token_out: token_out_hex.to_string(),
            amounts: vec!["2".to_string(), "5".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;

        assert_eq!(computation.responses.len(), 1);
        assert_eq!(computation.responses[0].gas_used, vec![2, 5]);
        assert_eq!(computation.responses[0].gas_in_sell, "0");
    }

    #[tokio::test]
    async fn get_amounts_out_sets_gas_in_sell_to_zero_when_spot_price_fails() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let token_in_meta = make_token_with_decimals(&token_in, "TK1", 2);
        let token_out_meta = make_token_with_decimals(&token_out, "TK2", 6);

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(token_in.clone(), token_in_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let spot_price_calls = Arc::new(AtomicUsize::new(0));
        let mut states = HashMap::new();
        states.insert(
            "pool-a".to_string(),
            Box::new(SpotPriceCountingSim {
                spot_price_value: None,
                max_in: 1_000_000,
                spot_price_calls: Arc::clone(&spot_price_calls),
            }) as Box<dyn ProtocolSim>,
        );
        let mut new_pairs = HashMap::new();
        new_pairs.insert(
            "pool-a".to_string(),
            ProtocolComponent::new(
                Bytes::from_str("0x0000000000000000000000000000000000000011")
                    .expect("valid pool address"),
                "uniswap_v2".to_string(),
                "uniswap_v2".to_string(),
                Chain::Ethereum,
                vec![token_in_meta.clone(), token_out_meta.clone()],
                Vec::new(),
                HashMap::new(),
                Bytes::default(),
                NaiveDateTime::default(),
            ),
        );

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(Some(
                5_000_000_000_000_000_000,
            ))),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(true)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-spot-failure".to_string(),
            auction_id: None,
            token_in: token_in_hex.to_string(),
            token_out: token_out_hex.to_string(),
            amounts: vec!["2".to_string(), "5".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;

        assert_eq!(spot_price_calls.load(Ordering::SeqCst), 0);
        assert_eq!(computation.responses.len(), 1);
        assert_eq!(computation.responses[0].gas_used, vec![2, 5]);
        assert_eq!(computation.responses[0].gas_in_sell, "0");
    }

    #[tokio::test]
    async fn vm_unavailable_is_false_when_vm_pools_disabled() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(token_in.clone(), make_token(&token_in, "TK1"));
        initial_tokens.insert(token_out.clone(), make_token(&token_out, "TK2"));

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        // Ensure the native store is ready so `get_amounts_out` reaches the vm readiness logic
        // instead of returning early with `QuoteStatus::WarmingUp`.
        let ready_token_a_hex = "0x0000000000000000000000000000000000000003";
        let ready_token_b_hex = "0x0000000000000000000000000000000000000004";
        let ready_token_a = Bytes::from_str(ready_token_a_hex).expect("valid address");
        let ready_token_b = Bytes::from_str(ready_token_b_hex).expect("valid address");
        let ready_component = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000009").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![
                make_token(&ready_token_a, "RDY1"),
                make_token(&ready_token_b, "RDY2"),
            ],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );
        let mut ready_states = HashMap::new();
        ready_states.insert(
            "pool-ready".to_string(),
            Box::new(LimitCountingSim {
                max_in: BigUint::from(1u8),
                calls: default_calls(),
            }) as Box<dyn ProtocolSim>,
        );
        let mut ready_pairs = HashMap::new();
        ready_pairs.insert("pool-ready".to_string(), ready_component);
        native_state_store
            .apply_update(Update::new(1, ready_states, ready_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store,
            vm_state_store,
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-vm-disabled".to_string(),
            auction_id: None,
            token_in: token_in_hex.to_string(),
            token_out: token_out_hex.to_string(),
            amounts: vec!["1".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;
        assert!(matches!(computation.meta.status, QuoteStatus::NoLiquidity));
        assert!(!computation.meta.vm_unavailable);
    }

    #[tokio::test]
    async fn skips_pool_when_request_exceeds_limits_and_max_probe_fails() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let token_in_meta = make_token(&token_in, "TK1");
        let token_out_meta = make_token(&token_out, "TK2");

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(token_in.clone(), token_in_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let pool_id = "pool-1".to_string();
        let component = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000009").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_in_meta, token_out_meta],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let calls = Arc::new(AtomicUsize::new(0));
        let sim = LimitCountingSim {
            max_in: BigUint::zero(),
            calls: Arc::clone(&calls),
        };

        let mut states = HashMap::new();
        states.insert(pool_id.clone(), Box::new(sim) as Box<dyn ProtocolSim>);
        let mut new_pairs = HashMap::new();
        new_pairs.insert(pool_id.clone(), component);

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-1".to_string(),
            auction_id: None,
            token_in: token_in_hex.to_string(),
            token_out: token_out_hex.to_string(),
            amounts: vec!["1".to_string(), "5".to_string(), "11".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(computation.responses.is_empty());
        assert!(matches!(computation.meta.status, QuoteStatus::NoLiquidity));
        assert_eq!(
            computation.meta.result_quality,
            QuoteResultQuality::NoResults
        );
        assert_eq!(computation.meta.failures.len(), 1);
        assert!(matches!(
            computation.meta.failures[0].kind,
            QuoteFailureKind::NoPools
        ));
        assert!(computation.meta.failures[0].message.contains("get_limits"));
        assert_eq!(computation.meta.pool_results.len(), 1);
        assert_eq!(
            computation.meta.pool_results[0].outcome,
            PoolOutcomeKind::SkippedPrecheck
        );
        assert_eq!(computation.metrics.skipped_native_limits, 1);
    }

    #[tokio::test]
    async fn does_not_skip_when_lower_ladder_amounts_are_within_limit() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let calls = Arc::new(AtomicUsize::new(0));
        let sim = LimitCountingSim {
            max_in: BigUint::from(10u8),
            calls: Arc::clone(&calls),
        };

        let permit = Arc::new(Semaphore::new(1))
            .acquire_owned()
            .await
            .expect("permit");

        let outcome = simulate_pool(
            "pool-1".to_string(),
            Arc::new(sim),
            "0x0000000000000000000000000000000000000009".to_string(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Arc::new(make_token(&token_in, "TK1")),
            Arc::new(make_token(&token_out, "TK2")),
            Arc::new(vec![BigUint::from(1u8), BigUint::from(11u8)]),
            BigUint::from(11u8),
            2,
            Instant::now() + Duration::from_secs(1),
            CancellationToken::new(),
            permit,
        )
        .await
        .expect("simulate_pool should return outcome");

        assert_eq!(calls.load(Ordering::SeqCst), 2, "probe + lower amount");
        match outcome {
            PoolSimOutcome::Simulated(result) => {
                assert_eq!(result.amounts_out.len(), 1);
                assert_eq!(result.gas_used.len(), 1);
                assert_eq!(result.errors.len(), 1);
                assert!(result.errors[0].contains("Probe quote failed"));
            }
            PoolSimOutcome::SkippedDueToLimits { .. } => {
                panic!("mixed ladder should still quote lower amounts")
            }
        }
    }

    #[tokio::test]
    async fn does_not_skip_when_all_amounts_exceed_reported_limit_but_some_are_quotable() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let calls = Arc::new(AtomicUsize::new(0));
        let sim = ConservativeLimitSim {
            reported_max_in: BigUint::from(10u8),
            actual_max_in: BigUint::from(15u8),
            calls: Arc::clone(&calls),
        };

        let permit = Arc::new(Semaphore::new(1))
            .acquire_owned()
            .await
            .expect("permit");

        let outcome = simulate_pool(
            "pool-1".to_string(),
            Arc::new(sim),
            "0x0000000000000000000000000000000000000009".to_string(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Arc::new(make_token(&token_in, "TK1")),
            Arc::new(make_token(&token_out, "TK2")),
            Arc::new(vec![BigUint::from(11u8), BigUint::from(20u8)]),
            BigUint::from(20u8),
            2,
            Instant::now() + Duration::from_secs(1),
            CancellationToken::new(),
            permit,
        )
        .await
        .expect("simulate_pool should return outcome");

        assert_eq!(calls.load(Ordering::SeqCst), 2, "probe + remaining amount");
        match outcome {
            PoolSimOutcome::Simulated(result) => {
                assert_eq!(result.amounts_out.len(), 1);
                assert_eq!(result.gas_used.len(), 1);
                assert_eq!(result.errors.len(), 1);
                assert!(result.errors[0]
                    .to_ascii_lowercase()
                    .contains("sell amount exceeds limit"));
            }
            PoolSimOutcome::SkippedDueToLimits { .. } => {
                panic!("min-amount probe should prevent conservative get_limits skip")
            }
        }
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct SoftLimitSim {
        max_in: BigUint,
        #[serde(skip, default = "default_calls")]
        calls: Arc<AtomicUsize>,
    }

    #[typetag::serde]
    impl ProtocolSim for SoftLimitSim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(&self, _base: &Token, _quote: &Token) -> Result<f64, SimulationError> {
            Ok(0.0)
        }

        fn get_amount_out(
            &self,
            _amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<GetAmountOutResult, SimulationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(GetAmountOutResult::new(
                BigUint::zero(),
                BigUint::zero(),
                self.clone_box(),
            ))
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<(BigUint, BigUint), SimulationError> {
            Ok((self.max_in.clone(), BigUint::zero()))
        }

        fn delta_transition(
            &mut self,
            _delta: ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &Balances,
        ) -> Result<(), TransitionError<String>> {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(self.clone())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other.as_any().is::<Self>()
        }
    }

    #[tokio::test]
    async fn does_not_skip_when_pool_can_quote_beyond_soft_limit() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let token_in_meta = make_token(&token_in, "TK1");
        let token_out_meta = make_token(&token_out, "TK2");

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(token_in.clone(), token_in_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let pool_id = "pool-1".to_string();
        let component = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000009").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_in_meta, token_out_meta],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let calls = Arc::new(AtomicUsize::new(0));
        let sim = SoftLimitSim {
            max_in: BigUint::from(10u8),
            calls: Arc::clone(&calls),
        };

        let mut states = HashMap::new();
        states.insert(pool_id.clone(), Box::new(sim) as Box<dyn ProtocolSim>);
        let mut new_pairs = HashMap::new();
        new_pairs.insert(pool_id.clone(), component);

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-2".to_string(),
            auction_id: None,
            token_in: token_in_hex.to_string(),
            token_out: token_out_hex.to_string(),
            amounts: vec!["1".to_string(), "11".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;
        assert!(matches!(computation.meta.status, QuoteStatus::Ready));
        assert_eq!(
            computation.meta.result_quality,
            QuoteResultQuality::Complete
        );
        assert!(computation.meta.failures.is_empty());
        assert!(computation.meta.pool_results.is_empty());
        assert_eq!(computation.metrics.skipped_native_limits, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(computation.responses.len(), 1);
        assert_eq!(computation.responses[0].amounts_out.len(), 2);
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct ProbeFatalSim {
        max_in: BigUint,
        #[serde(skip, default = "default_calls")]
        calls: Arc<AtomicUsize>,
    }

    #[typetag::serde]
    impl ProtocolSim for ProbeFatalSim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(&self, _base: &Token, _quote: &Token) -> Result<f64, SimulationError> {
            Ok(0.0)
        }

        fn get_amount_out(
            &self,
            amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<GetAmountOutResult, SimulationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if amount_in > self.max_in {
                return Err(SimulationError::FatalError("probe blew up".to_string()));
            }
            Ok(GetAmountOutResult::new(
                BigUint::zero(),
                BigUint::zero(),
                self.clone_box(),
            ))
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<(BigUint, BigUint), SimulationError> {
            Ok((self.max_in.clone(), BigUint::zero()))
        }

        fn delta_transition(
            &mut self,
            _delta: ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &Balances,
        ) -> Result<(), TransitionError<String>> {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(self.clone())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other.as_any().is::<Self>()
        }
    }

    #[tokio::test]
    async fn probe_fatal_error_is_not_reported_as_limits_skip() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let token_in_meta = make_token(&token_in, "TK1");
        let token_out_meta = make_token(&token_out, "TK2");

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(token_in.clone(), token_in_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let pool_id = "pool-1".to_string();
        let component = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000009").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_in_meta, token_out_meta],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let calls = Arc::new(AtomicUsize::new(0));
        let sim = ProbeFatalSim {
            max_in: BigUint::from(10u8),
            calls: Arc::clone(&calls),
        };

        let mut states = HashMap::new();
        states.insert(pool_id.clone(), Box::new(sim) as Box<dyn ProtocolSim>);
        let mut new_pairs = HashMap::new();
        new_pairs.insert(pool_id.clone(), component);

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-3".to_string(),
            auction_id: None,
            token_in: token_in_hex.to_string(),
            token_out: token_out_hex.to_string(),
            amounts: vec!["1".to_string(), "11".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "probe-only");
        assert!(computation.responses.is_empty());
        assert_eq!(
            computation.meta.result_quality,
            QuoteResultQuality::NoResults
        );
        assert!(matches!(
            computation.meta.status,
            QuoteStatus::PartialSuccess
        ));
        assert_eq!(computation.meta.failures.len(), 1);
        assert_eq!(computation.meta.pool_results.len(), 1);
        assert_eq!(
            computation.meta.pool_results[0].outcome,
            PoolOutcomeKind::SimulatorError
        );
        assert!(
            computation.meta.failures[0]
                .message
                .contains("Probe quote failed"),
            "message should explain this was a probe error"
        );
        assert_eq!(computation.metrics.skipped_native_limits, 0);
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct ProbeInvalidInputSim {
        max_in: BigUint,
        #[serde(skip, default = "default_calls")]
        calls: Arc<AtomicUsize>,
    }

    #[typetag::serde]
    impl ProtocolSim for ProbeInvalidInputSim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(&self, _base: &Token, _quote: &Token) -> Result<f64, SimulationError> {
            Ok(0.0)
        }

        fn get_amount_out(
            &self,
            amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<GetAmountOutResult, SimulationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if amount_in > self.max_in {
                return Err(SimulationError::InvalidInput(
                    "token_in invalid for probe".to_string(),
                    None,
                ));
            }
            Ok(GetAmountOutResult::new(
                BigUint::zero(),
                BigUint::zero(),
                self.clone_box(),
            ))
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<(BigUint, BigUint), SimulationError> {
            Ok((self.max_in.clone(), BigUint::zero()))
        }

        fn delta_transition(
            &mut self,
            _delta: ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &Balances,
        ) -> Result<(), TransitionError<String>> {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(self.clone())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other.as_any().is::<Self>()
        }
    }

    #[tokio::test]
    async fn probe_non_limit_invalid_input_continues_ladder() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let calls = Arc::new(AtomicUsize::new(0));
        let sim = ProbeInvalidInputSim {
            max_in: BigUint::from(10u8),
            calls: Arc::clone(&calls),
        };

        let permit = Arc::new(Semaphore::new(1))
            .acquire_owned()
            .await
            .expect("permit");

        let outcome = simulate_pool(
            "pool-1".to_string(),
            Arc::new(sim),
            "0x0000000000000000000000000000000000000009".to_string(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Arc::new(make_token(&token_in, "TK1")),
            Arc::new(make_token(&token_out, "TK2")),
            Arc::new(vec![BigUint::from(1u8), BigUint::from(11u8)]),
            BigUint::from(11u8),
            2,
            Instant::now() + Duration::from_secs(1),
            CancellationToken::new(),
            permit,
        )
        .await
        .expect("simulate_pool should return outcome");

        assert_eq!(calls.load(Ordering::SeqCst), 2, "probe + lower amount");
        match outcome {
            PoolSimOutcome::Simulated(result) => {
                assert_eq!(result.amounts_out.len(), 1);
                assert_eq!(result.gas_used.len(), 1);
                assert!(!result.timed_out);
                assert_eq!(result.errors.len(), 1);
                assert!(result.errors[0].contains("Probe quote failed"));
                assert!(result.errors[0].contains("Invalid input"));
            }
            PoolSimOutcome::SkippedDueToLimits { .. } => {
                panic!("non-limit invalid input should not be treated as limits skip")
            }
        }
    }

    #[tokio::test]
    async fn get_amounts_out_keeps_lower_quotes_on_non_limit_probe_error() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let token_in_meta = make_token(&token_in, "TK1");
        let token_out_meta = make_token(&token_out, "TK2");

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(token_in.clone(), token_in_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let pool_id = "pool-1".to_string();
        let component = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000009").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_in_meta, token_out_meta],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let calls = Arc::new(AtomicUsize::new(0));
        let sim = ProbeInvalidInputSim {
            max_in: BigUint::from(10u8),
            calls: Arc::clone(&calls),
        };

        let mut states = HashMap::new();
        states.insert(pool_id.clone(), Box::new(sim) as Box<dyn ProtocolSim>);
        let mut new_pairs = HashMap::new();
        new_pairs.insert(pool_id, component);

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-non-limit-probe".to_string(),
            auction_id: None,
            token_in: token_in_hex.to_string(),
            token_out: token_out_hex.to_string(),
            amounts: vec!["1".to_string(), "11".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;

        assert_eq!(calls.load(Ordering::SeqCst), 2, "probe + lower amount");
        assert_eq!(computation.responses.len(), 1);
        assert_eq!(computation.responses[0].amounts_out.len(), 1);
        assert!(matches!(
            computation.meta.status,
            QuoteStatus::PartialSuccess
        ));
        assert_eq!(computation.meta.result_quality, QuoteResultQuality::Partial);
        assert_eq!(computation.meta.failures.len(), 1);
        assert!(matches!(
            computation.meta.failures[0].kind,
            QuoteFailureKind::InconsistentResult
        ));
        assert_eq!(computation.meta.pool_results.len(), 1);
        assert_eq!(
            computation.meta.pool_results[0].outcome,
            PoolOutcomeKind::SimulatorError
        );
        assert_eq!(computation.metrics.skipped_native_limits, 0);
    }

    #[tokio::test]
    async fn mixed_outcomes_set_partial_quality_without_changing_status_behavior() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let token_in_meta = make_token(&token_in, "TK1");
        let token_out_meta = make_token(&token_out, "TK2");

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(token_in.clone(), token_in_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let component_good = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000011").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_in_meta.clone(), token_out_meta.clone()],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );
        let component_bad = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000012").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_in_meta, token_out_meta],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let mut states = HashMap::new();
        states.insert(
            "pool-good".to_string(),
            Box::new(SoftLimitSim {
                max_in: BigUint::from(100u8),
                calls: default_calls(),
            }) as Box<dyn ProtocolSim>,
        );
        states.insert(
            "pool-bad".to_string(),
            Box::new(ProbeFatalSim {
                max_in: BigUint::from(10u8),
                calls: default_calls(),
            }) as Box<dyn ProtocolSim>,
        );
        let mut new_pairs = HashMap::new();
        new_pairs.insert("pool-good".to_string(), component_good);
        new_pairs.insert("pool-bad".to_string(), component_bad);

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(2)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 2,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-mixed".to_string(),
            auction_id: None,
            token_in: token_in_hex.to_string(),
            token_out: token_out_hex.to_string(),
            amounts: vec!["1".to_string(), "11".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;
        assert_eq!(computation.responses.len(), 1);
        assert_eq!(computation.meta.result_quality, QuoteResultQuality::Partial);
        assert!(matches!(
            computation.meta.status,
            QuoteStatus::PartialSuccess
        ));
        assert!(computation
            .meta
            .pool_results
            .iter()
            .any(|outcome| outcome.outcome == PoolOutcomeKind::SimulatorError));
    }

    #[tokio::test]
    async fn records_skipped_concurrency_outcomes_in_pool_results() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let token_in_meta = make_token(&token_in, "TK1");
        let token_out_meta = make_token(&token_out, "TK2");

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(token_in.clone(), token_in_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let component = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000021").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_in_meta, token_out_meta],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let mut states = HashMap::new();
        states.insert(
            "pool-1".to_string(),
            Box::new(SoftLimitSim {
                max_in: BigUint::from(100u8),
                calls: default_calls(),
            }) as Box<dyn ProtocolSim>,
        );
        let mut new_pairs = HashMap::new();
        new_pairs.insert("pool-1".to_string(), component);

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(0)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 0,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-skip-concurrency".to_string(),
            auction_id: None,
            token_in: token_in_hex.to_string(),
            token_out: token_out_hex.to_string(),
            amounts: vec!["1".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;
        assert!(computation.responses.is_empty());
        assert_eq!(
            computation.meta.result_quality,
            QuoteResultQuality::NoResults
        );
        assert!(matches!(
            computation.meta.status,
            QuoteStatus::PartialSuccess
        ));
        assert_eq!(computation.metrics.skipped_native_concurrency, 1);
        assert!(computation
            .meta
            .pool_results
            .iter()
            .any(|outcome| outcome.outcome == PoolOutcomeKind::SkippedConcurrency));
        assert!(computation
            .meta
            .failures
            .iter()
            .any(|failure| matches!(failure.kind, QuoteFailureKind::ConcurrencyLimit)));
    }

    #[tokio::test]
    async fn records_skipped_deadline_outcomes_in_pool_results() {
        let token_in_hex = "0x0000000000000000000000000000000000000001";
        let token_out_hex = "0x0000000000000000000000000000000000000002";
        let token_in = Bytes::from_str(token_in_hex).expect("valid address");
        let token_out = Bytes::from_str(token_out_hex).expect("valid address");

        let token_in_meta = make_token(&token_in, "TK1");
        let token_out_meta = make_token(&token_out, "TK2");

        let mut initial_tokens = HashMap::new();
        initial_tokens.insert(token_in.clone(), token_in_meta.clone());
        initial_tokens.insert(token_out.clone(), token_out_meta.clone());

        let token_store = Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(60),
        ));

        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        let component = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000022").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_in_meta, token_out_meta],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let mut states = HashMap::new();
        states.insert(
            "pool-1".to_string(),
            Box::new(SoftLimitSim {
                max_in: BigUint::from(100u8),
                calls: default_calls(),
            }) as Box<dyn ProtocolSim>,
        );
        let mut new_pairs = HashMap::new();
        new_pairs.insert("pool-1".to_string(), component);

        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(0),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(2),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = AmountOutRequest {
            request_id: "req-skip-deadline".to_string(),
            auction_id: None,
            token_in: token_in_hex.to_string(),
            token_out: token_out_hex.to_string(),
            amounts: vec!["1".to_string()],
        };

        let computation = get_amounts_out(app_state, request, None).await;
        assert!(computation.responses.is_empty());
        assert_eq!(
            computation.meta.result_quality,
            QuoteResultQuality::NoResults
        );
        assert!(matches!(
            computation.meta.status,
            QuoteStatus::PartialSuccess
        ));
        assert_eq!(computation.metrics.skipped_native_deadline, 1);
        assert!(computation
            .meta
            .pool_results
            .iter()
            .any(|outcome| outcome.outcome == PoolOutcomeKind::SkippedDeadline));
    }

    #[test]
    fn partial_result_probe_error_still_requires_limit_signals() {
        assert!(!is_limits_exhaustion_probe_error(
            "token_in invalid for probe",
            true
        ));
        assert!(is_limits_exhaustion_probe_error(
            "Sell amount exceeds limit 42",
            true
        ));
        assert!(is_limits_exhaustion_probe_error("Ticks exceeded", true));
    }
}
