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

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum CalldataMode {
    #[default]
    TychoRouterTransferFrom,
    TychoRouterNone,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct CalldataConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<CalldataMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RouteHop {
    pub pool_id: String,
    pub token_in: String,
    pub token_out: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EncodeRoute {
    pub route_id: String,
    pub hops: Vec<RouteHop>,
    pub amounts: Vec<String>,
    pub amounts_out: Vec<String>,
    pub gas_used: Vec<u64>,
    pub block_number: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EncodeRequest {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auction_id: Option<String>,
    pub token_in: String,
    pub token_out: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calldata: Option<CalldataConfig>,
    pub routes: Vec<EncodeRoute>,
}

#[derive(Debug, Serialize, Clone)]
pub struct EncodeMeta {
    pub status: QuoteStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auction_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<QuoteFailure>,
}

#[derive(Debug, Serialize, Clone)]
pub struct EncodeRouteResult {
    pub route_id: String,
    pub amounts_out: Vec<String>,
    pub gas_used: Vec<u64>,
    pub block_number: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calldata: Option<CalldataResult>,
}

#[derive(Debug, Serialize, Clone)]
pub struct EncodeResult {
    pub request_id: String,
    pub data: Vec<EncodeRouteResult>,
    pub meta: EncodeMeta,
}

#[derive(Debug, Serialize, Clone)]
pub struct CalldataResult {
    pub mode: CalldataMode,
    pub sender: String,
    pub receiver: String,
    pub steps: Vec<CalldataStep>,
}

#[derive(Debug, Serialize, Clone)]
pub struct CalldataStep {
    pub calls: Vec<ExecutionCall>,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionCallType {
    Erc20Transfer,
    TychoRouter,
}

#[derive(Debug, Serialize, Clone)]
pub struct ExecutionCall {
    pub call_type: ExecutionCallType,
    pub target: String,
    pub value: String,
    pub calldata: String,
    pub allowances: Vec<Allowance>,
    pub inputs: Vec<Asset>,
    pub outputs: Vec<Asset>,
}

#[derive(Debug, Serialize, Clone)]
pub struct Asset {
    pub token: String,
    pub amount: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct Allowance {
    pub token: String,
    pub spender: String,
    pub amount: String,
}
