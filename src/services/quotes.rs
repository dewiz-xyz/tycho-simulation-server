use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{FuturesUnordered, StreamExt};
use num_bigint::BigUint;
use num_traits::cast::ToPrimitive;
use tokio::task::spawn_blocking;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};
use tycho_simulation::{
    models::Token,
    protocol::{models::ProtocolComponent, state::ProtocolSim},
    tycho_common::Bytes,
};

use crate::models::messages::{
    AmountOutRequest, AmountOutResponse, QuoteFailure, QuoteFailureKind, QuoteMeta, QuoteStatus,
};
use crate::models::state::AppState;

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
    let pool_timeout = state.pool_timeout();
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

    let token_in_ref = match state.tokens.ensure(&token_in_bytes).await {
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
        Err(err) => {
            error!("Failed to refresh token cache: {err}");
            failures.push(make_failure(
                QuoteFailureKind::Internal,
                format!("Failed to resolve token {}", token_in_address),
                None,
            ));
            meta.status = QuoteStatus::InternalError;
            meta.failures = failures;
            return QuoteComputation { responses, meta };
        }
    };

    let token_out_ref = match state.tokens.ensure(&token_out_bytes).await {
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
        Err(err) => {
            error!("Failed to refresh token cache: {err}");
            failures.push(make_failure(
                QuoteFailureKind::Internal,
                format!("Failed to resolve token {}", token_out_address),
                None,
            ));
            meta.status = QuoteStatus::InternalError;
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

    let candidates_raw = state
        .state_store
        .matching_pools_by_addresses(&token_in_bytes, &token_out_bytes)
        .await;

    meta.matching_pools = candidates_raw.len();
    meta.candidate_pools = candidates_raw.len();

    if candidates_raw.is_empty() {
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
        "Quote candidates prepared: matching_pools={} amounts_per_pool={}",
        meta.matching_pools,
        amounts_in.len()
    );

    let token_in = token_in_ref.clone();
    let token_out = token_out_ref.clone();
    let expected_len = amounts_in.len();

    let cancel_token = cancel
        .as_ref()
        .map(CancellationToken::child_token)
        .unwrap_or_default();
    let mut tasks = FuturesUnordered::new();
    for (id, (pool_state, component)) in candidates_raw.into_iter() {
        let token_in_clone = token_in.clone();
        let token_out_clone = token_out.clone();
        let amounts_clone = Arc::clone(&amounts_in);
        let pool_address = component.id.to_string();
        let pool_protocol = component.protocol_system.clone();
        let pool_name = derive_pool_name(&component);
        tasks.push(simulate_pool(
            id,
            pool_state,
            pool_address,
            pool_name,
            pool_protocol,
            token_in_clone,
            token_out_clone,
            amounts_clone,
            expected_len,
            pool_timeout,
            cancel_token.clone(),
        ));
    }

    if !tasks.is_empty() {
        let quote_timeout_sleep = sleep(quote_timeout);
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
                                    if result.amounts_out.len() == expected_len && !result.amounts_out.is_empty() {
                                        responses.push(AmountOutResponse {
                                            pool: result.pool,
                                            pool_name: result.pool_name,
                                            pool_address: result.pool_address,
                                            amounts_out: result.amounts_out,
                                            gas_used: result.gas_used,
                                            block_number: current_block,
                                        });
                                    } else {
                                        let context = FailureContext {
                                            pool_id: &result.pool,
                                            pool_name: Some(&result.pool_name),
                                            pool_address: Some(&result.pool_address),
                                            protocol: Some(&result.protocol),
                                        };

                                        if result.amounts_out.is_empty() {
                                            let descriptor = format_pool_descriptor(&context);
                                            let base_error = result
                                                .errors
                                                .first()
                                                .cloned()
                                                .unwrap_or_else(|| "Pool returned no quotes".to_string());
                                            let message = format!("{}: {}", descriptor, base_error);
                                            let kind = classify_failure(&base_error, true);
                                            failures.push(make_failure(kind, message, Some(context)));
                                        } else {
                                            let descriptor = format_pool_descriptor(&context);
                                            let message = format!(
                                                "{} produced partial ladder ({} of {} steps)",
                                                descriptor,
                                                result.amounts_out.len(),
                                                expected_len
                                            );
                                            failures.push(make_failure(
                                                QuoteFailureKind::InconsistentResult,
                                                message,
                                                Some(context),
                                            ));
                                        }
                                    }
                                }
                                Err(failure) => {
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

    if responses.is_empty() {
        meta.status = QuoteStatus::PartialFailure;
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

        if !failures.is_empty() {
            meta.status = QuoteStatus::PartialFailure;
        }
    }

    meta.failures = failures;
    QuoteComputation { responses, meta }
}

struct PoolQuoteResult {
    pool: String,
    pool_name: String,
    pool_address: String,
    protocol: String,
    amounts_out: Vec<String>,
    gas_used: Vec<u64>,
    errors: Vec<String>,
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
    pool_state: Box<dyn ProtocolSim>,
    pool_address: String,
    pool_name: String,
    pool_protocol: String,
    token_in: Token,
    token_out: Token,
    amounts: Arc<Vec<BigUint>>,
    expected_len: usize,
    timeout: Duration,
    cancel_token: CancellationToken,
) -> Result<PoolQuoteResult, QuoteFailure> {
    let pool_id_for_failure = pool_id.clone();
    let pool_addr_for_failure = pool_address.clone();
    let pool_name_for_failure = pool_name.clone();
    let pool_protocol_for_failure = pool_protocol.clone();
    let token_in_clone = token_in.clone();
    let token_out_clone = token_out.clone();
    let amounts_clone = Arc::clone(&amounts);

    let handle = spawn_blocking(move || {
        let mut amounts_out = Vec::with_capacity(expected_len);
        let mut gas_used = Vec::with_capacity(expected_len);
        let mut errors = Vec::new();

        for amount_in in amounts_clone.iter() {
            match pool_state.get_amount_out(amount_in.clone(), &token_in_clone, &token_out_clone) {
                Ok(result) => {
                    amounts_out.push(result.amount.to_string());
                    gas_used.push(result.gas.to_u64().unwrap_or(0));
                }
                Err(e) => {
                    let msg = e.to_string();
                    debug!(
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
        }

        PoolQuoteResult {
            pool: pool_id,
            pool_name,
            pool_address,
            protocol: pool_protocol,
            amounts_out,
            gas_used,
            errors,
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
        _ = sleep(timeout) => {
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
    if lowered.contains("warm") {
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
    while matches!(bytes.last(), Some(0)) {
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
