use std::{collections::HashMap, str::FromStr};

use serde::Deserialize;
use tycho_simulation::tycho_common::{
    models::{token::Token, Chain},
    Bytes,
};

#[derive(Deserialize, Debug)]
pub struct BebopResponse {
    pub tokens: HashMap<String, TokenBebop>,
    pub metadata: Option<Metadata>, // optional if sometimes missing
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub last_update: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBebop {
    pub name: String,
    pub ticker: String,
    pub availability: Availability,
    pub price_usd: Option<f64>,
    pub cid: String,
    pub display_decimals: Option<u8>,
    pub colour: Option<String>,
    pub tags: Vec<String>,
    pub icon_url: String,
    pub chain_info: Vec<ChainInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Availability {
    pub is_available: bool,
    pub can_buy: bool,
    pub can_sell: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainInfo {
    pub chain_id: u64,
    pub contract_address: String,
    pub decimals: u8,
}

impl TokenBebop {
    pub fn to_tycho_token(&self) -> Option<Token> {
        for chain_info in &self.chain_info {
            if chain_info.chain_id == 1 {
                return Some(Token {
                    address: Bytes::from_str(chain_info.contract_address.clone().as_str())
                        .expect("valid contract address"),
                    symbol: self.ticker.clone(),
                    decimals: chain_info.decimals as u32,
                    tax: 0,
                    gas: vec![],
                    chain: Chain::Ethereum,
                    quality: Default::default(),
                });
            }
        }
        None
    }
}
