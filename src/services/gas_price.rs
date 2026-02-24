use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: [(); 0],
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    result: Option<String>,
    error: Option<serde_json::Value>,
}

pub async fn fetch_eth_gas_price_wei(rpc_url: &str, client: &Client) -> Result<u128> {
    let request = JsonRpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "eth_gasPrice",
        params: [],
    };

    let response = client
        .post(rpc_url)
        .json(&request)
        .send()
        .await
        .with_context(|| format!("failed to call eth_gasPrice at {}", rpc_url))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("eth_gasPrice returned HTTP {}: {}", status, body));
    }

    let body: JsonRpcResponse = response
        .json()
        .await
        .context("failed to decode eth_gasPrice response")?;

    if let Some(error) = body.error {
        return Err(anyhow!("eth_gasPrice returned rpc error: {}", error));
    }

    let value = body
        .result
        .ok_or_else(|| anyhow!("eth_gasPrice response missing result field"))?;
    parse_hex_u128(&value)
}

fn parse_hex_u128(value: &str) -> Result<u128> {
    let raw = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| anyhow!("expected hex value with 0x prefix, got {}", value))?;

    if raw.is_empty() {
        return Err(anyhow!("empty hex value"));
    }

    u128::from_str_radix(raw, 16).with_context(|| format!("invalid hex u128 value: {}", value))
}

#[cfg(test)]
mod tests {
    use super::parse_hex_u128;

    #[test]
    fn parse_hex_u128_parses_valid_values() {
        assert_eq!(parse_hex_u128("0x1").unwrap(), 1);
        assert_eq!(parse_hex_u128("0x3b9aca00").unwrap(), 1_000_000_000);
    }

    #[test]
    fn parse_hex_u128_rejects_invalid_values() {
        assert!(parse_hex_u128("1").is_err());
        assert!(parse_hex_u128("0x").is_err());
        assert!(parse_hex_u128("0xzz").is_err());
    }
}
