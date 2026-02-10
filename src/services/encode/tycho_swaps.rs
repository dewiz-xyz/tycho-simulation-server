use std::collections::HashMap;
use std::sync::Arc;

use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};
use tycho_execution::encoding::models::Swap;
use tycho_simulation::tycho_common::{
    models::protocol::ProtocolComponent as CommonProtocolComponent, Bytes,
};

use super::model::ResimulatedRouteInternal;
use super::wire::format_address;
use super::EncodeError;

struct SplitStats {
    total_amount: BigUint,
    count: usize,
    last_index: usize,
}

pub(super) fn build_route_swaps(
    resimulated: &ResimulatedRouteInternal,
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
        let common_component: CommonProtocolComponent = swap.component.as_ref().clone().into();
        let swap_data = Swap::new(
            common_component,
            swap.token_in.clone(),
            swap.token_out.clone(),
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

pub(super) fn compute_split_fraction(
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::services::encode::model::{
        ResimulatedHopInternal, ResimulatedRouteInternal, ResimulatedSegmentInternal,
        ResimulatedSwapInternal,
    };
    use crate::services::encode::test_support::{dummy_component, pool_ref, MockProtocolSim};
    use tycho_simulation::tycho_common::simulation::protocol_sim::ProtocolSim;

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
    fn build_route_swaps_sets_remainder_split_last() {
        let resimulated = ResimulatedRouteInternal {
            segments: vec![ResimulatedSegmentInternal {
                share_bps: 10_000,
                amount_in: BigUint::from(100u32),
                expected_amount_out: BigUint::from(90u32),
                hops: vec![ResimulatedHopInternal {
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

        let swaps = build_route_swaps(&resimulated).unwrap();

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
                    share_bps: 6_000,
                    amount_in: BigUint::from(60u32),
                    expected_amount_out: BigUint::from(55u32),
                    hops: vec![
                        ResimulatedHopInternal {
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
                    share_bps: 4_000,
                    amount_in: BigUint::from(40u32),
                    expected_amount_out: BigUint::from(37u32),
                    hops: vec![
                        ResimulatedHopInternal {
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

        let swaps = build_route_swaps(&resimulated).unwrap();

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
                share_bps: 10_000,
                amount_in: BigUint::from(100u32),
                expected_amount_out: BigUint::from(90u32),
                hops: vec![
                    ResimulatedHopInternal {
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

        let err = build_route_swaps(&resimulated).expect_err("rejects repeated token depth");
        assert!(
            err.message().contains("multiple hop depths"),
            "unexpected error: {err:?}"
        );
    }
}
