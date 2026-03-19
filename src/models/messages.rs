use serde::{Deserialize, Serialize, Serializer};

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if predicates receive borrowed values"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AmountOutRequest {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auction_id: Option<String>,
    pub token_in: String,
    pub token_out: String,
    pub amounts: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct AmountOutResponse {
    pub pool: String,
    pub pool_name: String,
    pub pool_address: String,
    pub amounts_out: Vec<String>,
    pub gas_used: Vec<u64>,
    pub gas_in_sell: String,
    pub block_number: u64,
    /// Per-ladder-step slippage in basis points (1 bps = 0.01%).
    /// Positive = price impact against the trader; negative = favourable fill.
    /// `None` for a step when the pool's spot price was unavailable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slippage_bps: Vec<Option<i32>>,
    /// Fraction of the pool's reported `max_in` consumed by the largest requested
    /// amount, expressed in basis points (10_000 = 100 %).  `None` when the pool
    /// did not return valid limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_utilization_bps: Option<u32>,
    /// Composite execution-risk assessment derived from slippage, utilization,
    /// curve convexity, and ladder completeness.  `None` when insufficient data
    /// is available (e.g. all slippage entries are null and utilization unknown).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_risk: Option<ExecutionRisk>,
}

/// Qualitative risk level derived from the composite `risk_score`.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// `risk_score < 200` — deep pool, minimal sensitivity to concurrent activity.
    Low,
    /// `risk_score 200–499` — moderate sensitivity; suitable for most orders.
    Medium,
    /// `risk_score 500–999` — pool state sensitive to competing solver routing.
    High,
    /// `risk_score >= 1000` — prefer splitting across multiple pools.
    VeryHigh,
    /// Insufficient signal to compute a score (all slippage null, no utilization).
    Unknown,
}

/// Composite execution-risk score and qualitative label for a single pool quote.
///
/// The score is computed server-side from signals already present in the response:
/// slippage (price sensitivity), utilization (capacity pressure), curve convexity,
/// and partial-ladder detection.  Block-lag staleness is **not** included here —
/// clients should add that penalty using `block_number` vs. current chain head.
#[derive(Debug, Serialize, Clone)]
pub struct ExecutionRisk {
    /// Weighted composite score.  Lower is safer.
    /// `< 200` → low; `200–499` → medium; `500–999` → high; `>= 1000` → very high.
    pub risk_score: u32,
    /// Qualitative label derived from `risk_score`.
    pub risk_level: RiskLevel,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum QuoteStatus {
    Ready,
    WarmingUp,
    TokenMissing,
    NoLiquidity,
    InvalidRequest,
    InternalError,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuoteResultQuality {
    Complete,
    Partial,
    NoResults,
    RequestLevelFailure,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuotePartialKind {
    AmountLadders,
    PoolCoverage,
    Mixed,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PoolOutcomeKind {
    PartialOutput,
    ZeroOutput,
    SkippedConcurrency,
    SkippedDeadline,
    SkippedPrecheck,
    TimedOut,
    SimulatorError,
    InternalError,
}

#[derive(Debug, Clone, Copy)]
pub enum QuoteFailureKind {
    WarmUp,
    TokenValidation,
    TokenCoverage,
    Timeout,
    ConcurrencyLimit,
    Overflow,
    Simulator,
    NoPools,
    InconsistentResult,
    Internal,
    InvalidRequest,
}

impl QuoteFailureKind {
    pub const fn label(self) -> &'static str {
        match self {
            QuoteFailureKind::WarmUp => "warm_up",
            QuoteFailureKind::TokenValidation => "token_validation",
            QuoteFailureKind::TokenCoverage => "token_coverage",
            QuoteFailureKind::Timeout => "timeout",
            QuoteFailureKind::ConcurrencyLimit => "concurrency_limit",
            QuoteFailureKind::Overflow => "overflow",
            QuoteFailureKind::Simulator => "simulator",
            QuoteFailureKind::NoPools => "no_pools",
            QuoteFailureKind::InconsistentResult => "inconsistent_result",
            QuoteFailureKind::Internal => "internal",
            QuoteFailureKind::InvalidRequest => "invalid_request",
        }
    }
}

impl Serialize for QuoteFailureKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.label())
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct QuoteFailure {
    pub kind: QuoteFailureKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PoolSimulationOutcome {
    pub pool: String,
    pub pool_name: String,
    pub pool_address: String,
    pub protocol: String,
    pub outcome: PoolOutcomeKind,
    pub reported_steps: usize,
    pub expected_steps: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct QuoteMeta {
    pub status: QuoteStatus,
    pub result_quality: QuoteResultQuality,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_kind: Option<QuotePartialKind>,
    pub block_number: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vm_block_number: Option<u64>,
    pub matching_pools: usize,
    pub candidate_pools: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_pools: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auction_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pool_results: Vec<PoolSimulationOutcome>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub vm_unavailable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<QuoteFailure>,
}

#[derive(Debug, Serialize, Clone)]
pub struct QuoteResult {
    pub request_id: String,
    pub data: Vec<AmountOutResponse>,
    pub meta: QuoteMeta,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PoolRef {
    pub protocol: String,
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_address: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PoolSwapDraft {
    pub pool: PoolRef,
    pub token_in: String,
    pub token_out: String,
    pub split_bps: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HopDraft {
    pub token_in: String,
    pub token_out: String,
    pub swaps: Vec<PoolSwapDraft>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SegmentDraft {
    pub kind: SwapKind,
    pub share_bps: u32,
    pub hops: Vec<HopDraft>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum SwapKind {
    SimpleSwap,
    MultiSwap,
    MegaSwap,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteEncodeRequest {
    pub chain_id: u64,
    pub token_in: String,
    pub token_out: String,
    pub amount_in: String,
    pub min_amount_out: String,
    pub settlement_address: String,
    pub tycho_router_address: String,
    pub swap_kind: SwapKind,
    pub segments: Vec<SegmentDraft>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum InteractionKind {
    #[serde(rename = "ERC20_APPROVE")]
    Erc20Approve,
    #[serde(rename = "CALL")]
    Call,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Interaction {
    pub kind: InteractionKind,
    pub target: String,
    pub value: String,
    pub calldata: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ResimulationDebug {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tycho_state_tag: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RouteDebug {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resimulation: Option<ResimulationDebug>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RouteEncodeResponse {
    pub interactions: Vec<Interaction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<RouteDebug>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EncodeErrorResponse {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        AmountOutResponse, ExecutionRisk, PoolOutcomeKind, QuoteMeta, QuotePartialKind, QuoteResultQuality, RiskLevel,
        QuoteStatus,
    };
    use anyhow::Result;

    #[test]
    fn quote_result_quality_serializes_as_snake_case() -> Result<()> {
        assert_eq!(
            serde_json::to_string(&QuoteResultQuality::RequestLevelFailure)?,
            "\"request_level_failure\""
        );
        assert_eq!(
            serde_json::to_string(&QuoteResultQuality::NoResults)?,
            "\"no_results\""
        );
        Ok(())
    }

    #[test]
    fn quote_status_serializes_without_partial_success() -> Result<()> {
        assert_eq!(serde_json::to_string(&QuoteStatus::Ready)?, "\"ready\"");
        assert_eq!(
            serde_json::to_string(&QuoteStatus::InternalError)?,
            "\"internal_error\""
        );
        Ok(())
    }

    #[test]
    fn quote_partial_kind_serializes_as_snake_case() -> Result<()> {
        assert_eq!(
            serde_json::to_string(&QuotePartialKind::AmountLadders)?,
            "\"amount_ladders\""
        );
        assert_eq!(
            serde_json::to_string(&QuotePartialKind::PoolCoverage)?,
            "\"pool_coverage\""
        );
        assert_eq!(
            serde_json::to_string(&QuotePartialKind::Mixed)?,
            "\"mixed\""
        );
        Ok(())
    }

    #[test]
    fn pool_outcome_kind_serializes_as_snake_case() -> Result<()> {
        assert_eq!(
            serde_json::to_string(&PoolOutcomeKind::PartialOutput)?,
            "\"partial_output\""
        );
        Ok(())
    }

    #[test]
    fn quote_meta_omits_partial_kind_when_absent() -> Result<()> {
        let meta = QuoteMeta {
            status: QuoteStatus::Ready,
            result_quality: QuoteResultQuality::Complete,
            partial_kind: None,
            block_number: 1,
            vm_block_number: None,
            matching_pools: 0,
            candidate_pools: 0,
            total_pools: None,
            auction_id: None,
            pool_results: Vec::new(),
            vm_unavailable: false,
            failures: Vec::new(),
        };
        let value = serde_json::to_value(meta)?;
        assert!(value.get("partial_kind").is_none());
        Ok(())
    }

    #[test]
    fn amount_out_response_serializes_gas_in_sell_snake_case() -> Result<()> {
        let response = AmountOutResponse {
            pool: "pool-1".to_string(),
            pool_name: "name".to_string(),
            pool_address: "0x1".to_string(),
            amounts_out: vec!["1".to_string()],
            gas_used: vec![1],
            gas_in_sell: "3000000".to_string(),
            block_number: 1,
            slippage_bps: Vec::new(),
            pool_utilization_bps: None,
            execution_risk: None,
        };

        let value = serde_json::to_value(response)?;
        assert_eq!(value["gas_in_sell"], "3000000");
        assert!(value.get("gasInSellToken").is_none());
        Ok(())
    }

    #[test]
    fn risk_level_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&RiskLevel::Low).unwrap(),
            "\"low\""
        );
        assert_eq!(
            serde_json::to_string(&RiskLevel::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(
            serde_json::to_string(&RiskLevel::High).unwrap(),
            "\"high\""
        );
        assert_eq!(
            serde_json::to_string(&RiskLevel::VeryHigh).unwrap(),
            "\"very_high\""
        );
        assert_eq!(
            serde_json::to_string(&RiskLevel::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn execution_risk_serializes_with_expected_field_names() {
        let risk = ExecutionRisk {
            risk_score: 87,
            risk_level: RiskLevel::Low,
        };
        let value = serde_json::to_value(&risk).expect("serialize ExecutionRisk");
        assert_eq!(value["risk_score"], 87);
        assert_eq!(value["risk_level"], "low");
    }

    #[test]
    fn amount_out_response_omits_execution_risk_when_none() {
        let response = AmountOutResponse {
            pool: "pool-1".to_string(),
            pool_name: "name".to_string(),
            pool_address: "0x1".to_string(),
            amounts_out: vec!["1".to_string()],
            gas_used: vec![1],
            gas_in_sell: "0".to_string(),
            block_number: 1,
            slippage_bps: Vec::new(),
            pool_utilization_bps: None,
            execution_risk: None,
        };
        let value = serde_json::to_value(response).expect("serialize response");
        assert!(
            value.get("execution_risk").is_none(),
            "execution_risk should be omitted when None"
        );
    }

    #[test]
    fn amount_out_response_includes_execution_risk_when_present() {
        let response = AmountOutResponse {
            pool: "pool-1".to_string(),
            pool_name: "name".to_string(),
            pool_address: "0x1".to_string(),
            amounts_out: vec!["1".to_string()],
            gas_used: vec![1],
            gas_in_sell: "0".to_string(),
            block_number: 1,
            slippage_bps: Vec::new(),
            pool_utilization_bps: None,
            execution_risk: Some(ExecutionRisk {
                risk_score: 221,
                risk_level: RiskLevel::Medium,
            }),
        };
        let value = serde_json::to_value(response).expect("serialize response");
        let er = value.get("execution_risk").expect("execution_risk present");
        assert_eq!(er["risk_score"], 221);
        assert_eq!(er["risk_level"], "medium");
    }
}
