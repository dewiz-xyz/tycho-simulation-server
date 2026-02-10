use num_bigint::BigUint;
use num_traits::Zero;
use tycho_simulation::tycho_common::Bytes;

use crate::models::messages::PoolRef;

use super::model::NormalizedSwapDraftInternal;
use super::EncodeError;

pub(super) const BPS_DENOMINATOR: u32 = 10_000;

fn mul_bps(amount: &BigUint, bps: u32) -> BigUint {
    if bps == 0 {
        return BigUint::zero();
    }
    (amount * BigUint::from(bps)) / BigUint::from(BPS_DENOMINATOR)
}

pub(super) fn allocate_amounts_by_bps(
    total: &BigUint,
    shares: &[u32],
    label: &str,
    field_name: &str,
) -> Result<Vec<BigUint>, EncodeError> {
    if shares.is_empty() {
        return Err(EncodeError::invalid(format!(
            "{} {} must not be empty",
            label, field_name
        )));
    }

    let last_index = shares.len() - 1;
    let mut sum_bps: u32 = 0;
    let mut allocated = BigUint::zero();
    let mut amounts = Vec::with_capacity(shares.len());

    for (index, share) in shares.iter().enumerate() {
        if *share > BPS_DENOMINATOR {
            return Err(EncodeError::invalid(format!(
                "{} {} must be <= 10000",
                label, field_name
            )));
        }

        if index < last_index {
            if *share == 0 {
                return Err(EncodeError::invalid(format!(
                    "{} {} remainder must be last",
                    label, field_name
                )));
            }
            sum_bps = sum_bps.saturating_add(*share);
            if sum_bps > BPS_DENOMINATOR {
                return Err(EncodeError::invalid(format!(
                    "{} {} sum exceeds 10000",
                    label, field_name
                )));
            }
            let amount = mul_bps(total, *share);
            if last_index > 0 && amount.is_zero() {
                return Err(EncodeError::invalid(format!(
                    "{} {} rounds down to zero for this amountIn; increase amountIn or adjust {}",
                    label, field_name, field_name
                )));
            }
            allocated += amount.clone();
            amounts.push(amount);
        } else {
            if *share != 0 {
                return Err(EncodeError::invalid(format!(
                    "{} last {} must be 0 to take remainder",
                    label, field_name
                )));
            }
            let amount = total - &allocated;
            if last_index > 0 && amount.is_zero() {
                return Err(EncodeError::invalid(format!(
                    "{} {} must leave a remainder for the last entry",
                    label, field_name
                )));
            }
            amounts.push(amount);
        }
    }

    Ok(amounts)
}

#[derive(Debug)]
pub(super) struct AllocatedSwap {
    pub(super) pool: PoolRef,
    pub(super) token_in: Bytes,
    pub(super) token_out: Bytes,
    pub(super) split_bps: u32,
    pub(super) amount_in: BigUint,
}

pub(super) fn allocate_swaps_by_bps(
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
        "splitBps",
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::services::encode::test_support::pool_ref;

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
        assert_eq!(
            err.kind(),
            crate::services::encode::EncodeErrorKind::InvalidRequest
        );
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
        assert_eq!(
            err.kind(),
            crate::services::encode::EncodeErrorKind::InvalidRequest
        );
    }

    #[test]
    fn allocate_swaps_by_bps_rejects_zero_remainder_amount() {
        let hop_amount = BigUint::from(100u32);
        let swaps = vec![
            NormalizedSwapDraftInternal {
                pool: pool_ref("p1"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 5000,
            },
            NormalizedSwapDraftInternal {
                pool: pool_ref("p2"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 5000,
            },
            NormalizedSwapDraftInternal {
                pool: pool_ref("p3"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 0,
            },
        ];

        let err = allocate_swaps_by_bps(hop_amount, &swaps, 0, 0)
            .expect_err("remainder must be positive for multi-swap splits");
        assert_eq!(
            err.kind(),
            crate::services::encode::EncodeErrorKind::InvalidRequest
        );
        assert!(
            err.message().contains("leave a remainder"),
            "unexpected error: {}",
            err.message()
        );
    }

    #[test]
    fn allocate_swaps_by_bps_allows_sum_10000_with_positive_remainder() {
        let hop_amount = BigUint::from(101u32);
        let swaps = vec![
            NormalizedSwapDraftInternal {
                pool: pool_ref("p1"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 5000,
            },
            NormalizedSwapDraftInternal {
                pool: pool_ref("p2"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 5000,
            },
            NormalizedSwapDraftInternal {
                pool: pool_ref("p3"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 0,
            },
        ];

        let allocated = allocate_swaps_by_bps(hop_amount, &swaps, 0, 0).expect("allocates");
        assert_eq!(allocated.len(), 3);
        assert_eq!(allocated[0].amount_in, BigUint::from(50u32));
        assert_eq!(allocated[1].amount_in, BigUint::from(50u32));
        assert_eq!(allocated[2].amount_in, BigUint::from(1u32));
    }

    #[test]
    fn allocate_swaps_by_bps_rejects_non_remainder_rounding_to_zero() {
        let hop_amount = BigUint::from(1u32);
        let swaps = vec![
            NormalizedSwapDraftInternal {
                pool: pool_ref("p1"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 1,
            },
            NormalizedSwapDraftInternal {
                pool: pool_ref("p2"),
                token_in: Bytes::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                token_out: Bytes::from_str("0x0000000000000000000000000000000000000002").unwrap(),
                split_bps: 0,
            },
        ];

        let err =
            allocate_swaps_by_bps(hop_amount, &swaps, 0, 0).expect_err("rounding to zero invalid");
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
}
