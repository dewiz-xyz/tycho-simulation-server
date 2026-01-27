use std::collections::{HashMap, HashSet};
use std::env;
use std::str::FromStr;
use std::sync::Arc;

use alloy_primitives::Keccak256;
use alloy_sol_types::SolValue;
use axum::http::StatusCode;
use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{info, warn};
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
    Hop, Interaction, InteractionKind, NormalizedRoute, PoolRef, PoolSwap, PoolSwapDraft,
    ResimulationDebug, RouteDebug, RouteEncodeRequest, RouteEncodeResponse, Segment, SegmentDraft,
    SwapKind, TenderlyDebug,
};
use crate::models::state::AppState;

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
    kind: SwapKind,
    share_bps: u32,
    amount_in: BigUint,
    hops: Vec<NormalizedHopInternal>,
}

struct NormalizedHopInternal {
    share_bps: u32,
    token_in: Bytes,
    token_out: Bytes,
    swaps: Vec<NormalizedSwapDraftInternal>,
}

struct NormalizedSwapDraftInternal {
    pool: PoolRef,
    token_in: Bytes,
    token_out: Bytes,
    split_bps: u32,
}

struct ResimulatedRouteInternal {
    segments: Vec<ResimulatedSegmentInternal>,
}

struct ResimulatedSegmentInternal {
    kind: SwapKind,
    share_bps: u32,
    amount_in: BigUint,
    expected_amount_out: BigUint,
    hops: Vec<ResimulatedHopInternal>,
}

struct ResimulatedHopInternal {
    share_bps: u32,
    token_in: Bytes,
    token_out: Bytes,
    amount_in: BigUint,
    expected_amount_out: BigUint,
    swaps: Vec<ResimulatedSwapInternal>,
}

struct ResimulatedSwapInternal {
    pool: PoolRef,
    token_in: Bytes,
    token_out: Bytes,
    split_bps: u32,
    amount_in: BigUint,
    expected_amount_out: BigUint,
    pool_state: Arc<dyn ProtocolSim>,
    component: Arc<ProtocolComponent>,
}

struct CallDataPayload {
    target: Bytes,
    value: BigUint,
    calldata: Vec<u8>,
}

struct SplitStats {
    total_amount: BigUint,
    count: usize,
    last_index: usize,
}

struct RouteContext<'a> {
    request: &'a RouteEncodeRequest,
    chain: Chain,
    token_in: &'a Bytes,
    token_out: &'a Bytes,
    amount_in: &'a BigUint,
    router_address: &'a Bytes,
}

pub async fn encode_route(
    state: AppState,
    request: RouteEncodeRequest,
) -> Result<RouteEncodeResponse, EncodeError> {
    let chain = validate_chain(request.chain_id)?;
    validate_swap_kinds(&request)?;

    let token_in = parse_address(&request.token_in)?;
    let token_out = parse_address(&request.token_out)?;
    let amount_in = parse_amount(&request.amount_in)?;
    let min_amount_out = parse_amount(&request.min_amount_out)?;
    let router_address = parse_address(&request.tycho_router_address)?;
    ensure_erc20_tokens(chain, &token_in, &token_out)?;

    let native_address = chain.native_token().address;
    let normalized = normalize_route(&request, &token_in, &token_out, &amount_in, &native_address)?;
    let resimulated = resimulate_route(&state, &normalized, chain, &token_in, &token_out).await?;
    log_resimulation_amounts(request.request_id.as_deref(), &resimulated);
    let encoder = build_encoder(chain, router_address.clone())?;
    let expected_total = compute_expected_total(&resimulated);
    if expected_total < min_amount_out {
        return Err(EncodeError::simulation(
            "Route expectedAmountOut below minAmountOut",
        ));
    }
    let route_context = RouteContext {
        request: &request,
        chain,
        token_in: &token_in,
        token_out: &token_out,
        amount_in: &amount_in,
        router_address: &router_address,
    };
    let router_call = build_route_calldata_tx(
        &route_context,
        &resimulated,
        encoder.as_ref(),
        &min_amount_out,
    )?;
    let reset_approval =
        should_reset_allowance(&state.reset_allowance_tokens, request.chain_id, &token_in);
    let interactions =
        build_settlement_interactions(&token_in, &amount_in, router_call, reset_approval)?;

    let normalized_route = build_normalized_route_response(&resimulated);

    let debug = build_debug(&state, &request, &interactions).await;

    Ok(RouteEncodeResponse {
        chain_id: request.chain_id,
        token_in: format_address(&token_in),
        token_out: format_address(&token_out),
        amount_in: amount_in.to_str_radix(10),
        min_amount_out: min_amount_out.to_str_radix(10),
        swap_kind: request.swap_kind,
        normalized_route,
        interactions,
        debug,
    })
}

fn compute_expected_total(resimulated: &ResimulatedRouteInternal) -> BigUint {
    let mut expected_total = BigUint::zero();
    for segment in &resimulated.segments {
        expected_total += segment.expected_amount_out.clone();
    }
    expected_total
}

fn log_resimulation_amounts(request_id: Option<&str>, resimulated: &ResimulatedRouteInternal) {
    for (segment_index, segment) in resimulated.segments.iter().enumerate() {
        info!(
            request_id,
            segment_index,
            share_bps = segment.share_bps,
            amount_in = %segment.amount_in,
            expected_amount_out = %segment.expected_amount_out,
            "Resimulated segment"
        );

        for (hop_index, hop) in segment.hops.iter().enumerate() {
            info!(
                request_id,
                segment_index,
                hop_index,
                share_bps = hop.share_bps,
                token_in = %format_address(&hop.token_in),
                token_out = %format_address(&hop.token_out),
                amount_in = %hop.amount_in,
                expected_amount_out = %hop.expected_amount_out,
                "Resimulated hop"
            );

            for (swap_index, swap) in hop.swaps.iter().enumerate() {
                info!(
                    request_id,
                    segment_index,
                    hop_index,
                    swap_index,
                    split_bps = swap.split_bps,
                    pool_id = %swap.pool.component_id,
                    amount_in = %swap.amount_in,
                    expected_amount_out = %swap.expected_amount_out,
                    "Resimulated swap"
                );
            }
        }
    }
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

fn ensure_erc20_tokens(
    chain: Chain,
    token_in: &Bytes,
    token_out: &Bytes,
) -> Result<(), EncodeError> {
    let native = chain.native_token().address;
    if *token_in == native || *token_out == native {
        return Err(EncodeError::invalid(
            "tokenIn/tokenOut must be ERC20 addresses; use wrapped native token instead",
        ));
    }
    Ok(())
}

fn should_reset_allowance(
    reset_allowance_tokens: &HashMap<u64, HashSet<Bytes>>,
    chain_id: u64,
    token_in: &Bytes,
) -> bool {
    reset_allowance_tokens
        .get(&chain_id)
        .map(|tokens| tokens.contains(token_in))
        .unwrap_or(false)
}

fn infer_route_kind(segments: &[SegmentDraft]) -> SwapKind {
    if segments.len() > 1 {
        return SwapKind::MegaSwap;
    }
    let hops_len = segments
        .first()
        .map(|segment| segment.hops.len())
        .unwrap_or(0);
    if hops_len > 1 {
        SwapKind::MultiSwap
    } else {
        SwapKind::SimpleSwap
    }
}

fn infer_segment_kind(segment: &SegmentDraft) -> SwapKind {
    if segment.hops.len() > 1 {
        SwapKind::MultiSwap
    } else {
        SwapKind::SimpleSwap
    }
}

fn validate_swap_kinds(request: &RouteEncodeRequest) -> Result<(), EncodeError> {
    if request.segments.is_empty() {
        return Err(EncodeError::invalid("segments must not be empty"));
    }

    let expected_route_kind = infer_route_kind(&request.segments);
    if request.swap_kind != expected_route_kind {
        return Err(EncodeError::invalid(format!(
            "swapKind mismatch: expected {:?}",
            expected_route_kind
        )));
    }

    for (index, segment) in request.segments.iter().enumerate() {
        let expected_segment_kind = infer_segment_kind(segment);
        if segment.kind != expected_segment_kind {
            return Err(EncodeError::invalid(format!(
                "segment[{}].kind mismatch: expected {:?}",
                index, expected_segment_kind
            )));
        }
    }

    Ok(())
}

fn normalize_route(
    request: &RouteEncodeRequest,
    request_token_in: &Bytes,
    request_token_out: &Bytes,
    total_amount_in: &BigUint,
    native_address: &Bytes,
) -> Result<NormalizedRouteInternal, EncodeError> {
    if request.segments.is_empty() {
        return Err(EncodeError::invalid("segments must not be empty"));
    }
    if total_amount_in.is_zero() {
        return Err(EncodeError::invalid("amountIn must be > 0"));
    }

    let mut normalized_segments = Vec::with_capacity(request.segments.len());
    let segment_shares = request
        .segments
        .iter()
        .map(|segment| segment.share_bps)
        .collect::<Vec<_>>();
    let segment_amounts = allocate_amounts_by_bps(total_amount_in, &segment_shares, "segment")?;

    for (segment_index, segment) in request.segments.iter().enumerate() {
        validate_segment_shape(segment, segment_index)?;

        validate_hop_continuity(segment, request_token_in, request_token_out)?;

        let hops = segment
            .hops
            .iter()
            .enumerate()
            .map(|(hop_index, hop)| normalize_hop(hop, native_address, segment_index, hop_index))
            .collect::<Result<Vec<_>, _>>()?;

        let segment_amount_in = segment_amounts
            .get(segment_index)
            .cloned()
            .ok_or_else(|| EncodeError::internal("Missing segment amount allocation"))?;
        if segment_amount_in.is_zero() {
            return Err(EncodeError::invalid(format!(
                "segment[{}] amountIn must be > 0",
                segment_index
            )));
        }

        normalized_segments.push(NormalizedSegmentInternal {
            kind: segment.kind,
            share_bps: segment.share_bps,
            amount_in: segment_amount_in,
            hops,
        });
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

fn validate_hop_share_bps(
    hop: &crate::models::messages::HopDraft,
    segment_index: usize,
    hop_index: usize,
) -> Result<(), EncodeError> {
    if hop.share_bps > BPS_DENOMINATOR {
        return Err(EncodeError::invalid(format!(
            "segment[{}].hop[{}].shareBps must be <= 10000",
            segment_index, hop_index
        )));
    }

    if hop.share_bps != 0 && hop.share_bps != BPS_DENOMINATOR {
        return Err(EncodeError::invalid(format!(
            "segment[{}].hop[{}].shareBps must be 0 or 10000 for sequential hops",
            segment_index, hop_index
        )));
    }

    Ok(())
}

fn normalize_hop(
    hop: &crate::models::messages::HopDraft,
    native_address: &Bytes,
    segment_index: usize,
    hop_index: usize,
) -> Result<NormalizedHopInternal, EncodeError> {
    if hop.swaps.is_empty() {
        return Err(EncodeError::invalid("hop.swaps must not be empty"));
    }

    validate_hop_share_bps(hop, segment_index, hop_index)?;

    let token_in = parse_address(&hop.token_in)?;
    let token_out = parse_address(&hop.token_out)?;
    if token_in == *native_address || token_out == *native_address {
        return Err(EncodeError::invalid(
            "hop tokenIn/tokenOut must be ERC20 addresses; use wrapped native token instead",
        ));
    }
    let swaps = hop
        .swaps
        .iter()
        .map(|swap| normalize_swap(swap, &token_in, &token_out))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(NormalizedHopInternal {
        share_bps: hop.share_bps,
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

    if swap.split_bps > BPS_DENOMINATOR {
        return Err(EncodeError::invalid("swap splitBps must be <= 10000"));
    }

    Ok(NormalizedSwapDraftInternal {
        pool: swap.pool.clone(),
        token_in,
        token_out,
        split_bps: swap.split_bps,
    })
}

async fn resimulate_route(
    state: &AppState,
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
        let mut resim_hops = Vec::with_capacity(segment.hops.len());
        let mut hop_amount_in = segment.amount_in.clone();

        for (hop_index, hop) in segment.hops.iter().enumerate() {
            if hop_amount_in.is_zero() {
                return Err(EncodeError::invalid(format!(
                    "segment[{}].hop[{}] amountIn is zero",
                    segment_index, hop_index
                )));
            }

            let allocated_swaps =
                allocate_swaps_by_bps(hop_amount_in.clone(), &hop.swaps, segment_index, hop_index)?;
            let mut swap_results = Vec::with_capacity(allocated_swaps.len());
            let mut hop_expected = BigUint::zero();

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

                let pre_state = Arc::clone(&pool_entry.0);
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

                // Advance cached pool state so repeated-pool swaps simulate sequentially.
                pool_cache.insert(
                    allocated.pool.component_id.clone(),
                    (Arc::from(result.new_state), Arc::clone(&pool_entry.1)),
                );

                hop_expected += expected_out.clone();

                swap_results.push(ResimulatedSwapInternal {
                    pool: allocated.pool,
                    token_in: allocated.token_in,
                    token_out: allocated.token_out,
                    split_bps: allocated.split_bps,
                    amount_in: allocated.amount_in,
                    expected_amount_out: expected_out,
                    pool_state: pre_state,
                    component: Arc::clone(&pool_entry.1),
                });

                if swap_results.len() - 1 != swap_index {
                    return Err(EncodeError::internal("Swap allocation ordering mismatch"));
                }
            }

            if hop_expected.is_zero() {
                return Err(EncodeError::simulation(format!(
                    "segment[{}].hop[{}] produced zero amountOut",
                    segment_index, hop_index
                )));
            }

            let resim_hop = ResimulatedHopInternal {
                share_bps: hop.share_bps,
                token_in: hop.token_in.clone(),
                token_out: hop.token_out.clone(),
                amount_in: hop_amount_in.clone(),
                expected_amount_out: hop_expected.clone(),
                swaps: swap_results,
            };
            resim_hops.push(resim_hop);
            hop_amount_in = hop_expected;
        }

        let last_hop = resim_hops
            .last()
            .ok_or_else(|| EncodeError::internal("Segment hops missing after resimulation"))?;

        resim_segments.push(ResimulatedSegmentInternal {
            kind: segment.kind,
            share_bps: segment.share_bps,
            amount_in: segment.amount_in.clone(),
            expected_amount_out: last_hop.expected_amount_out.clone(),
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

#[derive(Debug)]
struct AllocatedSwap {
    pool: PoolRef,
    token_in: Bytes,
    token_out: Bytes,
    split_bps: u32,
    amount_in: BigUint,
}

fn allocate_swaps_by_bps(
    hop_amount_in: BigUint,
    swaps: &[NormalizedSwapDraftInternal],
    segment_index: usize,
    hop_index: usize,
) -> Result<Vec<AllocatedSwap>, EncodeError> {
    if swaps.is_empty() {
        return Err(EncodeError::invalid("hop.swaps must not be empty"));
    }
    if hop_amount_in.is_zero() {
        return Err(EncodeError::invalid("hop amountIn must be > 0"));
    }

    let mut allocations = Vec::with_capacity(swaps.len());
    let split_bps = swaps.iter().map(|swap| swap.split_bps).collect::<Vec<_>>();
    let amounts = allocate_amounts_by_bps(
        &hop_amount_in,
        &split_bps,
        &format!("segment[{}].hop[{}].swap", segment_index, hop_index),
    )?;

    for (index, swap) in swaps.iter().enumerate() {
        let split = swap.split_bps;
        let amount_in = amounts
            .get(index)
            .cloned()
            .ok_or_else(|| EncodeError::internal("Missing swap amount allocation"))?;
        allocations.push(AllocatedSwap {
            pool: swap.pool.clone(),
            token_in: swap.token_in.clone(),
            token_out: swap.token_out.clone(),
            split_bps: split,
            amount_in,
        });
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

fn build_route_calldata_tx(
    context: &RouteContext<'_>,
    resimulated: &ResimulatedRouteInternal,
    encoder: &dyn TychoEncoder,
    min_amount_out: &BigUint,
) -> Result<CallDataPayload, EncodeError> {
    let native_address = context.chain.native_token().address;
    let wrapped_address = context.chain.wrapped_native_token().address;
    let is_wrap = context.token_in == &native_address;
    let is_unwrap = context.token_out == &native_address;
    if is_wrap && is_unwrap {
        return Err(EncodeError::invalid(
            "Native-to-native swaps are not supported",
        ));
    }

    let swaps = build_route_swaps(
        resimulated,
        is_wrap,
        is_unwrap,
        &native_address,
        &wrapped_address,
    )?;
    let sender = parse_address(&context.request.settlement_address)?;
    let receiver = sender.clone();
    let native_action = if is_wrap {
        Some(NativeAction::Wrap)
    } else if is_unwrap {
        Some(NativeAction::Unwrap)
    } else {
        None
    };

    let solution = Solution {
        sender,
        receiver: receiver.clone(),
        given_token: context.token_in.clone(),
        given_amount: context.amount_in.clone(),
        checked_token: context.token_out.clone(),
        exact_out: false,
        checked_amount: min_amount_out.clone(),
        swaps,
        native_action,
    };

    let encoded_solution = encoder
        .encode_solutions(vec![solution.clone()])
        .map_err(|err| EncodeError::encoding(format!("Route encoding failed: {}", err)))?
        .into_iter()
        .next()
        .ok_or_else(|| EncodeError::encoding("Route encoding returned no solutions"))?;

    if encoded_solution.permit.is_some()
        || encoded_solution.function_signature.contains("Permit2")
        || encoded_solution.function_signature.contains("permit2")
    {
        return Err(EncodeError::encoding(
            "Permit2 encodings are not supported for route calldata",
        ));
    }

    if &encoded_solution.interacting_with != context.router_address {
        return Err(EncodeError::encoding(
            "Encoded router address does not match request tychoRouterAddress",
        ));
    }

    let calldata =
        encode_route_calldata(&encoded_solution, &solution, &receiver, is_wrap, is_unwrap)?;
    let value = if is_wrap {
        context.amount_in.clone()
    } else {
        BigUint::zero()
    };

    Ok(CallDataPayload {
        target: encoded_solution.interacting_with,
        value,
        calldata,
    })
}

fn build_settlement_interactions(
    token_in: &Bytes,
    amount_in: &BigUint,
    router_call: CallDataPayload,
    reset_approval: bool,
) -> Result<Vec<Interaction>, EncodeError> {
    let mut interactions = Vec::with_capacity(if reset_approval { 3 } else { 2 });
    if reset_approval {
        // Reset-then-approve is required for allowance-reset tokens like USDT.
        interactions.push(build_erc20_approve_interaction(
            token_in,
            &router_call.target,
            &BigUint::zero(),
        )?);
    }
    let set_approval = build_erc20_approve_interaction(token_in, &router_call.target, amount_in)?;
    let router_interaction = Interaction {
        kind: InteractionKind::Call,
        target: format_address(&router_call.target),
        value: router_call.value.to_str_radix(10),
        calldata: format_calldata(&router_call.calldata),
    };
    interactions.push(set_approval);
    interactions.push(router_interaction);
    Ok(interactions)
}

const ERC20_APPROVE_SIGNATURE: &str = "approve(address,uint256)";

fn build_erc20_approve_interaction(
    token: &Bytes,
    spender: &Bytes,
    amount: &BigUint,
) -> Result<Interaction, EncodeError> {
    let calldata = encode_erc20_approve_calldata(spender, amount)?;
    Ok(Interaction {
        kind: InteractionKind::Erc20Approve,
        target: format_address(token),
        value: "0".to_string(),
        calldata: format_calldata(&calldata),
    })
}

fn encode_erc20_approve_calldata(
    spender: &Bytes,
    amount: &BigUint,
) -> Result<Vec<u8>, EncodeError> {
    let spender = bytes_to_address(spender).map_err(map_encoding_error)?;
    let amount = biguint_to_u256(amount);
    let calldata_args = (spender, amount).abi_encode();
    encode_function_call(ERC20_APPROVE_SIGNATURE, calldata_args)
}

fn build_route_swaps(
    resimulated: &ResimulatedRouteInternal,
    is_wrap: bool,
    is_unwrap: bool,
    native_address: &Bytes,
    wrapped_address: &Bytes,
) -> Result<Vec<Swap>, EncodeError> {
    // Tycho split validation allows only one remainder per tokenIn, so depths must be consistent.
    ensure_token_in_single_depth(resimulated)?;

    let mut ordered_swaps = Vec::new();
    // Order swaps by hop depth across segments so split sets match token availability.
    let max_hops = resimulated
        .segments
        .iter()
        .map(|segment| segment.hops.len())
        .max()
        .unwrap_or(0);
    for hop_index in 0..max_hops {
        for segment in &resimulated.segments {
            if let Some(hop) = segment.hops.get(hop_index) {
                for swap in &hop.swaps {
                    ordered_swaps.push(swap);
                }
            }
        }
    }

    let mut split_stats: HashMap<Bytes, SplitStats> = HashMap::new();
    for (index, swap) in ordered_swaps.iter().enumerate() {
        let entry = split_stats
            .entry(swap.token_in.clone())
            .or_insert_with(|| SplitStats {
                total_amount: BigUint::zero(),
                count: 0,
                last_index: index,
            });
        entry.total_amount += swap.amount_in.clone();
        entry.count += 1;
        entry.last_index = index;
    }

    let mut swaps = Vec::with_capacity(ordered_swaps.len());
    for (index, swap) in ordered_swaps.iter().enumerate() {
        let stats = split_stats
            .get(&swap.token_in)
            .ok_or_else(|| EncodeError::internal("Missing split stats for swap tokenIn"))?;
        // Tycho split validation requires exactly one remainder (split=0) per tokenIn, and it must
        // be the last occurrence in order.
        let is_remainder = stats.count == 1 || stats.last_index == index;
        let split = compute_split_fraction(
            &swap.amount_in,
            &stats.total_amount,
            is_remainder,
            stats.count,
            &swap.token_in,
        )?;
        let swap_token_in = if is_wrap && swap.token_in == *native_address {
            wrapped_address.clone()
        } else {
            swap.token_in.clone()
        };
        let swap_token_out = if is_unwrap && swap.token_out == *native_address {
            wrapped_address.clone()
        } else {
            swap.token_out.clone()
        };
        let common_component: CommonProtocolComponent = swap.component.as_ref().clone().into();
        let swap_data = Swap::new(
            common_component,
            swap_token_in,
            swap_token_out,
            split,
            None,
            Some(Arc::clone(&swap.pool_state)),
            None,
        );
        swaps.push(swap_data);
    }

    Ok(swaps)
}

fn ensure_token_in_single_depth(resimulated: &ResimulatedRouteInternal) -> Result<(), EncodeError> {
    let mut token_depths: HashMap<Bytes, usize> = HashMap::new();
    for segment in &resimulated.segments {
        for (hop_index, hop) in segment.hops.iter().enumerate() {
            for swap in &hop.swaps {
                if let Some(existing_depth) = token_depths.get(&swap.token_in) {
                    if *existing_depth != hop_index {
                        return Err(EncodeError::invalid(format!(
                            "tokenIn {} appears at multiple hop depths",
                            format_address(&swap.token_in)
                        )));
                    }
                } else {
                    token_depths.insert(swap.token_in.clone(), hop_index);
                }
            }
        }
    }
    Ok(())
}

fn compute_split_fraction(
    amount_in: &BigUint,
    total_in: &BigUint,
    is_remainder: bool,
    count: usize,
    token_in: &Bytes,
) -> Result<f64, EncodeError> {
    if count <= 1 || is_remainder {
        return Ok(0.0);
    }

    if total_in.is_zero() {
        return Err(EncodeError::invalid(
            "TokenIn total amount must be positive for split calculation",
        ));
    }
    if amount_in.is_zero() || amount_in >= total_in {
        return Err(EncodeError::invalid(format!(
            "Invalid split ratio for tokenIn {}",
            format_address(token_in)
        )));
    }

    let amount_in = amount_in.to_f64().ok_or_else(|| {
        EncodeError::invalid("Failed to convert swap amountIn for split calculation")
    })?;
    let total_in = total_in.to_f64().ok_or_else(|| {
        EncodeError::invalid("Failed to convert tokenIn total amount for split calculation")
    })?;
    let mut split = amount_in / total_in;
    if !split.is_finite() {
        return Err(EncodeError::invalid("Invalid split ratio for tokenIn"));
    }
    if split <= 0.0 {
        // Clamp away from 0 for very skewed splits that lose precision in f64.
        split = f64::MIN_POSITIVE;
    } else if split >= 1.0 {
        // Clamp away from 1 to preserve a non-remainder split.
        split = 1.0 - f64::EPSILON;
    }
    Ok(split)
}

fn encode_route_calldata(
    encoded_solution: &EncodedSolution,
    solution: &Solution,
    receiver: &Bytes,
    is_wrap: bool,
    is_unwrap: bool,
) -> Result<Vec<u8>, EncodeError> {
    let signature = encoded_solution.function_signature.as_str();
    if signature.contains("Permit2") || signature.contains("permit2") {
        return Err(EncodeError::encoding(
            "Permit2 encodings are not supported for route calldata",
        ));
    }
    let is_transfer_from_allowed = !is_wrap;

    let calldata_args = if signature.contains("splitSwap") {
        (
            biguint_to_u256(&solution.given_amount),
            bytes_to_address(&solution.given_token).map_err(map_encoding_error)?,
            bytes_to_address(&solution.checked_token).map_err(map_encoding_error)?,
            biguint_to_u256(&solution.checked_amount),
            is_wrap,
            is_unwrap,
            alloy_primitives::U256::from(encoded_solution.n_tokens),
            bytes_to_address(receiver).map_err(map_encoding_error)?,
            is_transfer_from_allowed,
            encoded_solution.swaps.clone(),
        )
            .abi_encode()
    } else if signature.contains("singleSwap") || signature.contains("sequentialSwap") {
        (
            biguint_to_u256(&solution.given_amount),
            bytes_to_address(&solution.given_token).map_err(map_encoding_error)?,
            bytes_to_address(&solution.checked_token).map_err(map_encoding_error)?,
            biguint_to_u256(&solution.checked_amount),
            is_wrap,
            is_unwrap,
            bytes_to_address(receiver).map_err(map_encoding_error)?,
            is_transfer_from_allowed,
            encoded_solution.swaps.clone(),
        )
            .abi_encode()
    } else {
        return Err(EncodeError::encoding(format!(
            "Unsupported Tycho function signature: {}",
            signature
        )));
    };

    encode_function_call(signature, calldata_args)
}

fn encode_function_call(signature: &str, encoded_args: Vec<u8>) -> Result<Vec<u8>, EncodeError> {
    let selector = function_selector(signature)?;
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

fn build_normalized_route_response(resimulated: &ResimulatedRouteInternal) -> NormalizedRoute {
    let segments = resimulated
        .segments
        .iter()
        .map(|segment| Segment {
            kind: segment.kind,
            share_bps: segment.share_bps,
            hops: segment
                .hops
                .iter()
                .map(|hop| Hop {
                    share_bps: hop.share_bps,
                    token_in: format_address(&hop.token_in),
                    token_out: format_address(&hop.token_out),
                    swaps: hop
                        .swaps
                        .iter()
                        .map(|swap| PoolSwap {
                            pool: swap.pool.clone(),
                            token_in: format_address(&swap.token_in),
                            token_out: format_address(&swap.token_out),
                            split_bps: swap.split_bps,
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
    interactions: &[Interaction],
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
            Some(config) => match simulate_tenderly_bundle(request, interactions, &config).await {
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

fn map_swap_token(address: &Bytes, chain: Chain) -> Bytes {
    if *address == chain.native_token().address {
        chain.wrapped_native_token().address
    } else {
        address.clone()
    }
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
    interactions: &[Interaction],
    config: &TenderlyConfig,
) -> Result<TenderlyDebug, EncodeError> {
    if interactions.is_empty() {
        return Ok(TenderlyDebug {
            simulation_url: None,
            simulation_id: None,
        });
    }

    let simulations: Vec<Value> = interactions
        .iter()
        .map(|interaction| {
            let value_json = tenderly_value_json(&interaction.value)?;
            Ok(json!({
                "network_id": request.chain_id.to_string(),
                "save": true,
                "save_if_fails": true,
                "simulation_type": config.simulation_type,
                "from": request.settlement_address,
                "to": interaction.target,
                "input": interaction.calldata,
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

fn mul_bps(amount: &BigUint, bps: u32) -> BigUint {
    if bps == 0 {
        return BigUint::zero();
    }
    (amount * BigUint::from(bps)) / BigUint::from(BPS_DENOMINATOR)
}

fn allocate_amounts_by_bps(
    total: &BigUint,
    shares: &[u32],
    label: &str,
) -> Result<Vec<BigUint>, EncodeError> {
    if shares.is_empty() {
        return Err(EncodeError::invalid(format!(
            "{} shares must not be empty",
            label
        )));
    }

    let last_index = shares.len() - 1;
    let mut sum_bps: u32 = 0;
    let mut allocated = BigUint::zero();
    let mut amounts = Vec::with_capacity(shares.len());

    for (index, share) in shares.iter().enumerate() {
        if *share > BPS_DENOMINATOR {
            return Err(EncodeError::invalid(format!(
                "{} shareBps must be <= 10000",
                label
            )));
        }

        if index < last_index {
            if *share == 0 {
                return Err(EncodeError::invalid(format!(
                    "{} remainder must be last",
                    label
                )));
            }
            sum_bps = sum_bps.saturating_add(*share);
            if sum_bps > BPS_DENOMINATOR {
                return Err(EncodeError::invalid(format!(
                    "{} shareBps sum exceeds 10000",
                    label
                )));
            }
            let amount = mul_bps(total, *share);
            allocated += amount.clone();
            amounts.push(amount);
        } else {
            if *share != 0 {
                return Err(EncodeError::invalid(format!(
                    "{} last shareBps must be 0 to take remainder",
                    label
                )));
            }
            let amount = total - &allocated;
            amounts.push(amount);
        }
    }

    Ok(amounts)
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
    use crate::models::state::StateStore;
    use crate::models::tokens::TokenStore;
    use chrono::NaiveDateTime;
    use std::collections::HashMap;
    use std::time::Duration;
    use tokio::sync::Semaphore;
    use tycho_execution::encoding::models::Transaction;
    use tycho_execution::encoding::tycho_encoder::TychoEncoder;
    use tycho_simulation::protocol::models::Update;
    use tycho_simulation::tycho_common::models::Chain;

    fn pool_ref(id: &str) -> PoolRef {
        PoolRef {
            protocol: "uniswap_v2".to_string(),
            component_id: id.to_string(),
            pool_address: None,
        }
    }

    #[test]
    fn allocate_swaps_by_bps_applies_remainder() {
        let hop_amount = BigUint::from(100u32);
        let swaps = vec![
            NormalizedSwapDraftInternal {
                pool: pool_ref("p1"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 3000,
            },
            NormalizedSwapDraftInternal {
                pool: pool_ref("p2"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 0,
            },
        ];

        let allocated = allocate_swaps_by_bps(hop_amount, &swaps, 0, 0).expect("allocates");
        assert_eq!(allocated.len(), 2);
        assert_eq!(allocated[0].amount_in, BigUint::from(30u32));
        assert_eq!(allocated[1].amount_in, BigUint::from(70u32));
        assert_eq!(allocated[0].split_bps, 3000);
        assert_eq!(allocated[1].split_bps, 0);
    }

    #[test]
    fn allocate_swaps_by_bps_rejects_non_last_remainder() {
        let hop_amount = BigUint::from(100u32);
        let swaps = vec![
            NormalizedSwapDraftInternal {
                pool: pool_ref("p1"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 0,
            },
            NormalizedSwapDraftInternal {
                pool: pool_ref("p2"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 5000,
            },
        ];

        let err =
            allocate_swaps_by_bps(hop_amount, &swaps, 0, 0).expect_err("remainder must be last");
        assert_eq!(err.kind(), EncodeErrorKind::InvalidRequest);
    }

    #[test]
    fn allocate_swaps_by_bps_rejects_non_zero_last_share() {
        let hop_amount = BigUint::from(100u32);
        let swaps = vec![
            NormalizedSwapDraftInternal {
                pool: pool_ref("p1"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 6000,
            },
            NormalizedSwapDraftInternal {
                pool: pool_ref("p2"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 4000,
            },
        ];

        let err = allocate_swaps_by_bps(hop_amount, &swaps, 0, 0)
            .expect_err("last share must be zero to take remainder");
        assert_eq!(err.kind(), EncodeErrorKind::InvalidRequest);
    }

    #[test]
    fn normalize_route_computes_segment_amounts_from_share_bps() {
        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "100".to_string(),
            min_amount_out: "90".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: "0x0000000000000000000000000000000000000004".to_string(),
            swap_kind: SwapKind::MegaSwap,
            segments: vec![
                SegmentDraft {
                    kind: SwapKind::SimpleSwap,
                    share_bps: 2000,
                    hops: vec![HopDraft {
                        share_bps: 10000,
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p1"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: 0,
                        }],
                    }],
                },
                SegmentDraft {
                    kind: SwapKind::SimpleSwap,
                    share_bps: 0,
                    hops: vec![HopDraft {
                        share_bps: 10000,
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p2"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: 0,
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

        let native_address = Chain::Ethereum.native_token().address;
        let normalized =
            normalize_route(&request, &token_in, &token_out, &amount_in, &native_address).unwrap();
        assert_eq!(normalized.segments.len(), 2);
        assert_eq!(normalized.segments[0].share_bps, 2000);
        assert_eq!(normalized.segments[1].share_bps, 0);
        assert_eq!(normalized.segments[0].amount_in, BigUint::from(20u32));
        assert_eq!(normalized.segments[1].amount_in, BigUint::from(80u32));
    }

    #[test]
    fn normalize_route_rejects_non_zero_last_segment_share() {
        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "100".to_string(),
            min_amount_out: "90".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: "0x0000000000000000000000000000000000000004".to_string(),
            swap_kind: SwapKind::MegaSwap,
            segments: vec![
                SegmentDraft {
                    kind: SwapKind::SimpleSwap,
                    share_bps: 4000,
                    hops: vec![HopDraft {
                        share_bps: 10000,
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p1"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: 0,
                        }],
                    }],
                },
                SegmentDraft {
                    kind: SwapKind::SimpleSwap,
                    share_bps: 6000,
                    hops: vec![HopDraft {
                        share_bps: 10000,
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p2"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: 0,
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
        let native_address = Chain::Ethereum.native_token().address;

        match normalize_route(&request, &token_in, &token_out, &amount_in, &native_address) {
            Err(err) => assert_eq!(err.kind(), EncodeErrorKind::InvalidRequest),
            Ok(_) => panic!("non-zero last segment share should be rejected"),
        }
    }
    #[test]
    fn normalize_route_rejects_non_10000_hop_share_for_multihop() {
        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "100".to_string(),
            min_amount_out: "90".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: "0x0000000000000000000000000000000000000004".to_string(),
            swap_kind: SwapKind::MultiSwap,
            segments: vec![SegmentDraft {
                kind: SwapKind::MultiSwap,
                share_bps: 0,
                hops: vec![
                    HopDraft {
                        share_bps: 5000,
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: "0x0000000000000000000000000000000000000003".to_string(),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p1"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: "0x0000000000000000000000000000000000000003".to_string(),
                            split_bps: 0,
                        }],
                    },
                    HopDraft {
                        share_bps: 10000,
                        token_in: "0x0000000000000000000000000000000000000003".to_string(),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p2"),
                            token_in: "0x0000000000000000000000000000000000000003".to_string(),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: 0,
                        }],
                    },
                ],
            }],
            enable_tenderly_sim: None,
            request_id: None,
        };

        let token_in = parse_address(&request.token_in).unwrap();
        let token_out = parse_address(&request.token_out).unwrap();
        let amount_in = parse_amount(&request.amount_in).unwrap();

        let native_address = Chain::Ethereum.native_token().address;
        match normalize_route(&request, &token_in, &token_out, &amount_in, &native_address) {
            Err(err) => assert_eq!(err.kind(), EncodeErrorKind::InvalidRequest),
            Ok(_) => panic!("invalid hop share should be rejected"),
        }
    }

    #[test]
    fn normalize_route_rejects_native_hops() {
        let native = Chain::Ethereum.native_token().address;
        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "100".to_string(),
            min_amount_out: "90".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: "0x0000000000000000000000000000000000000004".to_string(),
            swap_kind: SwapKind::MultiSwap,
            segments: vec![SegmentDraft {
                kind: SwapKind::MultiSwap,
                share_bps: 0,
                hops: vec![
                    HopDraft {
                        share_bps: 10000,
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: format_address(&native),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p1"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: format_address(&native),
                            split_bps: 0,
                        }],
                    },
                    HopDraft {
                        share_bps: 10000,
                        token_in: format_address(&native),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p2"),
                            token_in: format_address(&native),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: 0,
                        }],
                    },
                ],
            }],
            enable_tenderly_sim: None,
            request_id: None,
        };

        let token_in = parse_address(&request.token_in).unwrap();
        let token_out = parse_address(&request.token_out).unwrap();
        let amount_in = parse_amount(&request.amount_in).unwrap();

        match normalize_route(&request, &token_in, &token_out, &amount_in, &native) {
            Err(err) => assert_eq!(err.kind(), EncodeErrorKind::InvalidRequest),
            Ok(_) => panic!("native hop tokens should be rejected"),
        }
    }

    #[test]
    fn validate_swap_kinds_rejects_route_mismatch() {
        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "100".to_string(),
            min_amount_out: "90".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: "0x0000000000000000000000000000000000000004".to_string(),
            swap_kind: SwapKind::SimpleSwap,
            segments: vec![
                SegmentDraft {
                    kind: SwapKind::SimpleSwap,
                    share_bps: 5000,
                    hops: vec![HopDraft {
                        share_bps: 10000,
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p1"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: 0,
                        }],
                    }],
                },
                SegmentDraft {
                    kind: SwapKind::SimpleSwap,
                    share_bps: 0,
                    hops: vec![HopDraft {
                        share_bps: 10000,
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![PoolSwapDraft {
                            pool: pool_ref("p2"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: 0,
                        }],
                    }],
                },
            ],
            enable_tenderly_sim: None,
            request_id: None,
        };

        match validate_swap_kinds(&request) {
            Err(err) => assert_eq!(err.kind(), EncodeErrorKind::InvalidRequest),
            Ok(_) => panic!("swapKind mismatch should be rejected"),
        }
    }

    #[test]
    fn ensure_erc20_tokens_rejects_native() {
        let native = Chain::Ethereum.native_token().address;
        let erc20 = Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap();

        match ensure_erc20_tokens(Chain::Ethereum, &native, &erc20) {
            Err(err) => assert_eq!(err.kind(), EncodeErrorKind::InvalidRequest),
            Ok(_) => panic!("native token should be rejected for settlement encoding"),
        }
    }

    #[test]
    fn compute_split_fraction_clamps_rounding() {
        let total_in = BigUint::from(1u64) << 60u32;
        let amount_in = total_in.clone() - BigUint::from(1u32);
        let token_in = Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap();

        let split = compute_split_fraction(&amount_in, &total_in, false, 2, &token_in).unwrap();
        assert!(split > 0.0, "split should remain positive");
        assert!(split < 1.0, "split should remain less than 1");
    }

    #[test]
    fn build_settlement_interactions_with_reset_approval() {
        let token_in = Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap();
        let router = Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap();
        let amount_in = BigUint::from(10u32);
        let router_call = CallDataPayload {
            target: router.clone(),
            value: BigUint::zero(),
            calldata: vec![0x11, 0x22, 0x33],
        };

        let interactions =
            build_settlement_interactions(&token_in, &amount_in, router_call, true).unwrap();

        assert_eq!(interactions.len(), 3);
        assert_eq!(interactions[0].kind, InteractionKind::Erc20Approve);
        assert_eq!(interactions[1].kind, InteractionKind::Erc20Approve);
        assert_eq!(interactions[2].kind, InteractionKind::Call);
        assert_eq!(interactions[0].target, format_address(&token_in));
        assert_eq!(interactions[2].target, format_address(&router));

        let selector = function_selector(ERC20_APPROVE_SIGNATURE).unwrap();
        let expected_prefix = format!("0x{}", alloy_primitives::hex::encode(selector));
        assert!(
            interactions[0].calldata.starts_with(&expected_prefix),
            "approval calldata should start with selector"
        );
        assert!(
            interactions[1].calldata.starts_with(&expected_prefix),
            "approval calldata should start with selector"
        );
        assert_eq!(
            interactions[2].calldata,
            format_calldata(&[0x11, 0x22, 0x33])
        );
    }

    #[test]
    fn build_settlement_interactions_without_reset_approval() {
        let token_in = Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap();
        let router = Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap();
        let amount_in = BigUint::from(10u32);
        let router_call = CallDataPayload {
            target: router.clone(),
            value: BigUint::zero(),
            calldata: vec![0xaa, 0xbb, 0xcc],
        };

        let interactions =
            build_settlement_interactions(&token_in, &amount_in, router_call, false).unwrap();

        assert_eq!(interactions.len(), 2);
        assert_eq!(interactions[0].kind, InteractionKind::Erc20Approve);
        assert_eq!(interactions[1].kind, InteractionKind::Call);
        assert_eq!(interactions[0].target, format_address(&token_in));
        assert_eq!(interactions[1].target, format_address(&router));
    }

    #[test]
    fn build_route_calldata_tx_encodes_router_call() {
        let router = Bytes::from_str("0x0000000000000000000000000000000000000004").unwrap();
        let encoder = MockTychoEncoder::new(
            "singleSwap(uint256,address,address,uint256,bool,bool,address,bool,bytes)",
            router.clone(),
        );

        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "10".to_string(),
            min_amount_out: "8".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: format_address(&router),
            swap_kind: SwapKind::SimpleSwap,
            segments: Vec::new(),
            enable_tenderly_sim: None,
            request_id: None,
        };

        let resimulated = ResimulatedRouteInternal {
            segments: vec![ResimulatedSegmentInternal {
                kind: SwapKind::SimpleSwap,
                share_bps: 10_000,
                amount_in: BigUint::from(10u32),
                expected_amount_out: BigUint::from(9u32),
                hops: vec![ResimulatedHopInternal {
                    share_bps: 10_000,
                    token_in: Bytes::from_str("0x0000000000000000000000000000000000000001")
                        .unwrap(),
                    token_out: Bytes::from_str("0x0000000000000000000000000000000000000002")
                        .unwrap(),
                    amount_in: BigUint::from(10u32),
                    expected_amount_out: BigUint::from(9u32),
                    swaps: vec![ResimulatedSwapInternal {
                        pool: pool_ref("p1"),
                        token_in: Bytes::from_str("0x0000000000000000000000000000000000000001")
                            .unwrap(),
                        token_out: Bytes::from_str("0x0000000000000000000000000000000000000002")
                            .unwrap(),
                        split_bps: 10_000,
                        amount_in: BigUint::from(10u32),
                        expected_amount_out: BigUint::from(9u32),
                        pool_state: Arc::new(MockProtocolSim {}),
                        component: Arc::new(dummy_component()),
                    }],
                }],
            }],
        };

        let token_in = parse_address(&request.token_in).unwrap();
        let token_out = parse_address(&request.token_out).unwrap();
        let amount_in = parse_amount(&request.amount_in).unwrap();

        let context = RouteContext {
            request: &request,
            chain: Chain::Ethereum,
            token_in: &token_in,
            token_out: &token_out,
            amount_in: &amount_in,
            router_address: &router,
        };

        let calldata =
            build_route_calldata_tx(&context, &resimulated, &encoder, &BigUint::from(8u32))
                .unwrap();

        assert_eq!(calldata.target, router);
        assert_eq!(calldata.value, BigUint::zero());
        let selector = function_selector(
            "singleSwap(uint256,address,address,uint256,bool,bool,address,bool,bytes)",
        )
        .unwrap();
        assert_eq!(&calldata.calldata[..4], &selector);
    }

    #[test]
    fn encode_function_call_preserves_leading_word() {
        let signature = "singleSwap(uint256,address,address,uint256,bool,bool,address,bool,bytes)";
        let calldata_args = (
            alloy_primitives::U256::from(32u32),
            alloy_primitives::Address::from([0x11; 20]),
            alloy_primitives::Address::from([0x22; 20]),
            alloy_primitives::U256::from(1u32),
            false,
            false,
            alloy_primitives::Address::from([0x33; 20]),
            false,
            vec![0x01u8, 0x02u8],
        )
            .abi_encode();

        let calldata = encode_function_call(signature, calldata_args.clone()).unwrap();
        assert_eq!(&calldata[4..], calldata_args.as_slice());
    }

    #[tokio::test]
    async fn resimulate_route_advances_pool_state_for_repeated_swaps() {
        let token_in = dummy_token("0x0000000000000000000000000000000000000001");
        let token_out = dummy_token("0x0000000000000000000000000000000000000002");
        let mut tokens = HashMap::new();
        tokens.insert(token_in.address.clone(), token_in.clone());
        tokens.insert(token_out.address.clone(), token_out.clone());

        let tokens_store = Arc::new(TokenStore::new(
            tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_millis(10),
        ));
        let state_store = Arc::new(StateStore::new(Arc::clone(&tokens_store)));

        let component = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000009").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_in.clone(), token_out.clone()],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );
        let mut states = HashMap::new();
        states.insert(
            "pool-1".to_string(),
            Box::new(StepProtocolSim { multiplier: 1 }) as Box<dyn ProtocolSim>,
        );
        let mut new_pairs = HashMap::new();
        new_pairs.insert("pool-1".to_string(), component);
        let update = Update::new(1, states, new_pairs);
        state_store.apply_update(update).await;

        let app_state = AppState {
            tokens: Arc::clone(&tokens_store),
            state_store: Arc::clone(&state_store),
            quote_timeout: Duration::from_millis(10),
            pool_timeout_native: Duration::from_millis(10),
            pool_timeout_vm: Duration::from_millis(10),
            request_timeout: Duration::from_millis(10),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
        };

        let normalized = NormalizedRouteInternal {
            segments: vec![NormalizedSegmentInternal {
                kind: SwapKind::SimpleSwap,
                share_bps: 10_000,
                amount_in: BigUint::from(10u32),
                hops: vec![NormalizedHopInternal {
                    share_bps: 10_000,
                    token_in: token_in.address.clone(),
                    token_out: token_out.address.clone(),
                    swaps: vec![
                        NormalizedSwapDraftInternal {
                            pool: pool_ref("pool-1"),
                            token_in: token_in.address.clone(),
                            token_out: token_out.address.clone(),
                            split_bps: 5000,
                        },
                        NormalizedSwapDraftInternal {
                            pool: pool_ref("pool-1"),
                            token_in: token_in.address.clone(),
                            token_out: token_out.address.clone(),
                            split_bps: 0,
                        },
                    ],
                }],
            }],
        };

        let resimulated = resimulate_route(
            &app_state,
            &normalized,
            Chain::Ethereum,
            &token_in.address,
            &token_out.address,
        )
        .await
        .unwrap();

        let swaps = &resimulated.segments[0].hops[0].swaps;
        assert_eq!(swaps[0].expected_amount_out, BigUint::from(5u32));
        assert_eq!(swaps[1].expected_amount_out, BigUint::from(10u32));
        assert_eq!(step_multiplier(&swaps[0].pool_state), 1);
        assert_eq!(step_multiplier(&swaps[1].pool_state), 2);
    }

    #[test]
    fn build_route_swaps_sets_remainder_split_last() {
        let resimulated = ResimulatedRouteInternal {
            segments: vec![ResimulatedSegmentInternal {
                kind: SwapKind::SimpleSwap,
                share_bps: 10_000,
                amount_in: BigUint::from(100u32),
                expected_amount_out: BigUint::from(90u32),
                hops: vec![ResimulatedHopInternal {
                    share_bps: 10_000,
                    token_in: Bytes::from_str("0x0000000000000000000000000000000000000001")
                        .unwrap(),
                    token_out: Bytes::from_str("0x0000000000000000000000000000000000000002")
                        .unwrap(),
                    amount_in: BigUint::from(100u32),
                    expected_amount_out: BigUint::from(90u32),
                    swaps: vec![
                        ResimulatedSwapInternal {
                            pool: pool_ref("p1"),
                            token_in: Bytes::from_str("0x0000000000000000000000000000000000000001")
                                .unwrap(),
                            token_out: Bytes::from_str(
                                "0x0000000000000000000000000000000000000002",
                            )
                            .unwrap(),
                            split_bps: 3_000,
                            amount_in: BigUint::from(30u32),
                            expected_amount_out: BigUint::from(27u32),
                            pool_state: Arc::new(MockProtocolSim {}),
                            component: Arc::new(dummy_component()),
                        },
                        ResimulatedSwapInternal {
                            pool: pool_ref("p2"),
                            token_in: Bytes::from_str("0x0000000000000000000000000000000000000001")
                                .unwrap(),
                            token_out: Bytes::from_str(
                                "0x0000000000000000000000000000000000000002",
                            )
                            .unwrap(),
                            split_bps: 7_000,
                            amount_in: BigUint::from(70u32),
                            expected_amount_out: BigUint::from(63u32),
                            pool_state: Arc::new(MockProtocolSim {}),
                            component: Arc::new(dummy_component()),
                        },
                    ],
                }],
            }],
        };

        let native = Chain::Ethereum.native_token().address;
        let wrapped = Chain::Ethereum.wrapped_native_token().address;
        let swaps = build_route_swaps(&resimulated, false, false, &native, &wrapped).unwrap();

        assert_eq!(swaps.len(), 2);
        assert!(swaps[0].split > 0.0);
        assert_eq!(swaps[1].split, 0.0);
    }

    #[test]
    fn build_route_swaps_orders_by_hop_depth_with_shared_intermediate() {
        let token_a = Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap();
        let token_b = Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap();
        let token_c = Bytes::from_str("0x0000000000000000000000000000000000000003").unwrap();
        let token_d = Bytes::from_str("0x0000000000000000000000000000000000000004").unwrap();
        let pool_state: Arc<dyn ProtocolSim> = Arc::new(MockProtocolSim {});
        let component = Arc::new(dummy_component());
        let resimulated = ResimulatedRouteInternal {
            segments: vec![
                ResimulatedSegmentInternal {
                    kind: SwapKind::MultiSwap,
                    share_bps: 6_000,
                    amount_in: BigUint::from(60u32),
                    expected_amount_out: BigUint::from(55u32),
                    hops: vec![
                        ResimulatedHopInternal {
                            share_bps: 10_000,
                            token_in: token_a.clone(),
                            token_out: token_b.clone(),
                            amount_in: BigUint::from(60u32),
                            expected_amount_out: BigUint::from(58u32),
                            swaps: vec![ResimulatedSwapInternal {
                                pool: pool_ref("p1"),
                                token_in: token_a.clone(),
                                token_out: token_b.clone(),
                                split_bps: 6_000,
                                amount_in: BigUint::from(60u32),
                                expected_amount_out: BigUint::from(58u32),
                                pool_state: Arc::clone(&pool_state),
                                component: Arc::clone(&component),
                            }],
                        },
                        ResimulatedHopInternal {
                            share_bps: 10_000,
                            token_in: token_b.clone(),
                            token_out: token_c.clone(),
                            amount_in: BigUint::from(60u32),
                            expected_amount_out: BigUint::from(55u32),
                            swaps: vec![ResimulatedSwapInternal {
                                pool: pool_ref("p2"),
                                token_in: token_b.clone(),
                                token_out: token_c.clone(),
                                split_bps: 6_000,
                                amount_in: BigUint::from(60u32),
                                expected_amount_out: BigUint::from(55u32),
                                pool_state: Arc::clone(&pool_state),
                                component: Arc::clone(&component),
                            }],
                        },
                    ],
                },
                ResimulatedSegmentInternal {
                    kind: SwapKind::MultiSwap,
                    share_bps: 4_000,
                    amount_in: BigUint::from(40u32),
                    expected_amount_out: BigUint::from(37u32),
                    hops: vec![
                        ResimulatedHopInternal {
                            share_bps: 10_000,
                            token_in: token_a.clone(),
                            token_out: token_b.clone(),
                            amount_in: BigUint::from(40u32),
                            expected_amount_out: BigUint::from(39u32),
                            swaps: vec![ResimulatedSwapInternal {
                                pool: pool_ref("p3"),
                                token_in: token_a.clone(),
                                token_out: token_b.clone(),
                                split_bps: 4_000,
                                amount_in: BigUint::from(40u32),
                                expected_amount_out: BigUint::from(39u32),
                                pool_state: Arc::clone(&pool_state),
                                component: Arc::clone(&component),
                            }],
                        },
                        ResimulatedHopInternal {
                            share_bps: 10_000,
                            token_in: token_b.clone(),
                            token_out: token_d.clone(),
                            amount_in: BigUint::from(40u32),
                            expected_amount_out: BigUint::from(37u32),
                            swaps: vec![ResimulatedSwapInternal {
                                pool: pool_ref("p4"),
                                token_in: token_b.clone(),
                                token_out: token_d.clone(),
                                split_bps: 4_000,
                                amount_in: BigUint::from(40u32),
                                expected_amount_out: BigUint::from(37u32),
                                pool_state: Arc::clone(&pool_state),
                                component: Arc::clone(&component),
                            }],
                        },
                    ],
                },
            ],
        };

        let native = Chain::Ethereum.native_token().address;
        let wrapped = Chain::Ethereum.wrapped_native_token().address;
        let swaps = build_route_swaps(&resimulated, false, false, &native, &wrapped).unwrap();

        assert_eq!(swaps.len(), 4);
        assert_eq!(swaps[0].token_in, token_a);
        assert_eq!(swaps[0].token_out, token_b);
        assert_eq!(swaps[1].token_in, token_a);
        assert_eq!(swaps[1].token_out, token_b);
        assert_eq!(swaps[2].token_in, token_b);
        assert_eq!(swaps[2].token_out, token_c);
        assert_eq!(swaps[3].token_in, token_b);
        assert_eq!(swaps[3].token_out, token_d);
        assert!((swaps[0].split - 0.6).abs() < 1e-9);
        assert_eq!(swaps[1].split, 0.0);
        assert!((swaps[2].split - 0.6).abs() < 1e-9);
        assert_eq!(swaps[3].split, 0.0);
    }

    #[test]
    fn build_route_swaps_rejects_token_in_at_multiple_depths() {
        let token_a = Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap();
        let token_b = Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap();
        let token_c = Bytes::from_str("0x0000000000000000000000000000000000000003").unwrap();
        let pool_state: Arc<dyn ProtocolSim> = Arc::new(MockProtocolSim {});
        let component = Arc::new(dummy_component());
        let resimulated = ResimulatedRouteInternal {
            segments: vec![ResimulatedSegmentInternal {
                kind: SwapKind::MultiSwap,
                share_bps: 10_000,
                amount_in: BigUint::from(100u32),
                expected_amount_out: BigUint::from(90u32),
                hops: vec![
                    ResimulatedHopInternal {
                        share_bps: 10_000,
                        token_in: token_a.clone(),
                        token_out: token_b.clone(),
                        amount_in: BigUint::from(100u32),
                        expected_amount_out: BigUint::from(95u32),
                        swaps: vec![ResimulatedSwapInternal {
                            pool: pool_ref("p1"),
                            token_in: token_a.clone(),
                            token_out: token_b.clone(),
                            split_bps: 10_000,
                            amount_in: BigUint::from(100u32),
                            expected_amount_out: BigUint::from(95u32),
                            pool_state: Arc::clone(&pool_state),
                            component: Arc::clone(&component),
                        }],
                    },
                    ResimulatedHopInternal {
                        share_bps: 10_000,
                        token_in: token_b.clone(),
                        token_out: token_a.clone(),
                        amount_in: BigUint::from(95u32),
                        expected_amount_out: BigUint::from(92u32),
                        swaps: vec![ResimulatedSwapInternal {
                            pool: pool_ref("p2"),
                            token_in: token_b.clone(),
                            token_out: token_a.clone(),
                            split_bps: 10_000,
                            amount_in: BigUint::from(95u32),
                            expected_amount_out: BigUint::from(92u32),
                            pool_state: Arc::clone(&pool_state),
                            component: Arc::clone(&component),
                        }],
                    },
                    ResimulatedHopInternal {
                        share_bps: 10_000,
                        token_in: token_a.clone(),
                        token_out: token_c.clone(),
                        amount_in: BigUint::from(92u32),
                        expected_amount_out: BigUint::from(90u32),
                        swaps: vec![ResimulatedSwapInternal {
                            pool: pool_ref("p3"),
                            token_in: token_a.clone(),
                            token_out: token_c.clone(),
                            split_bps: 10_000,
                            amount_in: BigUint::from(92u32),
                            expected_amount_out: BigUint::from(90u32),
                            pool_state: Arc::clone(&pool_state),
                            component: Arc::clone(&component),
                        }],
                    },
                ],
            }],
        };

        let native = Chain::Ethereum.native_token().address;
        let wrapped = Chain::Ethereum.wrapped_native_token().address;
        let err = build_route_swaps(&resimulated, false, false, &native, &wrapped)
            .expect_err("rejects repeated token depth");
        assert!(
            err.message.contains("multiple hop depths"),
            "unexpected error: {err:?}"
        );
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

    #[derive(Debug)]
    struct StepProtocolSim {
        multiplier: u32,
    }

    impl ProtocolSim for StepProtocolSim {
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
            let amount_out = amount_in * BigUint::from(self.multiplier);
            let next = StepProtocolSim {
                multiplier: self.multiplier + 1,
            };
            Ok(
                tycho_simulation::tycho_common::simulation::protocol_sim::GetAmountOutResult::new(
                    amount_out,
                    BigUint::zero(),
                    Box::new(next),
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
            Box::new(Self {
                multiplier: self.multiplier,
            })
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other
                .as_any()
                .downcast_ref::<StepProtocolSim>()
                .is_some_and(|rhs| rhs.multiplier == self.multiplier)
        }
    }

    fn step_multiplier(state: &Arc<dyn ProtocolSim>) -> u32 {
        state
            .as_any()
            .downcast_ref::<StepProtocolSim>()
            .expect("Expected StepProtocolSim")
            .multiplier
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
