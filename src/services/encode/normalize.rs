use num_bigint::BigUint;
use num_traits::Zero;
use tycho_simulation::tycho_common::Bytes;

use crate::models::messages::{HopDraft, PoolSwapDraft, RouteEncodeRequest, SegmentDraft};

use super::allocation::{allocate_amounts_by_bps, BPS_DENOMINATOR};
use super::model::{
    NormalizedHopInternal, NormalizedRouteInternal, NormalizedSegmentInternal,
    NormalizedSwapDraftInternal,
};
use super::request::{is_native_protocol_allowlisted, native_protocol_allowlist};
use super::wire::parse_address;
use super::EncodeError;

pub(super) fn normalize_route(
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
    let segment_amounts =
        allocate_amounts_by_bps(total_amount_in, &segment_shares, "segment", "shareBps")?;

    for (segment_index, segment) in request.segments.iter().enumerate() {
        validate_segment_shape(segment, segment_index)?;

        validate_hop_continuity(segment, request_token_in, request_token_out)?;

        let hops = segment
            .hops
            .iter()
            .map(|hop| normalize_hop(hop, native_address))
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

fn normalize_hop(
    hop: &HopDraft,
    native_address: &Bytes,
) -> Result<NormalizedHopInternal, EncodeError> {
    if hop.swaps.is_empty() {
        return Err(EncodeError::invalid("hop.swaps must not be empty"));
    }

    let token_in = parse_address(&hop.token_in)?;
    let token_out = parse_address(&hop.token_out)?;
    if token_in == *native_address || token_out == *native_address {
        for swap in &hop.swaps {
            if !is_native_protocol_allowlisted(&swap.pool.protocol) {
                let supported = native_protocol_allowlist().join(", ");
                return Err(EncodeError::invalid(format!(
                    "native tokenIn/tokenOut is only supported for protocols [{}]; got {}",
                    supported, swap.pool.protocol
                )));
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::models::messages::PoolRef;
    use crate::models::messages::{PoolSwapDraft, SwapKind};
    use crate::services::encode::test_support::pool_ref;
    use crate::services::encode::wire::{format_address, parse_address, parse_amount};
    use tycho_simulation::tycho_common::models::Chain;

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
    fn normalize_route_rejects_segment_share_rounding_to_zero() {
        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "1".to_string(),
            min_amount_out: "1".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: "0x0000000000000000000000000000000000000004".to_string(),
            swap_kind: SwapKind::MegaSwap,
            segments: vec![
                SegmentDraft {
                    kind: SwapKind::SimpleSwap,
                    share_bps: 1,
                    hops: vec![HopDraft {
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
            request_id: None,
        };

        let token_in = Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap();
        let token_out = Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap();
        let amount_in = BigUint::from(1u32);
        let native = Chain::Ethereum.native_token().address;

        let err = match normalize_route(&request, &token_in, &token_out, &amount_in, &native) {
            Ok(_) => panic!("Expected segment share rounding to zero to be rejected"),
            Err(err) => err,
        };
        assert_eq!(
            err.kind(),
            crate::services::encode::EncodeErrorKind::InvalidRequest
        );
        assert!(
            err.message().contains("rounds down to zero"),
            "unexpected error: {}",
            err.message()
        );
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
            request_id: None,
        };

        let token_in = parse_address(&request.token_in).unwrap();
        let token_out = parse_address(&request.token_out).unwrap();
        let amount_in = parse_amount(&request.amount_in).unwrap();
        let native_address = Chain::Ethereum.native_token().address;

        match normalize_route(&request, &token_in, &token_out, &amount_in, &native_address) {
            Err(err) => assert_eq!(
                err.kind(),
                crate::services::encode::EncodeErrorKind::InvalidRequest
            ),
            Ok(_) => panic!("non-zero last segment share should be rejected"),
        }
    }

    #[test]
    fn normalize_route_rejects_native_hops_for_non_allowlisted_protocols() {
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
            request_id: None,
        };

        let token_in = parse_address(&request.token_in).unwrap();
        let token_out = parse_address(&request.token_out).unwrap();
        let amount_in = parse_amount(&request.amount_in).unwrap();

        match normalize_route(&request, &token_in, &token_out, &amount_in, &native) {
            Err(err) => assert_eq!(
                err.kind(),
                crate::services::encode::EncodeErrorKind::InvalidRequest
            ),
            Ok(_) => panic!("native hop tokens should be rejected for non-allowlisted protocols"),
        }
    }

    #[test]
    fn normalize_route_allows_native_hops_for_rocketpool() {
        let native = Chain::Ethereum.native_token().address;
        let token_out = Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap();
        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: format_address(&native),
            token_out: format_address(&token_out),
            amount_in: "100".to_string(),
            min_amount_out: "90".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: "0x0000000000000000000000000000000000000004".to_string(),
            swap_kind: SwapKind::SimpleSwap,
            segments: vec![SegmentDraft {
                kind: SwapKind::SimpleSwap,
                share_bps: 0,
                hops: vec![HopDraft {
                    token_in: format_address(&native),
                    token_out: format_address(&token_out),
                    swaps: vec![PoolSwapDraft {
                        pool: PoolRef {
                            protocol: "rocketpool".to_string(),
                            component_id: "p1".to_string(),
                            pool_address: None,
                        },
                        token_in: format_address(&native),
                        token_out: format_address(&token_out),
                        split_bps: 0,
                    }],
                }],
            }],
            request_id: None,
        };

        let token_in = parse_address(&request.token_in).unwrap();
        let request_token_out = parse_address(&request.token_out).unwrap();
        let amount_in = parse_amount(&request.amount_in).unwrap();

        let normalized =
            normalize_route(&request, &token_in, &request_token_out, &amount_in, &native)
                .expect("rocketpool native route should normalize");
        assert_eq!(normalized.segments.len(), 1);
        assert_eq!(normalized.segments[0].hops.len(), 1);
        assert_eq!(normalized.segments[0].hops[0].token_in, native);
        assert_eq!(normalized.segments[0].hops[0].token_out, token_out);
    }
}
