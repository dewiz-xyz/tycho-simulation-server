use crate::models::messages::{
    QuoteFailure, QuoteFailureKind, QuoteMeta, QuoteResult, QuoteStatus,
};

/// Constructs a router-level timeout response using sentinel values.
/// - request_id: empty
/// - block_number: 0
/// - total_pools: None
pub fn router_timeout_result() -> QuoteResult {
    let failure = QuoteFailure {
        kind: QuoteFailureKind::Timeout,
        message: "Simulate request timed out".to_string(),
        pool: None,
        pool_name: None,
        pool_address: None,
        protocol: None,
    };
    let meta = QuoteMeta {
        status: QuoteStatus::PartialFailure,
        block_number: 0,
        matching_pools: 0,
        candidate_pools: 0,
        total_pools: None,
        auction_id: None,
        failures: vec![failure],
    };
    QuoteResult {
        request_id: String::new(),
        data: Vec::new(),
        meta,
    }
}
