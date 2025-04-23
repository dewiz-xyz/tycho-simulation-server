use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Clone)]
pub struct AmountOutRequest {
    pub request_id: Option<String>,
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

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", content = "data")]
pub enum UpdateMessage {
    BlockUpdate {
        block_number: u64,
        states_count: usize,
        new_pairs_count: usize,
        removed_pairs_count: usize,
    },
    QuoteUpdate {
        request_id: String,
        data: Vec<AmountOutResponse>,
    },
}
