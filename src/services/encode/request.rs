use std::collections::{HashMap, HashSet};

use tycho_simulation::tycho_common::{models::Chain, Bytes};

use crate::models::messages::{RouteEncodeRequest, SegmentDraft, SwapKind};

use super::EncodeError;

pub(super) fn validate_chain(chain_id: u64) -> Result<Chain, EncodeError> {
    let chain = Chain::Ethereum;
    if chain.id() != chain_id {
        return Err(EncodeError::invalid(format!(
            "Unsupported chainId: {}",
            chain_id
        )));
    }
    Ok(chain)
}

pub(super) fn ensure_erc20_tokens(
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

pub(super) fn should_reset_allowance(
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

pub(super) fn validate_swap_kinds(request: &RouteEncodeRequest) -> Result<(), EncodeError> {
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

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
                    hops: vec![crate::models::messages::HopDraft {
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![crate::models::messages::PoolSwapDraft {
                            pool: crate::services::encode::test_support::pool_ref("p1"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: 0,
                        }],
                    }],
                },
                SegmentDraft {
                    kind: SwapKind::SimpleSwap,
                    share_bps: 0,
                    hops: vec![crate::models::messages::HopDraft {
                        token_in: "0x0000000000000000000000000000000000000001".to_string(),
                        token_out: "0x0000000000000000000000000000000000000002".to_string(),
                        swaps: vec![crate::models::messages::PoolSwapDraft {
                            pool: crate::services::encode::test_support::pool_ref("p2"),
                            token_in: "0x0000000000000000000000000000000000000001".to_string(),
                            token_out: "0x0000000000000000000000000000000000000002".to_string(),
                            split_bps: 0,
                        }],
                    }],
                },
            ],
            request_id: None,
        };

        match validate_swap_kinds(&request) {
            Err(err) => assert_eq!(
                err.kind(),
                crate::services::encode::EncodeErrorKind::InvalidRequest
            ),
            Ok(_) => panic!("swapKind mismatch should be rejected"),
        }
    }

    #[test]
    fn ensure_erc20_tokens_rejects_native() {
        let native = Chain::Ethereum.native_token().address;
        let erc20 = Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap();

        match ensure_erc20_tokens(Chain::Ethereum, &native, &erc20) {
            Err(err) => assert_eq!(
                err.kind(),
                crate::services::encode::EncodeErrorKind::InvalidRequest
            ),
            Ok(_) => panic!("native token should be rejected for settlement encoding"),
        }
    }
}
