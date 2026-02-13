use crate::models::messages::{
    EncodeErrorResponse, QuoteFailure, QuoteFailureKind, QuoteMeta, QuoteResult,
    QuoteResultQuality, QuoteStatus,
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
        status: QuoteStatus::PartialSuccess,
        result_quality: QuoteResultQuality::RequestLevelFailure,
        block_number: 0,
        vm_block_number: None,
        matching_pools: 0,
        candidate_pools: 0,
        total_pools: None,
        auction_id: None,
        pool_results: Vec::new(),
        vm_unavailable: false,
        failures: vec![failure],
    };
    QuoteResult {
        request_id: String::new(),
        data: Vec::new(),
        meta,
    }
}

/// Constructs an encode router-level timeout response using sentinel values.
pub fn encode_router_timeout_result() -> EncodeErrorResponse {
    EncodeErrorResponse {
        error: "Encode request timed out".to_string(),
        request_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::router_timeout_result;
    use crate::models::messages::{QuoteResultQuality, QuoteStatus};

    #[test]
    fn router_timeout_result_sets_request_level_failure_quality() {
        let result = router_timeout_result();
        assert_eq!(result.request_id, "");
        assert!(result.data.is_empty());
        assert!(matches!(result.meta.status, QuoteStatus::PartialSuccess));
        assert_eq!(
            result.meta.result_quality,
            QuoteResultQuality::RequestLevelFailure
        );
        assert!(result.meta.pool_results.is_empty());
        assert!(!result.meta.vm_unavailable);
        assert_eq!(result.meta.failures.len(), 1);
    }
}
