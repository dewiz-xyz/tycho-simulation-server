use std::collections::HashMap;
use std::env;
use std::str::FromStr;
use std::sync::Arc;

use alloy_primitives::Keccak256;
use alloy_sol_types::SolValue;
use axum::http::StatusCode;
use num_bigint::BigUint;
use num_traits::{CheckedSub, One, ToPrimitive, Zero};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::warn;
use tycho_execution::encoding::{errors::EncodingError, evm::utils::bytes_to_address};
use tycho_execution::encoding::{
    evm::{encoder_builders::TychoRouterEncoderBuilder, utils::biguint_to_u256},
    models::{EncodedSolution, NativeAction, Solution, Swap, UserTransferType},
    tycho_encoder::TychoEncoder,
};
use tycho_simulation::{
    protocol::models::ProtocolComponent,
    tycho_common::{
        models::{protocol::ProtocolComponent as CommonProtocolComponent, token::Token, Chain},
        simulation::protocol_sim::ProtocolSim,
        Bytes,
    },
};

use crate::models::messages::{
    Approval, ExecutionCall, ExecutionCallKind, Hop, NormalizedRoute, PoolRef, PoolSwap,
    PoolSwapDraft, ResimulationDebug, RouteDebug, RouteEncodeRequest, RouteEncodeResponse,
    RouteTotals, Segment, SegmentDraft, TenderlyDebug,
};
use crate::models::state::AppState;

const SCHEMA_VERSION: &str = "2026-01-22";
const BPS_DENOMINATOR: u32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeErrorKind {
    InvalidRequest,
    NotFound,
    Simulation,
    Encoding,
    Internal,
}

#[derive(Debug)]
pub struct EncodeError {
    kind: EncodeErrorKind,
    message: String,
}

impl EncodeError {
    pub fn invalid<T: Into<String>>(message: T) -> Self {
        Self {
            kind: EncodeErrorKind::InvalidRequest,
            message: message.into(),
        }
    }

    pub fn not_found<T: Into<String>>(message: T) -> Self {
        Self {
            kind: EncodeErrorKind::NotFound,
            message: message.into(),
        }
    }

    pub fn simulation<T: Into<String>>(message: T) -> Self {
        Self {
            kind: EncodeErrorKind::Simulation,
            message: message.into(),
        }
    }

    pub fn encoding<T: Into<String>>(message: T) -> Self {
        Self {
            kind: EncodeErrorKind::Encoding,
            message: message.into(),
        }
    }

    pub fn internal<T: Into<String>>(message: T) -> Self {
        Self {
            kind: EncodeErrorKind::Internal,
            message: message.into(),
        }
    }

    pub fn status_code(&self) -> StatusCode {
        match self.kind {
            EncodeErrorKind::InvalidRequest => StatusCode::BAD_REQUEST,
            EncodeErrorKind::NotFound => StatusCode::NOT_FOUND,
            EncodeErrorKind::Simulation => StatusCode::UNPROCESSABLE_ENTITY,
            EncodeErrorKind::Encoding | EncodeErrorKind::Internal => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn kind(&self) -> EncodeErrorKind {
        self.kind
    }
}

struct NormalizedRouteInternal {
    segments: Vec<NormalizedSegmentInternal>,
}

struct NormalizedSegmentInternal {
    share_bps: u32,
    amount_in: BigUint,
    hops: Vec<NormalizedHopInternal>,
}

struct NormalizedHopInternal {
    token_in: Bytes,
    token_out: Bytes,
    swaps: Vec<NormalizedSwapDraftInternal>,
}

struct NormalizedSwapDraftInternal {
    pool: PoolRef,
    token_in: Bytes,
    token_out: Bytes,
    split_bps: Option<u32>,
    amount_in: Option<BigUint>,
}

struct ResimulatedRouteInternal {
    segments: Vec<ResimulatedSegmentInternal>,
}

struct ResimulatedSegmentInternal {
    share_bps: u32,
    amount_in: BigUint,
    expected_amount_out: BigUint,
    min_amount_out: BigUint,
    hops: Vec<ResimulatedHopInternal>,
}

struct ResimulatedHopInternal {
    token_in: Bytes,
    token_out: Bytes,
    amount_in: BigUint,
    expected_amount_out: BigUint,
    min_amount_out: BigUint,
    swaps: Vec<ResimulatedSwapInternal>,
}

struct ResimulatedSwapInternal {
    pool: PoolRef,
    token_in: Bytes,
    token_out: Bytes,
    split_bps: u32,
    amount_in: BigUint,
    expected_amount_out: BigUint,
    min_amount_out: BigUint,
    pool_state: Arc<dyn ProtocolSim>,
    component: Arc<ProtocolComponent>,
}

struct CallDataPayload {
    target: Bytes,
    value: BigUint,
    calldata: Vec<u8>,
}

pub async fn encode_route(
    state: AppState,
    request: RouteEncodeRequest,
) -> Result<RouteEncodeResponse, EncodeError> {
    let chain = validate_chain(request.chain_id)?;
    validate_slippage(request.slippage_bps, request.per_pool_slippage_bps.as_ref())?;

    let token_in = parse_address(&request.token_in)?;
    let token_out = parse_address(&request.token_out)?;
    let amount_in = parse_amount(&request.amount_in)?;
    let router_address = parse_address(&request.tycho_router_address)?;

    let normalized = normalize_route(&request, &token_in, &token_out, &amount_in)?;
    let resimulated =
        resimulate_route(&state, &request, &normalized, chain, &token_in, &token_out).await?;

    let encoder = build_encoder(chain, router_address.clone())?;
    let calls = build_execution_calls(
        &request,
        chain,
        &router_address,
        &resimulated,
        encoder.as_ref(),
    )?;

    let normalized_route = build_normalized_route_response(&resimulated);
    let totals = RouteTotals {
        expected_amount_out: resimulated
            .segments
            .iter()
            .fold(BigUint::zero(), |acc, seg| {
                acc + seg.expected_amount_out.clone()
            })
            .to_str_radix(10),
        min_amount_out: resimulated
            .segments
            .iter()
            .fold(BigUint::zero(), |acc, seg| acc + seg.min_amount_out.clone())
            .to_str_radix(10),
    };

    let debug = build_debug(&state, &request, &calls).await;

    Ok(RouteEncodeResponse {
        schema_version: SCHEMA_VERSION.to_string(),
        chain_id: request.chain_id,
        token_in: format_address(&token_in),
        token_out: format_address(&token_out),
        amount_in: amount_in.to_str_radix(10),
        normalized_route,
        calls,
        totals,
        debug,
    })
}

fn validate_chain(chain_id: u64) -> Result<Chain, EncodeError> {
    let chain = Chain::Ethereum;
    if chain.id() != chain_id {
        return Err(EncodeError::invalid(format!(
            "Unsupported chainId: {}",
            chain_id
        )));
    }
    Ok(chain)
}

fn validate_slippage(
    slippage_bps: u32,
    per_pool: Option<&HashMap<String, u32>>,
) -> Result<(), EncodeError> {
    if slippage_bps > BPS_DENOMINATOR {
        return Err(EncodeError::invalid("slippageBps must be <= 10000"));
    }
    if let Some(per_pool) = per_pool {
        for (pool, bps) in per_pool {
            if *bps > BPS_DENOMINATOR {
                return Err(EncodeError::invalid(format!(
                    "perPoolSlippageBps for {} must be <= 10000",
                    pool
                )));
            }
        }
    }
    Ok(())
}

fn normalize_route(
    request: &RouteEncodeRequest,
    request_token_in: &Bytes,
    request_token_out: &Bytes,
    total_amount_in: &BigUint,
) -> Result<NormalizedRouteInternal, EncodeError> {
    if request.segments.is_empty() {
        return Err(EncodeError::invalid("segments must not be empty"));
    }

    let mut remainder_index: Option<usize> = None;
    let mut share_sum: u32 = 0;
    let mut normalized_segments = Vec::with_capacity(request.segments.len());

    for (segment_index, segment) in request.segments.iter().enumerate() {
        validate_segment_shape(segment, segment_index)?;

        let is_remainder = segment.is_remainder.unwrap_or(false);
        if is_remainder {
            if remainder_index.is_some() {
                return Err(EncodeError::invalid(
                    "Only one segment can be marked as remainder",
                ));
            }
            remainder_index = Some(segment_index);
        } else {
            let share_bps = segment.share_bps.ok_or_else(|| {
                EncodeError::invalid("shareBps required for non-remainder segment")
            })?;
            if share_bps == 0 || share_bps > BPS_DENOMINATOR {
                return Err(EncodeError::invalid("shareBps must be 1..10000"));
            }
            share_sum = share_sum.saturating_add(share_bps);
            if share_sum > BPS_DENOMINATOR {
                return Err(EncodeError::invalid(
                    "Sum of segment shareBps must be <= 10000",
                ));
            }
        }

        validate_hop_continuity(segment, request_token_in, request_token_out)?;

        let hops = segment
            .hops
            .iter()
            .map(normalize_hop)
            .collect::<Result<Vec<_>, _>>()?;

        normalized_segments.push(NormalizedSegmentInternal {
            share_bps: segment.share_bps.unwrap_or(0),
            amount_in: BigUint::zero(),
            hops,
        });
    }

    if remainder_index.is_none() && share_sum < BPS_DENOMINATOR {
        warn!(
            request_id = request.request_id.as_deref().unwrap_or(""),
            share_sum, "Segments do not allocate full amountIn; remainder is unallocated"
        );
    }

    let mut allocated = BigUint::zero();
    for (index, segment) in normalized_segments.iter_mut().enumerate() {
        if Some(index) == remainder_index {
            continue;
        }
        if segment.share_bps == 0 {
            return Err(EncodeError::invalid(
                "shareBps must be set for non-remainder segments",
            ));
        }
        segment.amount_in = mul_bps(total_amount_in, segment.share_bps);
        allocated += segment.amount_in.clone();
    }

    if let Some(index) = remainder_index {
        let remainder_share = BPS_DENOMINATOR.saturating_sub(share_sum);
        let segment = normalized_segments
            .get_mut(index)
            .ok_or_else(|| EncodeError::internal("Remainder segment index out of bounds"))?;
        segment.share_bps = remainder_share;
        segment.amount_in = total_amount_in
            .checked_sub(&allocated)
            .ok_or_else(|| EncodeError::invalid("Segment allocation underflow"))?;
    }

    Ok(NormalizedRouteInternal {
        segments: normalized_segments,
    })
}

fn validate_segment_shape(segment: &SegmentDraft, segment_index: usize) -> Result<(), EncodeError> {
    if segment.hops.is_empty() {
        return Err(EncodeError::invalid(format!(
            "segment[{}].hops must not be empty",
            segment_index
        )));
    }
    Ok(())
}

fn validate_hop_continuity(
    segment: &SegmentDraft,
    request_token_in: &Bytes,
    request_token_out: &Bytes,
) -> Result<(), EncodeError> {
    let first = segment
        .hops
        .first()
        .ok_or_else(|| EncodeError::invalid("segment.hops must not be empty"))?;
    let last = segment
        .hops
        .last()
        .ok_or_else(|| EncodeError::invalid("segment.hops must not be empty"))?;

    let first_in = parse_address(&first.token_in)?;
    if &first_in != request_token_in {
        return Err(EncodeError::invalid(
            "segment first hop tokenIn does not match request tokenIn",
        ));
    }

    let last_out = parse_address(&last.token_out)?;
    if &last_out != request_token_out {
        return Err(EncodeError::invalid(
            "segment last hop tokenOut does not match request tokenOut",
        ));
    }

    for window in segment.hops.windows(2) {
        let prev_out = parse_address(&window[0].token_out)?;
        let next_in = parse_address(&window[1].token_in)?;
        if prev_out != next_in {
            return Err(EncodeError::invalid(
                "hop token continuity mismatch within segment",
            ));
        }
    }
    Ok(())
}

fn normalize_hop(
    hop: &crate::models::messages::HopDraft,
) -> Result<NormalizedHopInternal, EncodeError> {
    if hop.swaps.is_empty() {
        return Err(EncodeError::invalid("hop.swaps must not be empty"));
    }

    let token_in = parse_address(&hop.token_in)?;
    let token_out = parse_address(&hop.token_out)?;
    let swaps = hop
        .swaps
        .iter()
        .map(|swap| normalize_swap(swap, &token_in, &token_out))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(NormalizedHopInternal {
        token_in,
        token_out,
        swaps,
    })
}

fn normalize_swap(
    swap: &PoolSwapDraft,
    hop_token_in: &Bytes,
    hop_token_out: &Bytes,
) -> Result<NormalizedSwapDraftInternal, EncodeError> {
    let token_in = parse_address(&swap.token_in)?;
    let token_out = parse_address(&swap.token_out)?;
    if &token_in != hop_token_in {
        return Err(EncodeError::invalid(
            "swap tokenIn does not match hop tokenIn",
        ));
    }
    if &token_out != hop_token_out {
        return Err(EncodeError::invalid(
            "swap tokenOut does not match hop tokenOut",
        ));
    }

    if swap.split_bps.is_none() && swap.amount_in.is_none() {
        return Err(EncodeError::invalid(
            "swap must specify splitBps or amountIn",
        ));
    }
    if let Some(split) = swap.split_bps {
        if split > BPS_DENOMINATOR {
            return Err(EncodeError::invalid("swap splitBps must be <= 10000"));
        }
    }
    let amount_in = if let Some(amount) = &swap.amount_in {
        Some(parse_amount(amount)?)
    } else {
        None
    };

    Ok(NormalizedSwapDraftInternal {
        pool: swap.pool.clone(),
        token_in,
        token_out,
        split_bps: swap.split_bps,
        amount_in,
    })
}

async fn resimulate_route(
    state: &AppState,
    request: &RouteEncodeRequest,
    normalized: &NormalizedRouteInternal,
    chain: Chain,
    request_token_in: &Bytes,
    request_token_out: &Bytes,
) -> Result<ResimulatedRouteInternal, EncodeError> {
    let mut token_cache = TokenCache::new(state);
    let mut pool_cache: HashMap<String, (Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> =
        HashMap::new();

    let mut resim_segments = Vec::with_capacity(normalized.segments.len());
    for (segment_index, segment) in normalized.segments.iter().enumerate() {
        let mut hop_amount_in = segment.amount_in.clone();
        let mut resim_hops = Vec::with_capacity(segment.hops.len());

        for (hop_index, hop) in segment.hops.iter().enumerate() {
            if hop_amount_in.is_zero() {
                return Err(EncodeError::invalid(format!(
                    "segment[{}].hop[{}] amountIn is zero after allocation",
                    segment_index, hop_index
                )));
            }

            let allocated_swaps = allocate_swaps(hop_amount_in.clone(), &hop.swaps)?;
            let mut swap_results = Vec::with_capacity(allocated_swaps.len());
            let mut hop_expected = BigUint::zero();
            let mut hop_min = BigUint::zero();

            for (swap_index, allocated) in allocated_swaps.into_iter().enumerate() {
                let pool_entry = if let Some(entry) = pool_cache.get(&allocated.pool.component_id) {
                    (Arc::clone(&entry.0), Arc::clone(&entry.1))
                } else {
                    let entry = state
                        .pool_by_id(&allocated.pool.component_id)
                        .await
                        .ok_or_else(|| {
                            EncodeError::not_found(format!(
                                "Pool {} not found",
                                allocated.pool.component_id
                            ))
                        })?;
                    pool_cache.insert(allocated.pool.component_id.clone(), entry.clone());
                    entry
                };

                let sim_token_in = map_swap_token(&allocated.token_in, chain);
                let sim_token_out = map_swap_token(&allocated.token_out, chain);
                let token_in = token_cache.get(&sim_token_in).await?;
                let token_out = token_cache.get(&sim_token_out).await?;

                let result = pool_entry
                    .0
                    .get_amount_out(allocated.amount_in.clone(), &token_in, &token_out)
                    .map_err(|err| {
                        EncodeError::simulation(format!(
                            "Pool {} simulation failed: {}",
                            allocated.pool.component_id, err
                        ))
                    })?;
                let expected_out = result.amount;
                if expected_out.is_zero() {
                    return Err(EncodeError::simulation(format!(
                        "Pool {} returned zero amountOut",
                        allocated.pool.component_id
                    )));
                }

                let slippage_bps = request
                    .per_pool_slippage_bps
                    .as_ref()
                    .and_then(|per_pool| per_pool.get(&allocated.pool.component_id).cloned())
                    .unwrap_or(request.slippage_bps);
                let min_out = apply_slippage(&expected_out, slippage_bps);
                if min_out.is_zero() {
                    return Err(EncodeError::invalid(format!(
                        "Pool {} minAmountOut is zero",
                        allocated.pool.component_id
                    )));
                }

                hop_expected += expected_out.clone();
                hop_min += min_out.clone();

                swap_results.push(ResimulatedSwapInternal {
                    pool: allocated.pool,
                    token_in: allocated.token_in,
                    token_out: allocated.token_out,
                    split_bps: allocated.split_bps,
                    amount_in: allocated.amount_in,
                    expected_amount_out: expected_out,
                    min_amount_out: min_out,
                    pool_state: Arc::clone(&pool_entry.0),
                    component: Arc::clone(&pool_entry.1),
                });

                if swap_results.len() - 1 != swap_index {
                    return Err(EncodeError::internal("Swap allocation ordering mismatch"));
                }
            }

            let resim_hop = ResimulatedHopInternal {
                token_in: hop.token_in.clone(),
                token_out: hop.token_out.clone(),
                amount_in: hop_amount_in.clone(),
                expected_amount_out: hop_expected.clone(),
                min_amount_out: hop_min.clone(),
                swaps: swap_results,
            };
            hop_amount_in = hop_expected;
            resim_hops.push(resim_hop);
        }

        let last_hop = resim_hops
            .last()
            .ok_or_else(|| EncodeError::internal("Segment hops missing after resimulation"))?;

        resim_segments.push(ResimulatedSegmentInternal {
            share_bps: segment.share_bps,
            amount_in: segment.amount_in.clone(),
            expected_amount_out: last_hop.expected_amount_out.clone(),
            min_amount_out: last_hop.min_amount_out.clone(),
            hops: resim_hops,
        });
    }

    if request_token_in.is_empty() || request_token_out.is_empty() {
        return Err(EncodeError::internal(
            "Request tokens missing after resimulation",
        ));
    }

    Ok(ResimulatedRouteInternal {
        segments: resim_segments,
    })
}

struct AllocatedSwap {
    pool: PoolRef,
    token_in: Bytes,
    token_out: Bytes,
    split_bps: u32,
    amount_in: BigUint,
}

fn allocate_swaps(
    hop_amount_in: BigUint,
    swaps: &[NormalizedSwapDraftInternal],
) -> Result<Vec<AllocatedSwap>, EncodeError> {
    if swaps.is_empty() {
        return Err(EncodeError::invalid("hop.swaps must not be empty"));
    }
    if hop_amount_in.is_zero() {
        return Err(EncodeError::invalid("hop amountIn must be > 0"));
    }

    let has_split = swaps.iter().any(|swap| swap.split_bps.is_some());
    let mut computed_splits = Vec::with_capacity(swaps.len());

    if has_split {
        let mut sum_splits: u32 = 0;
        for swap in swaps {
            let split = if let Some(split) = swap.split_bps {
                split
            } else if let Some(amount_in) = &swap.amount_in {
                mul_bps_reverse(amount_in, &hop_amount_in)
            } else {
                0
            };
            if split > BPS_DENOMINATOR {
                return Err(EncodeError::invalid("swap splitBps must be <= 10000"));
            }
            sum_splits = sum_splits.saturating_add(split);
            computed_splits.push(split);
        }
        if sum_splits > BPS_DENOMINATOR {
            return Err(EncodeError::invalid(
                "sum of splitBps for hop exceeds 10000",
            ));
        }

        let mut allocations = Vec::with_capacity(swaps.len());
        let mut allocated_amount = BigUint::zero();
        for (index, swap) in swaps.iter().enumerate() {
            let split = computed_splits[index];
            let amount_in = if index == swaps.len() - 1 {
                // Keep rounding deterministic by pushing remainder into the last swap.
                hop_amount_in
                    .checked_sub(&allocated_amount)
                    .ok_or_else(|| EncodeError::invalid("swap allocation underflow"))?
            } else {
                let amount = mul_bps(&hop_amount_in, split);
                allocated_amount += amount.clone();
                amount
            };

            if let Some(expected) = &swap.amount_in {
                let diff = abs_diff(expected, &amount_in);
                if diff > BigUint::one() {
                    return Err(EncodeError::invalid(
                        "swap amountIn inconsistent with splitBps",
                    ));
                }
            }

            allocations.push(AllocatedSwap {
                pool: swap.pool.clone(),
                token_in: swap.token_in.clone(),
                token_out: swap.token_out.clone(),
                split_bps: split,
                amount_in,
            });
        }
        return Ok(allocations);
    }

    let mut sum_amounts = BigUint::zero();
    for swap in swaps {
        let amount = swap.amount_in.clone().ok_or_else(|| {
            EncodeError::invalid("swap amountIn required when splitBps not provided")
        })?;
        sum_amounts += amount;
    }
    if sum_amounts > hop_amount_in {
        return Err(EncodeError::invalid(
            "sum of swap amountIn exceeds hop amountIn",
        ));
    }

    let mut allocations = Vec::with_capacity(swaps.len());
    let mut allocated_amount = BigUint::zero();
    let mut split_sum: u32 = 0;
    for (index, swap) in swaps.iter().enumerate() {
        let mut amount_in = swap
            .amount_in
            .clone()
            .ok_or_else(|| EncodeError::invalid("swap amountIn required"))?;
        if index == swaps.len() - 1 {
            // If amounts don't cover the hop, assign the remainder to the last swap.
            let remainder = hop_amount_in
                .checked_sub(&sum_amounts)
                .ok_or_else(|| EncodeError::invalid("swap allocation underflow"))?;
            amount_in += remainder;
        }
        let split = if index == swaps.len() - 1 {
            BPS_DENOMINATOR.saturating_sub(split_sum)
        } else {
            let split = mul_bps_reverse(&amount_in, &hop_amount_in);
            split_sum = split_sum.saturating_add(split);
            split
        };

        allocated_amount += amount_in.clone();
        allocations.push(AllocatedSwap {
            pool: swap.pool.clone(),
            token_in: swap.token_in.clone(),
            token_out: swap.token_out.clone(),
            split_bps: split,
            amount_in,
        });
    }

    if allocated_amount != hop_amount_in {
        return Err(EncodeError::invalid(
            "swap allocation did not consume full hop amountIn",
        ));
    }

    Ok(allocations)
}

fn build_encoder(
    chain: Chain,
    router_address: Bytes,
) -> Result<Arc<dyn TychoEncoder>, EncodeError> {
    TychoRouterEncoderBuilder::new()
        .chain(chain)
        .user_transfer_type(UserTransferType::TransferFrom)
        .router_address(router_address)
        .build()
        .map(Arc::from)
        .map_err(|err| EncodeError::encoding(format!("Failed to build Tycho encoder: {}", err)))
}

fn build_execution_calls(
    request: &RouteEncodeRequest,
    chain: Chain,
    router_address: &Bytes,
    resimulated: &ResimulatedRouteInternal,
    encoder: &dyn TychoEncoder,
) -> Result<Vec<ExecutionCall>, EncodeError> {
    build_execution_calls_with(resimulated, |swap, _hop| {
        encode_single_swap_call(chain, router_address, encoder, request, swap)
    })
}

fn build_execution_calls_with<F>(
    resimulated: &ResimulatedRouteInternal,
    mut build_call: F,
) -> Result<Vec<ExecutionCall>, EncodeError>
where
    F: FnMut(
        &ResimulatedSwapInternal,
        &ResimulatedHopInternal,
    ) -> Result<CallDataPayload, EncodeError>,
{
    let mut calls = Vec::new();
    let mut index: u32 = 0;

    for (segment_index, segment) in resimulated.segments.iter().enumerate() {
        for (hop_index, hop) in segment.hops.iter().enumerate() {
            for swap in &hop.swaps {
                let calldata_payload = build_call(swap, hop)?;
                let approvals =
                    build_approvals(&swap.token_in, &calldata_payload.target, &swap.amount_in);

                calls.push(ExecutionCall {
                    index,
                    target: format_address(&calldata_payload.target),
                    value: calldata_payload.value.to_str_radix(10),
                    calldata: format_calldata(&calldata_payload.calldata),
                    approvals,
                    kind: ExecutionCallKind::TychoSingleSwap,
                    hop_path: format!("segment[{}].hop[{}]", segment_index, hop_index),
                    pool: swap.pool.clone(),
                    token_in: format_address(&swap.token_in),
                    token_out: format_address(&swap.token_out),
                    amount_in: swap.amount_in.to_str_radix(10),
                    min_amount_out: swap.min_amount_out.to_str_radix(10),
                    expected_amount_out: swap.expected_amount_out.to_str_radix(10),
                });
                index = index.saturating_add(1);
            }
        }
    }

    Ok(calls)
}

fn encode_single_swap_call(
    chain: Chain,
    router_address: &Bytes,
    encoder: &dyn TychoEncoder,
    request: &RouteEncodeRequest,
    swap: &ResimulatedSwapInternal,
) -> Result<CallDataPayload, EncodeError> {
    let native_address = chain.native_token().address;
    let wrapped_address = chain.wrapped_native_token().address;
    let is_wrap = swap.token_in == native_address;
    let is_unwrap = swap.token_out == native_address;
    if is_wrap && is_unwrap {
        return Err(EncodeError::invalid(
            "Native-to-native swaps are not supported",
        ));
    }

    // TychoRouter wraps/unwraps native ETH, but pool swap data expects WETH.
    let swap_token_in = if is_wrap {
        wrapped_address.clone()
    } else {
        swap.token_in.clone()
    };
    let swap_token_out = if is_unwrap {
        wrapped_address.clone()
    } else {
        swap.token_out.clone()
    };

    let common_component: CommonProtocolComponent = swap.component.as_ref().clone().into();
    let swap_data = Swap::new(
        common_component,
        swap_token_in.clone(),
        swap_token_out.clone(),
        0.0,
        None,
        Some(Arc::clone(&swap.pool_state)),
        None,
    );

    let native_action = if is_wrap {
        Some(NativeAction::Wrap)
    } else if is_unwrap {
        Some(NativeAction::Unwrap)
    } else {
        None
    };

    let sender = parse_address(&request.settlement_address)?;
    let receiver = sender.clone();

    let solution = Solution {
        sender,
        receiver: receiver.clone(),
        given_token: swap.token_in.clone(),
        given_amount: swap.amount_in.clone(),
        checked_token: swap.token_out.clone(),
        exact_out: false,
        checked_amount: swap.min_amount_out.clone(),
        swaps: vec![swap_data],
        native_action,
    };

    let encoded_solution = encoder
        .encode_solutions(vec![solution.clone()])
        .map_err(|err| EncodeError::encoding(format!("Swap encoding failed: {}", err)))?
        .into_iter()
        .next()
        .ok_or_else(|| EncodeError::encoding("Swap encoding returned no solutions"))?;

    ensure_single_swap_signature(&encoded_solution)?;

    if &encoded_solution.interacting_with != router_address {
        return Err(EncodeError::encoding(
            "Encoded router address does not match request tychoRouterAddress",
        ));
    }

    let is_transfer_from_allowed = !is_wrap;
    let calldata = encode_single_swap_calldata(
        &encoded_solution,
        swap,
        &receiver,
        is_wrap,
        is_unwrap,
        is_transfer_from_allowed,
    )?;

    Ok(CallDataPayload {
        target: encoded_solution.interacting_with,
        value: if is_wrap {
            swap.amount_in.clone()
        } else {
            BigUint::zero()
        },
        calldata,
    })
}

fn encode_single_swap_calldata(
    encoded_solution: &EncodedSolution,
    swap: &ResimulatedSwapInternal,
    receiver: &Bytes,
    is_wrap: bool,
    is_unwrap: bool,
    is_transfer_from_allowed: bool,
) -> Result<Vec<u8>, EncodeError> {
    let calldata_args = (
        biguint_to_u256(&swap.amount_in),
        bytes_to_address(&swap.token_in).map_err(map_encoding_error)?,
        bytes_to_address(&swap.token_out).map_err(map_encoding_error)?,
        biguint_to_u256(&swap.min_amount_out),
        is_wrap,
        is_unwrap,
        bytes_to_address(receiver).map_err(map_encoding_error)?,
        is_transfer_from_allowed,
        encoded_solution.swaps.clone(),
    )
        .abi_encode();

    encode_function_call(&encoded_solution.function_signature, calldata_args)
}

const EXPECTED_SINGLE_SWAP_SIGNATURE: &str =
    "singleSwap(uint256,address,address,uint256,bool,bool,address,bool,bytes)";

fn ensure_single_swap_signature(encoded_solution: &EncodedSolution) -> Result<(), EncodeError> {
    let signature = encoded_solution.function_signature.as_str();
    let normalized_signature: String = signature.chars().filter(|c| !c.is_whitespace()).collect();

    if encoded_solution.permit.is_some()
        || normalized_signature.contains("Permit2")
        || normalized_signature.contains("permit2")
    {
        return Err(EncodeError::encoding(
            "Permit2 encodings are not supported for singleSwap calls",
        ));
    }

    if normalized_signature != EXPECTED_SINGLE_SWAP_SIGNATURE {
        return Err(EncodeError::encoding(format!(
            "Unsupported Tycho function signature: {}",
            signature
        )));
    }

    Ok(())
}

fn encode_function_call(
    signature: &str,
    mut encoded_args: Vec<u8>,
) -> Result<Vec<u8>, EncodeError> {
    let selector = function_selector(signature)?;
    // Alloy encodes standalone dynamic bytes with a leading offset; strip if present.
    if encoded_args.len() > 32 {
        let mut prefix = vec![0u8; 31];
        prefix.push(32);
        if encoded_args[..32] == prefix {
            encoded_args = encoded_args[32..].to_vec();
        }
    }
    let mut call_data = Vec::with_capacity(4 + encoded_args.len());
    call_data.extend_from_slice(&selector);
    call_data.extend(encoded_args);
    Ok(call_data)
}

fn function_selector(signature: &str) -> Result<[u8; 4], EncodeError> {
    let normalized = signature.trim();
    if !normalized.contains('(') {
        return Err(EncodeError::encoding(format!(
            "Invalid function signature: {}",
            signature
        )));
    }
    let mut hasher = Keccak256::new();
    hasher.update(normalized.as_bytes());
    let hash = hasher.finalize();
    Ok([hash[0], hash[1], hash[2], hash[3]])
}

fn map_encoding_error(err: EncodingError) -> EncodeError {
    EncodeError::encoding(format!("Tycho encoding error: {err}"))
}

fn build_approvals(token_in: &Bytes, spender: &Bytes, amount_in: &BigUint) -> Vec<Approval> {
    if is_native(token_in) {
        return Vec::new();
    }
    vec![Approval {
        token: format_address(token_in),
        spender: format_address(spender),
        amount: amount_in.to_str_radix(10),
        reset_to_zero: None,
    }]
}

fn build_normalized_route_response(resimulated: &ResimulatedRouteInternal) -> NormalizedRoute {
    let segments = resimulated
        .segments
        .iter()
        .map(|segment| Segment {
            share_bps: segment.share_bps,
            amount_in: segment.amount_in.to_str_radix(10),
            expected_amount_out: segment.expected_amount_out.to_str_radix(10),
            min_amount_out: segment.min_amount_out.to_str_radix(10),
            hops: segment
                .hops
                .iter()
                .map(|hop| Hop {
                    token_in: format_address(&hop.token_in),
                    token_out: format_address(&hop.token_out),
                    amount_in: hop.amount_in.to_str_radix(10),
                    expected_amount_out: hop.expected_amount_out.to_str_radix(10),
                    min_amount_out: hop.min_amount_out.to_str_radix(10),
                    swaps: hop
                        .swaps
                        .iter()
                        .map(|swap| PoolSwap {
                            pool: swap.pool.clone(),
                            token_in: format_address(&swap.token_in),
                            token_out: format_address(&swap.token_out),
                            split_bps: swap.split_bps,
                            amount_in: swap.amount_in.to_str_radix(10),
                            expected_amount_out: swap.expected_amount_out.to_str_radix(10),
                            min_amount_out: swap.min_amount_out.to_str_radix(10),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    NormalizedRoute { segments }
}

async fn build_debug(
    state: &AppState,
    request: &RouteEncodeRequest,
    calls: &[ExecutionCall],
) -> Option<RouteDebug> {
    let mut debug = RouteDebug {
        request_id: request.request_id.clone(),
        tenderly: None,
        resimulation: None,
    };

    let block_number = state.current_block().await;
    debug.resimulation = Some(ResimulationDebug {
        block_number: Some(block_number),
        tycho_state_tag: None,
    });

    if request.enable_tenderly_sim.unwrap_or(false) {
        match TenderlyConfig::from_env() {
            Some(config) => match simulate_tenderly_bundle(request, calls, &config).await {
                Ok(tenderly) => {
                    debug.tenderly = Some(tenderly);
                }
                Err(err) => {
                    warn!(
                        request_id = request.request_id.as_deref().unwrap_or(""),
                        error = err.message(),
                        "Tenderly simulation failed"
                    );
                    debug.tenderly = Some(TenderlyDebug {
                        simulation_url: None,
                        simulation_id: None,
                    });
                }
            },
            None => {
                warn!(
                    request_id = request.request_id.as_deref().unwrap_or(""),
                    "Tenderly simulation requested but not configured"
                );
                debug.tenderly = Some(TenderlyDebug {
                    simulation_url: None,
                    simulation_id: None,
                });
            }
        }
    }

    if debug.request_id.is_none() && debug.tenderly.is_none() && debug.resimulation.is_none() {
        None
    } else {
        Some(debug)
    }
}

fn parse_amount(value: &str) -> Result<BigUint, EncodeError> {
    BigUint::from_str(value).map_err(|_| EncodeError::invalid(format!("Invalid amount: {}", value)))
}

fn parse_address(value: &str) -> Result<Bytes, EncodeError> {
    let trimmed = value.trim();
    let bytes = Bytes::from_str(trimmed)
        .map_err(|err| EncodeError::invalid(format!("Invalid address {}: {}", value, err)))?;
    if bytes.len() != 20 {
        return Err(EncodeError::invalid(format!(
            "Invalid address length for {}",
            value
        )));
    }
    Ok(bytes)
}

fn format_address(bytes: &Bytes) -> String {
    format_0x_hex(&format!("{bytes:x}"))
}

fn format_0x_hex(raw: &str) -> String {
    if let Some(stripped) = raw.strip_prefix("0x") {
        return format!("0x{stripped}");
    }
    if let Some(stripped) = raw.strip_prefix("0X") {
        return format!("0x{stripped}");
    }
    format!("0x{raw}")
}

fn format_calldata(data: &[u8]) -> String {
    let bytes = Bytes::from(data.to_vec());
    format_0x_hex(&format!("{bytes:x}"))
}

fn is_native(address: &Bytes) -> bool {
    *address == Chain::Ethereum.native_token().address
}

fn map_swap_token(address: &Bytes, chain: Chain) -> Bytes {
    if *address == chain.native_token().address {
        chain.wrapped_native_token().address
    } else {
        address.clone()
    }
}

fn mul_bps(amount: &BigUint, bps: u32) -> BigUint {
    (amount * BigUint::from(bps)) / BigUint::from(BPS_DENOMINATOR)
}

fn mul_bps_reverse(amount: &BigUint, total: &BigUint) -> u32 {
    if total.is_zero() {
        return 0;
    }
    let numerator = amount * BigUint::from(BPS_DENOMINATOR);
    let result = numerator / total;
    result.to_u32().unwrap_or(0)
}

fn apply_slippage(amount: &BigUint, slippage_bps: u32) -> BigUint {
    let safe_bps = BPS_DENOMINATOR.saturating_sub(slippage_bps);
    mul_bps(amount, safe_bps)
}

struct TenderlyConfig {
    account_slug: String,
    project_slug: String,
    access_key: String,
    simulation_type: String,
    gas: u64,
}

impl TenderlyConfig {
    fn from_env() -> Option<Self> {
        let account_slug = env::var("TENDERLY_ACCOUNT_SLUG").ok()?;
        let project_slug = env::var("TENDERLY_PROJECT_SLUG").ok()?;
        let access_key = env::var("TENDERLY_ACCESS_KEY").ok()?;
        let simulation_type =
            env::var("TENDERLY_SIMULATION_TYPE").unwrap_or_else(|_| "full".to_string());
        let gas = env::var("TENDERLY_SIMULATION_GAS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(8_000_000);
        Some(Self {
            account_slug,
            project_slug,
            access_key,
            simulation_type,
            gas,
        })
    }
}

async fn simulate_tenderly_bundle(
    request: &RouteEncodeRequest,
    calls: &[ExecutionCall],
    config: &TenderlyConfig,
) -> Result<TenderlyDebug, EncodeError> {
    if calls.is_empty() {
        return Ok(TenderlyDebug {
            simulation_url: None,
            simulation_id: None,
        });
    }

    let simulations: Vec<Value> = calls
        .iter()
        .map(|call| {
            let value_json = tenderly_value_json(&call.value)?;
            Ok(json!({
                "network_id": request.chain_id.to_string(),
                "save": true,
                "save_if_fails": true,
                "simulation_type": config.simulation_type,
                "from": request.settlement_address,
                "to": call.target,
                "input": call.calldata,
                "gas": config.gas,
                "gas_price": 0,
                "value": value_json,
            }))
        })
        .collect::<Result<Vec<_>, EncodeError>>()?;

    let payload = json!({ "simulations": simulations });
    let url = format!(
        "https://api.tenderly.co/api/v1/account/{}/project/{}/simulate-bundle",
        config.account_slug, config.project_slug
    );

    let client = Client::new();
    let response = client
        .post(url)
        .header("X-Access-Key", &config.access_key)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|err| EncodeError::encoding(format!("Tenderly request failed: {err}")))?;

    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|err| EncodeError::encoding(format!("Tenderly response decode failed: {err}")))?;

    if !status.is_success() {
        return Err(EncodeError::encoding(format!(
            "Tenderly simulation returned status {}",
            status
        )));
    }

    let simulation_id = extract_simulation_id(&body);
    let simulation_url = simulation_id.as_ref().map(|id| {
        format!(
            "https://dashboard.tenderly.co/{}/{}/simulator/{}",
            config.account_slug, config.project_slug, id
        )
    });

    Ok(TenderlyDebug {
        simulation_url,
        simulation_id,
    })
}

fn tenderly_value_json(value: &str) -> Result<Value, EncodeError> {
    let amount = parse_amount(value)?;
    if let Some(numeric) = amount.to_u64() {
        Ok(json!(numeric))
    } else {
        Ok(json!(amount.to_str_radix(10)))
    }
}

fn extract_simulation_id(body: &Value) -> Option<String> {
    let direct = body.pointer("/simulation/id").and_then(string_or_number);
    if direct.is_some() {
        return direct;
    }

    let from_bundle = body
        .get("simulation_results")
        .and_then(|value| value.as_array())
        .and_then(|items| items.last())
        .and_then(|item| item.pointer("/simulation/id"))
        .and_then(string_or_number);
    if from_bundle.is_some() {
        return from_bundle;
    }

    body.get("simulations")
        .and_then(|value| value.as_array())
        .and_then(|items| items.last())
        .and_then(|item| item.pointer("/simulation/id"))
        .and_then(string_or_number)
}

fn string_or_number(value: &Value) -> Option<String> {
    if let Some(id) = value.as_str() {
        Some(id.to_string())
    } else {
        value.as_u64().map(|id| id.to_string())
    }
}

fn abs_diff(a: &BigUint, b: &BigUint) -> BigUint {
    if a >= b {
        a - b
    } else {
        b - a
    }
}

struct TokenCache<'a> {
    state: &'a AppState,
    cache: HashMap<Bytes, Token>,
}

impl<'a> TokenCache<'a> {
    fn new(state: &'a AppState) -> Self {
        Self {
            state,
            cache: HashMap::new(),
        }
    }

    async fn get(&mut self, address: &Bytes) -> Result<Token, EncodeError> {
        if let Some(token) = self.cache.get(address) {
            return Ok(token.clone());
        }
        let token = self
            .state
            .tokens
            .ensure(address)
            .await
            .map_err(|err| EncodeError::simulation(format!("Token lookup failed: {}", err)))?
            .ok_or_else(|| EncodeError::invalid("Token not found"))?;
        self.cache.insert(address.clone(), token.clone());
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::messages::{HopDraft, SegmentDraft};
    use chrono::NaiveDateTime;
    use tycho_execution::encoding::models::Transaction;
    use tycho_execution::encoding::tycho_encoder::TychoEncoder;
    use tycho_simulation::tycho_common::models::Chain;

    fn pool_ref(id: &str) -> PoolRef {
        PoolRef {
            protocol: "uniswap_v2".to_string(),
            component_id: id.to_string(),
            pool_address: None,
        }
    }

    #[test]
    fn allocate_swaps_split_remainder() {
        let hop_amount = BigUint::from(100u32);
        let swaps = vec![
            NormalizedSwapDraftInternal {
                pool: pool_ref("p1"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: Some(5000),
                amount_in: None,
            },
            NormalizedSwapDraftInternal {
                pool: pool_ref("p2"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: Some(0),
                amount_in: None,
            },
        ];

        let allocated = allocate_swaps(hop_amount, &swaps).expect("allocates");
        assert_eq!(allocated.len(), 2);
        assert_eq!(allocated[0].amount_in, BigUint::from(50u32));
        assert_eq!(allocated[1].amount_in, BigUint::from(50u32));
    }

    #[test]
    fn allocate_swaps_amount_in_remainder() {
        let hop_amount = BigUint::from(100u32);
        let swaps = vec![
            NormalizedSwapDraftInternal {
                pool: pool_ref("p1"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: None,
                amount_in: Some(BigUint::from(30u32)),
            },
            NormalizedSwapDraftInternal {
                pool: pool_ref("p2"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: None,
                amount_in: Some(BigUint::from(30u32)),
            },
        ];

        let allocated = allocate_swaps(hop_amount, &swaps).expect("allocates");
        assert_eq!(allocated[0].amount_in, BigUint::from(30u32));
        assert_eq!(allocated[1].amount_in, BigUint::from(70u32));
        assert_eq!(allocated[0].split_bps, 3000);
        assert_eq!(allocated[1].split_bps, 7000);
    }

    #[test]
    fn normalize_segment_remainder_allocates() {
        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "100".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: "0x0000000000000000000000000000000000000004".to_string(),
            slippage_bps: 25,
            per_pool_slippage_bps: None,
            segments: vec![
                SegmentDraft {
                    share_bps: Some(2000),
                    is_remainder: None,
                    hops: vec![HopDraft {
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p1"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: Some(10000),
                            amount_in: None,
                            amount_out: None,
                        }],
                    }],
                },
                SegmentDraft {
                    share_bps: None,
                    is_remainder: Some(true),
                    hops: vec![HopDraft {
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p2"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: Some(10000),
                            amount_in: None,
                            amount_out: None,
                        }],
                    }],
                },
            ],
            enable_tenderly_sim: None,
            request_id: None,
        };

        let token_in = parse_address(&request.token_in).unwrap();
        let token_out = parse_address(&request.token_out).unwrap();
        let amount_in = parse_amount(&request.amount_in).unwrap();

        let normalized = normalize_route(&request, &token_in, &token_out, &amount_in).unwrap();
        assert_eq!(normalized.segments.len(), 2);
        assert_eq!(normalized.segments[0].amount_in, BigUint::from(20u32));
        assert_eq!(normalized.segments[1].amount_in, BigUint::from(80u32));
    }

    #[test]
    fn build_calls_ordering_for_mega_route() {
        let resimulated = ResimulatedRouteInternal {
            segments: vec![
                ResimulatedSegmentInternal {
                    share_bps: 2000,
                    amount_in: BigUint::from(20u32),
                    expected_amount_out: BigUint::from(2u32),
                    min_amount_out: BigUint::from(1u32),
                    hops: vec![ResimulatedHopInternal {
                        token_in: Bytes::from_str("0x0000000000000000000000000000000000000001")
                            .unwrap(),
                        token_out: Bytes::from_str("0x0000000000000000000000000000000000000002")
                            .unwrap(),
                        amount_in: BigUint::from(20u32),
                        expected_amount_out: BigUint::from(2u32),
                        min_amount_out: BigUint::from(1u32),
                        swaps: vec![
                            ResimulatedSwapInternal {
                                pool: pool_ref("p1"),
                                token_in: Bytes::from_str(
                                    "0x0000000000000000000000000000000000000001",
                                )
                                .unwrap(),
                                token_out: Bytes::from_str(
                                    "0x0000000000000000000000000000000000000002",
                                )
                                .unwrap(),
                                split_bps: 5000,
                                amount_in: BigUint::from(10u32),
                                expected_amount_out: BigUint::from(1u32),
                                min_amount_out: BigUint::from(1u32),
                                pool_state: Arc::new(MockProtocolSim {}),
                                component: Arc::new(dummy_component()),
                            },
                            ResimulatedSwapInternal {
                                pool: pool_ref("p2"),
                                token_in: Bytes::from_str(
                                    "0x0000000000000000000000000000000000000001",
                                )
                                .unwrap(),
                                token_out: Bytes::from_str(
                                    "0x0000000000000000000000000000000000000002",
                                )
                                .unwrap(),
                                split_bps: 5000,
                                amount_in: BigUint::from(10u32),
                                expected_amount_out: BigUint::from(1u32),
                                min_amount_out: BigUint::from(1u32),
                                pool_state: Arc::new(MockProtocolSim {}),
                                component: Arc::new(dummy_component()),
                            },
                        ],
                    }],
                },
                ResimulatedSegmentInternal {
                    share_bps: 6000,
                    amount_in: BigUint::from(60u32),
                    expected_amount_out: BigUint::from(6u32),
                    min_amount_out: BigUint::from(5u32),
                    hops: vec![
                        ResimulatedHopInternal {
                            token_in: Bytes::from_str("0x0000000000000000000000000000000000000001")
                                .unwrap(),
                            token_out: Bytes::from_str(
                                "0x0000000000000000000000000000000000000003",
                            )
                            .unwrap(),
                            amount_in: BigUint::from(60u32),
                            expected_amount_out: BigUint::from(3u32),
                            min_amount_out: BigUint::from(2u32),
                            swaps: vec![dummy_swap("p3"), dummy_swap("p4")],
                        },
                        ResimulatedHopInternal {
                            token_in: Bytes::from_str("0x0000000000000000000000000000000000000003")
                                .unwrap(),
                            token_out: Bytes::from_str(
                                "0x0000000000000000000000000000000000000002",
                            )
                            .unwrap(),
                            amount_in: BigUint::from(3u32),
                            expected_amount_out: BigUint::from(3u32),
                            min_amount_out: BigUint::from(3u32),
                            swaps: vec![dummy_swap("p5"), dummy_swap("p6")],
                        },
                    ],
                },
                ResimulatedSegmentInternal {
                    share_bps: 2000,
                    amount_in: BigUint::from(20u32),
                    expected_amount_out: BigUint::from(2u32),
                    min_amount_out: BigUint::from(1u32),
                    hops: vec![
                        ResimulatedHopInternal {
                            token_in: Bytes::from_str("0x0000000000000000000000000000000000000001")
                                .unwrap(),
                            token_out: Bytes::from_str(
                                "0x0000000000000000000000000000000000000004",
                            )
                            .unwrap(),
                            amount_in: BigUint::from(20u32),
                            expected_amount_out: BigUint::from(2u32),
                            min_amount_out: BigUint::from(1u32),
                            swaps: vec![dummy_swap("p7"), dummy_swap("p8")],
                        },
                        ResimulatedHopInternal {
                            token_in: Bytes::from_str("0x0000000000000000000000000000000000000004")
                                .unwrap(),
                            token_out: Bytes::from_str(
                                "0x0000000000000000000000000000000000000002",
                            )
                            .unwrap(),
                            amount_in: BigUint::from(2u32),
                            expected_amount_out: BigUint::from(2u32),
                            min_amount_out: BigUint::from(1u32),
                            swaps: vec![dummy_swap("p9"), dummy_swap("p10")],
                        },
                    ],
                },
            ],
        };

        let calls = build_execution_calls_with(&resimulated, |_swap, _hop| {
            Ok(CallDataPayload {
                target: Bytes::from_str("0x0000000000000000000000000000000000000005").unwrap(),
                value: BigUint::zero(),
                calldata: vec![0x01],
            })
        })
        .unwrap();

        assert_eq!(calls.len(), 10);
        assert_eq!(calls[0].hop_path, "segment[0].hop[0]");
        assert_eq!(calls[1].hop_path, "segment[0].hop[0]");
        assert_eq!(calls[2].hop_path, "segment[1].hop[0]");
        assert_eq!(calls[3].hop_path, "segment[1].hop[0]");
        assert_eq!(calls[4].hop_path, "segment[1].hop[1]");
        assert_eq!(calls[5].hop_path, "segment[1].hop[1]");
        assert_eq!(calls[6].hop_path, "segment[2].hop[0]");
        assert_eq!(calls[7].hop_path, "segment[2].hop[0]");
        assert_eq!(calls[8].hop_path, "segment[2].hop[1]");
        assert_eq!(calls[9].hop_path, "segment[2].hop[1]");
    }

    #[test]
    fn encode_single_swap_call_sets_value_for_native() {
        let router = Bytes::from_str("0x0000000000000000000000000000000000000004").unwrap();
        let encoder = MockTychoEncoder::new(
            "singleSwap(uint256,address,address,uint256,bool,bool,address,bool,bytes)",
            router.clone(),
        );

        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000000".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "10".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: format_address(&router),
            slippage_bps: 25,
            per_pool_slippage_bps: None,
            segments: Vec::new(),
            enable_tenderly_sim: None,
            request_id: None,
        };

        let swap = ResimulatedSwapInternal {
            pool: pool_ref("p1"),
            token_in: Chain::Ethereum.native_token().address,
            token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
            split_bps: 10_000,
            amount_in: BigUint::from(10u32),
            expected_amount_out: BigUint::from(9u32),
            min_amount_out: BigUint::from(8u32),
            pool_state: Arc::new(MockProtocolSim {}),
            component: Arc::new(dummy_component()),
        };

        let payload =
            encode_single_swap_call(Chain::Ethereum, &router, &encoder, &request, &swap).unwrap();

        assert_eq!(payload.value, BigUint::from(10u32));
        let selector = function_selector(
            "singleSwap(uint256,address,address,uint256,bool,bool,address,bool,bytes)",
        )
        .unwrap();
        assert_eq!(&payload.calldata[..4], &selector);
    }

    #[test]
    fn encode_single_swap_call_rejects_permit2() {
        let router = Bytes::from_str("0x0000000000000000000000000000000000000004").unwrap();
        let encoder = MockTychoEncoder::new(
            "singleSwapPermit2(uint256,address,address,uint256,bool,bool,address,(uint256),bytes)",
            router.clone(),
        );

        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "10".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: format_address(&router),
            slippage_bps: 25,
            per_pool_slippage_bps: None,
            segments: Vec::new(),
            enable_tenderly_sim: None,
            request_id: None,
        };

        let swap = ResimulatedSwapInternal {
            pool: pool_ref("p1"),
            token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
            split_bps: 10_000,
            amount_in: BigUint::from(10u32),
            expected_amount_out: BigUint::from(9u32),
            min_amount_out: BigUint::from(8u32),
            pool_state: Arc::new(MockProtocolSim {}),
            component: Arc::new(dummy_component()),
        };

        match encode_single_swap_call(Chain::Ethereum, &router, &encoder, &request, &swap) {
            Err(err) => assert_eq!(err.kind(), EncodeErrorKind::Encoding),
            Ok(_) => panic!("permit2 should be rejected"),
        }
    }

    #[test]
    fn encode_single_swap_call_rejects_unknown_signature() {
        let router = Bytes::from_str("0x0000000000000000000000000000000000000004").unwrap();
        let encoder = MockTychoEncoder::new(
            "singleSwap(uint256,address,address,uint256,bool,bool,address,bool,bytes32)",
            router.clone(),
        );

        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "10".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: format_address(&router),
            slippage_bps: 25,
            per_pool_slippage_bps: None,
            segments: Vec::new(),
            enable_tenderly_sim: None,
            request_id: None,
        };

        let swap = ResimulatedSwapInternal {
            pool: pool_ref("p1"),
            token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
            split_bps: 10_000,
            amount_in: BigUint::from(10u32),
            expected_amount_out: BigUint::from(9u32),
            min_amount_out: BigUint::from(8u32),
            pool_state: Arc::new(MockProtocolSim {}),
            component: Arc::new(dummy_component()),
        };

        match encode_single_swap_call(Chain::Ethereum, &router, &encoder, &request, &swap) {
            Err(err) => assert_eq!(err.kind(), EncodeErrorKind::Encoding),
            Ok(_) => panic!("unknown signature should be rejected"),
        }
    }

    fn dummy_swap(id: &str) -> ResimulatedSwapInternal {
        ResimulatedSwapInternal {
            pool: pool_ref(id),
            token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
            split_bps: 5000,
            amount_in: BigUint::from(1u32),
            expected_amount_out: BigUint::from(1u32),
            min_amount_out: BigUint::from(1u32),
            pool_state: Arc::new(MockProtocolSim {}),
            component: Arc::new(dummy_component()),
        }
    }

    fn dummy_component() -> ProtocolComponent {
        let token_a = dummy_token("0x0000000000000000000000000000000000000001");
        let token_b = dummy_token("0x0000000000000000000000000000000000000002");
        ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000009").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_a, token_b],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        )
    }

    fn dummy_token(address: &str) -> Token {
        let bytes = Bytes::from_str(address).unwrap();
        Token::new(&bytes, "TKN", 18, 0, &[], Chain::Ethereum, 100)
    }

    #[derive(Debug)]
    struct MockProtocolSim;

    impl ProtocolSim for MockProtocolSim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(
            &self,
            _base: &Token,
            _quote: &Token,
        ) -> Result<f64, tycho_simulation::tycho_common::simulation::errors::SimulationError>
        {
            Ok(0.0)
        }

        fn get_amount_out(
            &self,
            amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<
            tycho_simulation::tycho_common::simulation::protocol_sim::GetAmountOutResult,
            tycho_simulation::tycho_common::simulation::errors::SimulationError,
        > {
            Ok(
                tycho_simulation::tycho_common::simulation::protocol_sim::GetAmountOutResult::new(
                    amount_in,
                    BigUint::zero(),
                    self.clone_box(),
                ),
            )
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<
            (BigUint, BigUint),
            tycho_simulation::tycho_common::simulation::errors::SimulationError,
        > {
            Ok((BigUint::zero(), BigUint::zero()))
        }

        fn delta_transition(
            &mut self,
            _delta: tycho_simulation::tycho_common::dto::ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &tycho_simulation::tycho_common::simulation::protocol_sim::Balances,
        ) -> Result<(), tycho_simulation::tycho_common::simulation::errors::TransitionError<String>>
        {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(Self {})
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn eq(&self, _other: &dyn ProtocolSim) -> bool {
            true
        }
    }

    struct MockTychoEncoder {
        signature: String,
        router: Bytes,
        swaps: Vec<u8>,
    }

    impl MockTychoEncoder {
        fn new(signature: &str, router: Bytes) -> Self {
            Self {
                signature: signature.to_string(),
                router,
                swaps: vec![0x01, 0x02],
            }
        }
    }

    impl TychoEncoder for MockTychoEncoder {
        fn encode_solutions(
            &self,
            _solutions: Vec<Solution>,
        ) -> Result<Vec<EncodedSolution>, EncodingError> {
            Ok(vec![EncodedSolution {
                swaps: self.swaps.clone(),
                interacting_with: self.router.clone(),
                function_signature: self.signature.clone(),
                n_tokens: 0,
                permit: None,
            }])
        }

        fn encode_full_calldata(
            &self,
            _solutions: Vec<Solution>,
        ) -> Result<Vec<Transaction>, EncodingError> {
            Err(EncodingError::FatalError(
                "encode_full_calldata not supported in tests".to_string(),
            ))
        }

        fn validate_solution(&self, _solution: &Solution) -> Result<(), EncodingError> {
            Ok(())
        }
    }
}
