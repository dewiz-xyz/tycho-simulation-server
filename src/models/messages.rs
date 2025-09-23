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

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    #[serde(rename = "QuoteAmountOut")]
    QuoteAmountOut(AmountOutRequest),
    #[serde(rename = "CancelAuctionRequests")]
    CancelAuctionRequests { auction_id: String },
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
        meta: QuoteMeta,
    },
    CancelAck {
        auction_id: String,
        canceled_count: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_parsing_with_and_without_auction_id() {
        let json_without_auction = r#"{
            "type": "QuoteAmountOut",
            "data": {
                "request_id": "req-1",
                "token_in": "0xabc",
                "token_out": "0xdef",
                "amounts": ["1", "2"]
            }
        }"#;
        let parsed: ClientMessage = serde_json::from_str(json_without_auction).unwrap();
        match parsed {
            ClientMessage::QuoteAmountOut(req) => {
                assert_eq!(req.request_id.as_str(), "req-1");
                assert!(req.auction_id.is_none());
            }
            _ => panic!("wrong variant"),
        }

        let json_with_auction = r#"{
            "type": "QuoteAmountOut",
            "data": {
                "request_id": "req-2",
                "auction_id": "auc-123",
                "token_in": "0x111",
                "token_out": "0x222",
                "amounts": ["10"]
            }
        }"#;
        let parsed2: ClientMessage = serde_json::from_str(json_with_auction).unwrap();
        match parsed2 {
            ClientMessage::QuoteAmountOut(req) => {
                assert_eq!(req.request_id.as_str(), "req-2");
                assert_eq!(req.auction_id.as_deref(), Some("auc-123"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_round_trip() {
        let req = AmountOutRequest {
            request_id: "r1".into(),
            auction_id: Some("a1".into()),
            token_in: "0x1".into(),
            token_out: "0x2".into(),
            amounts: vec!["42".into()],
        };
        let msg = ClientMessage::QuoteAmountOut(req.clone());
        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        match back {
            ClientMessage::QuoteAmountOut(req2) => {
                assert_eq!(req2.request_id, req.request_id);
                assert_eq!(req2.auction_id, req.auction_id);
                assert_eq!(req2.amounts, req.amounts);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn cancel_ack_serialization() {
        let ack = UpdateMessage::CancelAck {
            auction_id: "a1".into(),
            canceled_count: 2,
        };
        let json = serde_json::to_string(&ack).unwrap();
        assert!(json.contains("\"type\":\"CancelAck\""));
        assert!(json.contains("\"auction_id\":\"a1\""));
        assert!(json.contains("\"canceled_count\":2"));
    }
}
