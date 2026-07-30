use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use state_history::BaseFeeSource;
use tracing::warn;

const RPC_TIMEOUT: Duration = Duration::from_secs(2);
const CACHE_CAPACITY: usize = 4_096;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcBlock {
    base_fee_per_gas: Option<String>,
    number: Option<String>,
}

#[derive(Deserialize)]
struct RpcResponse {
    result: Option<RpcBlock>,
}

enum RpcTransportError {
    Http(String),
    Timeout(String),
    Decode(String),
}

trait BlockTransport: Send + Sync {
    fn get_block<'a>(
        &'a self,
        rpc_url: &'a str,
        block_number: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Option<RpcBlock>, RpcTransportError>> + Send + 'a>>;
}

struct ReqwestBlockTransport {
    client: Client,
}

impl BlockTransport for ReqwestBlockTransport {
    fn get_block<'a>(
        &'a self,
        rpc_url: &'a str,
        block_number: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Option<RpcBlock>, RpcTransportError>> + Send + 'a>>
    {
        Box::pin(async move {
            let response = self
                .client
                .post(rpc_url)
                .json(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "eth_getBlockByNumber",
                    "params": [format!("{block_number:#x}"), false],
                }))
                .timeout(RPC_TIMEOUT)
                .send()
                .await
                .map_err(classify_request_error)?
                .error_for_status()
                .map_err(|error| RpcTransportError::Http(error.to_string()))?;

            response
                .json::<RpcResponse>()
                .await
                .map(|response| response.result)
                .map_err(classify_decode_error)
        })
    }
}

fn classify_request_error(error: reqwest::Error) -> RpcTransportError {
    if error.is_timeout() {
        RpcTransportError::Timeout(error.to_string())
    } else {
        RpcTransportError::Http(error.to_string())
    }
}

fn classify_decode_error(error: reqwest::Error) -> RpcTransportError {
    if error.is_timeout() {
        RpcTransportError::Timeout(error.to_string())
    } else {
        RpcTransportError::Decode(error.to_string())
    }
}

/// Resolves historical EIP-1559 base fees through an Ethereum JSON-RPC endpoint.
pub struct RpcBaseFeeSource {
    rpc_url: String,
    transport: Arc<dyn BlockTransport>,
    cache: Mutex<BTreeMap<(u64, u64), String>>,
}

impl RpcBaseFeeSource {
    pub fn new(rpc_url: String) -> Self {
        Self {
            rpc_url,
            transport: Arc::new(ReqwestBlockTransport {
                client: Client::new(),
            }),
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    #[cfg(test)]
    fn with_transport(rpc_url: String, transport: Arc<dyn BlockTransport>) -> Self {
        Self {
            rpc_url,
            transport,
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    async fn resolve_base_fee(&self, chain_id: u64, block_number: u64) -> Option<String> {
        let cache_key = (chain_id, block_number);
        if let Some(base_fee) = self.cached_base_fee(cache_key) {
            return Some(base_fee);
        }

        let block = match self.transport.get_block(&self.rpc_url, block_number).await {
            Ok(Some(block)) => block,
            Ok(None) => {
                warn!(
                    scope = "state_history_base_fee",
                    chain_id, block_number, "Base fee RPC returned no block"
                );
                return None;
            }
            Err(RpcTransportError::Http(error)) => {
                warn!(
                    scope = "state_history_base_fee",
                    chain_id,
                    block_number,
                    %error,
                    "Base fee RPC HTTP request failed"
                );
                return None;
            }
            Err(RpcTransportError::Timeout(error)) => {
                warn!(
                    scope = "state_history_base_fee",
                    chain_id,
                    block_number,
                    %error,
                    "Base fee RPC request timed out"
                );
                return None;
            }
            Err(RpcTransportError::Decode(error)) => {
                warn!(
                    scope = "state_history_base_fee",
                    chain_id,
                    block_number,
                    %error,
                    "Base fee RPC response could not be decoded"
                );
                return None;
            }
        };

        let Some(echoed_number) = block.number.as_deref() else {
            warn!(
                scope = "state_history_base_fee",
                chain_id, block_number, "Base fee RPC block number echo was missing"
            );
            return None;
        };
        let Ok(echoed_number) = parse_hex_u64(echoed_number) else {
            warn!(
                scope = "state_history_base_fee",
                chain_id,
                block_number,
                echoed_number,
                "Base fee RPC block number was not valid hex"
            );
            return None;
        };
        if echoed_number != block_number {
            warn!(
                scope = "state_history_base_fee",
                chain_id,
                block_number,
                echoed_number,
                "Base fee RPC returned a mismatched block number"
            );
            return None;
        }

        let Some(base_fee_hex) = block.base_fee_per_gas.as_deref() else {
            warn!(
                scope = "state_history_base_fee",
                chain_id, block_number, "Base fee RPC block omitted baseFeePerGas"
            );
            return None;
        };
        let Ok(base_fee) = parse_hex_u128(base_fee_hex) else {
            warn!(
                scope = "state_history_base_fee",
                chain_id,
                block_number,
                base_fee_hex,
                "Base fee RPC baseFeePerGas was not valid hex"
            );
            return None;
        };

        let base_fee = base_fee.to_string();
        self.cache_base_fee(cache_key, base_fee.clone());
        Some(base_fee)
    }

    fn cached_base_fee(&self, key: (u64, u64)) -> Option<String> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned()
    }

    fn cache_base_fee(&self, key: (u64, u64), base_fee: String) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.insert(key, base_fee);
        if cache.len() > CACHE_CAPACITY {
            cache.pop_first();
        }
    }
}

impl BaseFeeSource for RpcBaseFeeSource {
    fn base_fee_wei(
        &self,
        chain_id: u64,
        block_number: u64,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        Box::pin(self.resolve_base_fee(chain_id, block_number))
    }
}

fn parse_hex_u64(value: &str) -> Result<u64, std::num::ParseIntError> {
    u64::from_str_radix(strip_hex_prefix(value), 16)
}

fn parse_hex_u128(value: &str) -> Result<u128, std::num::ParseIntError> {
    u128::from_str_radix(strip_hex_prefix(value), 16)
}

fn strip_hex_prefix(value: &str) -> &str {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use state_history::BaseFeeSource;

    use super::{BlockTransport, RpcBaseFeeSource, RpcBlock, RpcTransportError};

    struct CountingTransport {
        calls: AtomicUsize,
        response: Option<RpcBlock>,
    }

    impl CountingTransport {
        fn new(response: Option<RpcBlock>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                response,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl BlockTransport for CountingTransport {
        fn get_block<'a>(
            &'a self,
            _rpc_url: &'a str,
            _block_number: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Option<RpcBlock>, RpcTransportError>> + Send + 'a>>
        {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let response = self.response.clone();
            Box::pin(async move { Ok(response) })
        }
    }

    fn source_with_response(response: RpcBlock) -> (RpcBaseFeeSource, Arc<CountingTransport>) {
        let transport = Arc::new(CountingTransport::new(Some(response)));
        let source =
            RpcBaseFeeSource::with_transport("http://rpc.example".to_string(), transport.clone());
        (source, transport)
    }

    #[tokio::test]
    async fn parses_base_fee_hex_to_decimal_string() {
        let (source, _) = source_with_response(RpcBlock {
            base_fee_per_gas: Some("0x3b9aca00".to_string()),
            number: Some("0x64".to_string()),
        });

        assert_eq!(
            source.base_fee_wei(1, 100).await,
            Some("1000000000".to_string())
        );
    }

    #[tokio::test]
    async fn rejects_mismatched_block_number_echo() {
        let (source, _) = source_with_response(RpcBlock {
            base_fee_per_gas: Some("0x3b9aca00".to_string()),
            number: Some("0x65".to_string()),
        });

        assert_eq!(source.base_fee_wei(1, 100).await, None);
    }

    #[tokio::test]
    async fn missing_base_fee_yields_none() {
        let (source, _) = source_with_response(RpcBlock {
            base_fee_per_gas: None,
            number: Some("0x64".to_string()),
        });

        assert_eq!(source.base_fee_wei(1, 100).await, None);
    }

    #[tokio::test]
    async fn cache_hits_skip_lookup() {
        let (source, transport) = source_with_response(RpcBlock {
            base_fee_per_gas: Some("0x3b9aca00".to_string()),
            number: Some("0x64".to_string()),
        });

        assert_eq!(
            source.base_fee_wei(1, 100).await,
            Some("1000000000".to_string())
        );
        assert_eq!(
            source.base_fee_wei(1, 100).await,
            Some("1000000000".to_string())
        );
        assert_eq!(transport.calls(), 1);
    }
}
