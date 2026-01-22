use serde::{Deserialize, Serialize};

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
    pub block_number: u64,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum QuoteStatus {
    Ready,
    WarmingUp,
    TokenMissing,
    NoLiquidity,
    PartialFailure,
    InvalidRequest,
    InternalError,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
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
pub struct QuoteMeta {
    pub status: QuoteStatus,
    pub block_number: u64,
    pub matching_pools: usize,
    pub candidate_pools: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_pools: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auction_id: Option<String>,
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
#[serde(rename_all = "camelCase")]
pub struct PoolSwapDraft {
    pub pool: PoolRef,
    pub token_in: String,
    pub token_out: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split_bps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_in: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_out: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HopDraft {
    pub token_in: String,
    pub token_out: String,
    pub swaps: Vec<PoolSwapDraft>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SegmentDraft {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_bps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_remainder: Option<bool>,
    pub hops: Vec<HopDraft>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RouteEncodeRequest {
    pub chain_id: u64,
    pub token_in: String,
    pub token_out: String,
    pub amount_in: String,
    pub settlement_address: String,
    pub tycho_router_address: String,
    pub slippage_bps: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_pool_slippage_bps: Option<std::collections::HashMap<String, u32>>,
    pub segments: Vec<SegmentDraft>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_tenderly_sim: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub token: String,
    pub spender: String,
    pub amount: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_to_zero: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum ExecutionCallKind {
    #[serde(rename = "TYCHO_SINGLE_SWAP")]
    TychoSingleSwap,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionCall {
    pub index: u32,
    pub target: String,
    pub value: String,
    pub calldata: String,
    pub approvals: Vec<Approval>,
    pub kind: ExecutionCallKind,
    pub hop_path: String,
    pub pool: PoolRef,
    pub token_in: String,
    pub token_out: String,
    pub amount_in: String,
    pub min_amount_out: String,
    pub expected_amount_out: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PoolSwap {
    pub pool: PoolRef,
    pub token_in: String,
    pub token_out: String,
    pub split_bps: u32,
    pub amount_in: String,
    pub expected_amount_out: String,
    pub min_amount_out: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Hop {
    pub token_in: String,
    pub token_out: String,
    pub amount_in: String,
    pub expected_amount_out: String,
    pub min_amount_out: String,
    pub swaps: Vec<PoolSwap>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub share_bps: u32,
    pub amount_in: String,
    pub expected_amount_out: String,
    pub min_amount_out: String,
    pub hops: Vec<Hop>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RouteTotals {
    pub expected_amount_out: String,
    pub min_amount_out: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TenderlyDebug {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simulation_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simulation_id: Option<String>,
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
    pub tenderly: Option<TenderlyDebug>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resimulation: Option<ResimulationDebug>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedRoute {
    pub segments: Vec<Segment>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RouteEncodeResponse {
    pub schema_version: String,
    pub chain_id: u64,
    pub token_in: String,
    pub token_out: String,
    pub amount_in: String,
    pub normalized_route: NormalizedRoute,
    pub calls: Vec<ExecutionCall>,
    pub totals: RouteTotals,
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
