use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{FuturesUnordered, StreamExt};
use num_bigint::BigUint;
use num_traits::cast::ToPrimitive;
use tokio::task::spawn_blocking;
use tokio::time::sleep;
use tracing::{debug, error, info};
use tycho_simulation::{models::Token, protocol::state::ProtocolSim, tycho_core::Bytes};

use crate::models::messages::{
    AmountOutRequest, AmountOutResponse, QuoteFailure, QuoteFailureKind, QuoteMeta, QuoteStatus,
};
use crate::models::state::AppState;

pub struct QuoteComputation {
    pub responses: Vec<AmountOutResponse>,
    pub meta: QuoteMeta,
}

pub async fn get_amounts_out(state: AppState, request: AmountOutRequest) -> QuoteComputation {
    let mut responses = Vec::new();
    let mut failures = Vec::new();

    let readiness_wait = Duration::from_secs(2);
    let simulation_timeout = Duration::from_secs(5);

    let mut current_block = state.current_block().await;
    let mut total_pools = state.total_pools().await;

    let mut meta = QuoteMeta {
        status: QuoteStatus::Ready,
        block_number: current_block,
        matching_pools: 0,
        candidate_pools: 0,
        total_pools: Some(total_pools),
        failures: Vec::new(),
    };

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

    let mut tasks = FuturesUnordered::new();
    for (id, (pool_state, component)) in candidates_raw.into_iter() {
        let token_in_clone = token_in.clone();
        let token_out_clone = token_out.clone();
        let amounts_clone = Arc::clone(&amounts_in);
        let pool_address = component.id.to_string();
        tasks.push(simulate_pool(
            id,
            pool_state,
            pool_address,
            token_in_clone,
            token_out_clone,
            amounts_clone,
            expected_len,
            simulation_timeout,
        ));
    }

    while let Some(outcome) = tasks.next().await {
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
                    let message = if result.amounts_out.is_empty() {
                        result
                            .errors
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "Pool returned no quotes".to_string())
                    } else {
                        format!(
                            "Pool produced partial ladder ({} of {} steps)",
                            result.amounts_out.len(),
                            expected_len
                        )
                    };
                    failures.push(make_failure(
                        if result.amounts_out.is_empty() {
                            classify_failure(&message)
                        } else {
                            QuoteFailureKind::InconsistentResult
                        },
                        message,
                        Some(result.pool),
                    ));
                }
            }
            Err(failure) => {
                failures.push(failure);
            }
        }
    }

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
                .get(0)
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
    amounts_out: Vec<String>,
    gas_used: Vec<u64>,
    errors: Vec<String>,
}

async fn simulate_pool(
    pool_id: String,
    pool_state: Box<dyn ProtocolSim>,
    pool_address: String,
    token_in: Token,
    token_out: Token,
    amounts: Arc<Vec<BigUint>>,
    expected_len: usize,
    timeout: Duration,
) -> Result<PoolQuoteResult, QuoteFailure> {
    let pool_id_for_failure = pool_id.clone();
    let pool_addr_for_failure = pool_address.clone();
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
                    debug!("Pool {} quote error: {}", pool_id, msg);
                    errors.push(msg);
                }
            }
        }

        let pool_name = format!("{:?}", pool_state)
            .split_whitespace()
            .next()
            .unwrap_or("Unknown")
            .to_string();

        PoolQuoteResult {
            pool: pool_id,
            pool_name,
            pool_address,
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
                    Err(make_failure(
                        QuoteFailureKind::Internal,
                        format!("Quote computation panicked for pool {}: {}", pool_id_for_failure, join_err),
                        Some(pool_id_for_failure),
                    ))
                }
            }
        }
        _ = sleep(timeout) => {
            handle.as_mut().abort();
            Err(make_failure(
                QuoteFailureKind::Timeout,
                format!("Quote computation timed out for pool {} ({})", pool_id_for_failure, pool_addr_for_failure),
                Some(pool_id_for_failure),
            ))
        }
    }
}

fn classify_failure(message: &str) -> QuoteFailureKind {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("warm") {
        QuoteFailureKind::WarmUp
    } else if lowered.contains("overflow") {
        QuoteFailureKind::Overflow
    } else if lowered.contains("timeout") {
        QuoteFailureKind::Timeout
    } else if lowered.contains("token") {
        QuoteFailureKind::TokenCoverage
    } else {
        QuoteFailureKind::Simulator
    }
}

fn make_failure(kind: QuoteFailureKind, message: String, pool: Option<String>) -> QuoteFailure {
    QuoteFailure {
        kind,
        message,
        pool,
    }
}
