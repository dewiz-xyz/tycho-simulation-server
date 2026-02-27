use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use tycho_simulation::tycho_common::{models::Chain, Bytes};

mod logging;
mod memory;
pub use logging::init_logging;
pub use memory::MemoryConfig;

/// Per-chain runtime profile resolved from `CHAIN_ID`.
#[derive(Clone, Debug)]
pub struct ChainProfile {
    pub chain: Chain,
    pub native_protocols: Vec<String>,
    pub vm_protocols: Vec<String>,
    /// Protocols allowed to swap with the native token (e.g. rocketpool on Ethereum).
    pub native_token_protocol_allowlist: Vec<String>,
    pub reset_allowance_tokens: HashMap<u64, HashSet<Bytes>>,
}

fn ethereum_profile() -> ChainProfile {
    let mut reset_tokens = HashMap::new();
    let mut mainnet = HashSet::new();
    mainnet.insert(parse_address(ETHEREUM_USDT));
    reset_tokens.insert(1, mainnet);

    ChainProfile {
        chain: Chain::Ethereum,
        native_protocols: vec![
            "uniswap_v2".into(),
            "sushiswap_v2".into(),
            "pancakeswap_v2".into(),
            "uniswap_v3".into(),
            "pancakeswap_v3".into(),
            "uniswap_v4".into(),
            "ekubo_v2".into(),
            "fluid_v1".into(),
            "ekubo_v3".into(),
        ],
        vm_protocols: vec![
            "vm:curve".into(),
            "vm:balancer_v2".into(),
            "vm:maverick_v2".into(),
        ],
        native_token_protocol_allowlist: vec!["rocketpool".into()],
        reset_allowance_tokens: reset_tokens,
    }
}

fn base_profile() -> ChainProfile {
    ChainProfile {
        chain: Chain::Base,
        native_protocols: vec![
            "uniswap_v2".into(),
            "uniswap_v3".into(),
            "uniswap_v4".into(),
            "pancakeswap_v3".into(),
        ],
        vm_protocols: vec![],
        native_token_protocol_allowlist: vec![],
        reset_allowance_tokens: HashMap::new(),
    }
}

fn resolve_chain_profile(chain_id: u64) -> ChainProfile {
    match chain_id {
        1 => ethereum_profile(),
        8453 => base_profile(),
        other => panic!(
            "Unsupported CHAIN_ID={}: supported values are 1 (Ethereum), 8453 (Base)",
            other
        ),
    }
}

pub fn load_config() -> AppConfig {
    dotenv::dotenv().ok();

    let chain_id: u64 = std::env::var("CHAIN_ID")
        .expect("CHAIN_ID must be set (supported: 1 for Ethereum, 8453 for Base)")
        .parse()
        .expect("CHAIN_ID must be a valid u64");
    let chain_profile = resolve_chain_profile(chain_id);

    let tycho_url =
        std::env::var("TYCHO_URL").unwrap_or_else(|_| "tycho-beta.propellerheads.xyz".to_string());
    let api_key = std::env::var("TYCHO_API_KEY").expect("TYCHO_API_KEY must be set");
    let rpc_url = std::env::var("RPC_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let gas_price_refresh_interval_ms: u64 = std::env::var("GAS_PRICE_REFRESH_INTERVAL_MS")
        .unwrap_or_else(|_| "5000".to_string())
        .parse()
        .expect("Invalid GAS_PRICE_REFRESH_INTERVAL_MS");
    let gas_price_failure_tolerance: u64 = std::env::var("GAS_PRICE_FAILURE_TOLERANCE")
        .unwrap_or_else(|_| "50".to_string())
        .parse()
        .expect("Invalid GAS_PRICE_FAILURE_TOLERANCE");
    let tvl_threshold: f64 = std::env::var("TVL_THRESHOLD")
        .unwrap_or_else(|_| "100".to_string())
        .parse()
        .expect("Invalid TVL_THRESHOLD");
    // Keep/remove ratio: defaults to 20% of add threshold; clamp to <= add
    let tvl_keep_ratio: f64 = std::env::var("TVL_KEEP_RATIO")
        .unwrap_or_else(|_| "0.2".to_string())
        .parse()
        .expect("Invalid TVL_KEEP_RATIO");
    let tvl_keep_threshold: f64 = (tvl_threshold * tvl_keep_ratio).min(tvl_threshold);
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "3000".to_string())
        .parse()
        .expect("Invalid PORT");
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());

    // Create socket address from host and port
    let addr = host.to_string();
    // Parse the address
    let _ = addr.parse::<std::net::IpAddr>().expect("Invalid HOST");

    // Basic validation
    assert!(tvl_threshold > 0.0, "TVL_THRESHOLD must be > 0");
    assert!(
        tvl_keep_ratio > 0.0 && tvl_keep_ratio <= 1.0,
        "TVL_KEEP_RATIO must be in (0, 1]"
    );
    assert!(
        tvl_keep_threshold > 0.0,
        "Derived TVL keep threshold must be > 0"
    );

    let quote_timeout_ms: u64 = std::env::var("QUOTE_TIMEOUT_MS")
        .unwrap_or_else(|_| "150".to_string())
        .parse()
        .expect("Invalid QUOTE_TIMEOUT_MS");
    let pool_timeout_native_ms: u64 = std::env::var("POOL_TIMEOUT_NATIVE_MS")
        .unwrap_or_else(|_| "20".to_string())
        .parse()
        .expect("Invalid POOL_TIMEOUT_NATIVE_MS");
    let pool_timeout_vm_ms: u64 = std::env::var("POOL_TIMEOUT_VM_MS")
        .unwrap_or_else(|_| "150".to_string())
        .parse()
        .expect("Invalid POOL_TIMEOUT_VM_MS");

    let request_timeout_ms: u64 = std::env::var("REQUEST_TIMEOUT_MS")
        .unwrap_or_else(|_| "4000".to_string())
        .parse()
        .expect("Invalid REQUEST_TIMEOUT_MS");
    assert!(quote_timeout_ms > 0, "QUOTE_TIMEOUT_MS must be > 0");
    assert!(
        pool_timeout_native_ms > 0,
        "POOL_TIMEOUT_NATIVE_MS must be > 0"
    );
    assert!(pool_timeout_vm_ms > 0, "POOL_TIMEOUT_VM_MS must be > 0");
    let token_refresh_timeout_ms: u64 = std::env::var("TOKEN_REFRESH_TIMEOUT_MS")
        .unwrap_or_else(|_| "1000".to_string())
        .parse()
        .expect("Invalid TOKEN_REFRESH_TIMEOUT_MS");
    assert!(
        token_refresh_timeout_ms > 0,
        "TOKEN_REFRESH_TIMEOUT_MS must be > 0"
    );
    assert!(
        gas_price_refresh_interval_ms > 0,
        "GAS_PRICE_REFRESH_INTERVAL_MS must be > 0"
    );

    let enable_vm_pools: bool = std::env::var("ENABLE_VM_POOLS")
        .unwrap_or_else(|_| "true".to_string())
        .parse()
        .expect("Invalid ENABLE_VM_POOLS");

    let reset_allowance_tokens = Arc::new(chain_profile.reset_allowance_tokens.clone());

    let cpu_count = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);

    let default_native = (cpu_count.saturating_mul(4)).max(1);
    let default_vm = cpu_count.max(1);

    let global_native_sim_concurrency: usize = std::env::var("GLOBAL_NATIVE_SIM_CONCURRENCY")
        .ok()
        .map(|value| {
            value
                .parse()
                .expect("Invalid GLOBAL_NATIVE_SIM_CONCURRENCY")
        })
        .unwrap_or(default_native)
        .max(1);
    let global_vm_sim_concurrency: usize = std::env::var("GLOBAL_VM_SIM_CONCURRENCY")
        .ok()
        .map(|value| value.parse().expect("Invalid GLOBAL_VM_SIM_CONCURRENCY"))
        .unwrap_or(default_vm)
        .max(1);

    let stream_stale_secs: u64 = std::env::var("STREAM_STALE_SECS")
        .unwrap_or_else(|_| "120".to_string())
        .parse()
        .expect("Invalid STREAM_STALE_SECS");
    let stream_missing_block_burst: u64 = std::env::var("STREAM_MISSING_BLOCK_BURST")
        .unwrap_or_else(|_| "3".to_string())
        .parse()
        .expect("Invalid STREAM_MISSING_BLOCK_BURST");
    let stream_missing_block_window_secs: u64 = std::env::var("STREAM_MISSING_BLOCK_WINDOW_SECS")
        .unwrap_or_else(|_| "60".to_string())
        .parse()
        .expect("Invalid STREAM_MISSING_BLOCK_WINDOW_SECS");
    let stream_error_burst: u64 = std::env::var("STREAM_ERROR_BURST")
        .unwrap_or_else(|_| "3".to_string())
        .parse()
        .expect("Invalid STREAM_ERROR_BURST");
    let stream_error_window_secs: u64 = std::env::var("STREAM_ERROR_WINDOW_SECS")
        .unwrap_or_else(|_| "60".to_string())
        .parse()
        .expect("Invalid STREAM_ERROR_WINDOW_SECS");
    let resync_grace_secs: u64 = std::env::var("RESYNC_GRACE_SECS")
        .unwrap_or_else(|_| "60".to_string())
        .parse()
        .expect("Invalid RESYNC_GRACE_SECS");
    let stream_restart_backoff_min_ms: u64 = std::env::var("STREAM_RESTART_BACKOFF_MIN_MS")
        .unwrap_or_else(|_| "500".to_string())
        .parse()
        .expect("Invalid STREAM_RESTART_BACKOFF_MIN_MS");
    let stream_restart_backoff_max_ms: u64 = std::env::var("STREAM_RESTART_BACKOFF_MAX_MS")
        .unwrap_or_else(|_| "30000".to_string())
        .parse()
        .expect("Invalid STREAM_RESTART_BACKOFF_MAX_MS");
    let stream_restart_backoff_jitter_pct: f64 = std::env::var("STREAM_RESTART_BACKOFF_JITTER_PCT")
        .unwrap_or_else(|_| "0.2".to_string())
        .parse()
        .expect("Invalid STREAM_RESTART_BACKOFF_JITTER_PCT");
    let readiness_stale_secs: u64 = std::env::var("READINESS_STALE_SECS")
        .unwrap_or_else(|_| "120".to_string())
        .parse()
        .expect("Invalid READINESS_STALE_SECS");

    assert!(stream_stale_secs > 0, "STREAM_STALE_SECS must be > 0");
    assert!(
        stream_missing_block_burst > 0,
        "STREAM_MISSING_BLOCK_BURST must be > 0"
    );
    assert!(
        stream_missing_block_window_secs > 0,
        "STREAM_MISSING_BLOCK_WINDOW_SECS must be > 0"
    );
    assert!(stream_error_burst > 0, "STREAM_ERROR_BURST must be > 0");
    assert!(
        stream_error_window_secs > 0,
        "STREAM_ERROR_WINDOW_SECS must be > 0"
    );
    assert!(resync_grace_secs > 0, "RESYNC_GRACE_SECS must be > 0");
    assert!(
        stream_restart_backoff_min_ms > 0,
        "STREAM_RESTART_BACKOFF_MIN_MS must be > 0"
    );
    assert!(
        stream_restart_backoff_max_ms > 0,
        "STREAM_RESTART_BACKOFF_MAX_MS must be > 0"
    );
    assert!(
        stream_restart_backoff_min_ms <= stream_restart_backoff_max_ms,
        "STREAM_RESTART_BACKOFF_MIN_MS must be <= STREAM_RESTART_BACKOFF_MAX_MS"
    );
    assert!(
        (0.0..=1.0).contains(&stream_restart_backoff_jitter_pct),
        "STREAM_RESTART_BACKOFF_JITTER_PCT must be within [0.0, 1.0]"
    );
    assert!(readiness_stale_secs > 0, "READINESS_STALE_SECS must be > 0");

    let memory = MemoryConfig::from_env();

    AppConfig {
        chain_profile,
        tycho_url,
        api_key,
        rpc_url,
        gas_price_refresh_interval_ms,
        gas_price_failure_tolerance,
        tvl_threshold,
        tvl_keep_threshold,
        port,
        host,
        quote_timeout_ms,
        pool_timeout_native_ms,
        pool_timeout_vm_ms,
        request_timeout_ms,
        token_refresh_timeout_ms,
        enable_vm_pools,
        global_native_sim_concurrency,
        global_vm_sim_concurrency,
        reset_allowance_tokens,
        stream_stale_secs,
        stream_missing_block_burst,
        stream_missing_block_window_secs,
        stream_error_burst,
        stream_error_window_secs,
        resync_grace_secs,
        stream_restart_backoff_min_ms,
        stream_restart_backoff_max_ms,
        stream_restart_backoff_jitter_pct,
        readiness_stale_secs,
        memory,
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub chain_profile: ChainProfile,
    pub tycho_url: String,
    pub api_key: String,
    pub rpc_url: Option<String>,
    pub gas_price_refresh_interval_ms: u64,
    pub gas_price_failure_tolerance: u64,
    pub tvl_threshold: f64,
    pub tvl_keep_threshold: f64,
    pub port: u16,
    pub host: String,
    pub quote_timeout_ms: u64,
    pub pool_timeout_native_ms: u64,
    pub pool_timeout_vm_ms: u64,
    pub request_timeout_ms: u64,
    pub token_refresh_timeout_ms: u64,
    pub enable_vm_pools: bool,
    pub global_native_sim_concurrency: usize,
    pub global_vm_sim_concurrency: usize,
    pub reset_allowance_tokens: Arc<HashMap<u64, HashSet<Bytes>>>,
    pub stream_stale_secs: u64,
    pub stream_missing_block_burst: u64,
    pub stream_missing_block_window_secs: u64,
    pub stream_error_burst: u64,
    pub stream_error_window_secs: u64,
    pub resync_grace_secs: u64,
    pub stream_restart_backoff_min_ms: u64,
    pub stream_restart_backoff_max_ms: u64,
    pub stream_restart_backoff_jitter_pct: f64,
    pub readiness_stale_secs: u64,
    pub memory: MemoryConfig,
}

const ETHEREUM_USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";

fn parse_address(value: &str) -> Bytes {
    let bytes = Bytes::from_str(value).expect("reset_allowance_tokens address is invalid");
    assert!(
        bytes.len() == 20,
        "reset_allowance_tokens address must be 20 bytes"
    );
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_ethereum_profile() {
        let profile = resolve_chain_profile(1);
        assert_eq!(profile.chain, Chain::Ethereum);
        assert!(profile.native_protocols.contains(&"uniswap_v2".to_string()));
        assert!(profile.native_protocols.contains(&"uniswap_v3".to_string()));
        assert!(profile.vm_protocols.contains(&"vm:curve".to_string()));
        assert!(profile
            .native_token_protocol_allowlist
            .contains(&"rocketpool".to_string()));
        assert!(profile.reset_allowance_tokens.contains_key(&1));
    }

    #[test]
    fn resolve_base_profile() {
        let profile = resolve_chain_profile(8453);
        assert_eq!(profile.chain, Chain::Base);
        assert!(profile.native_protocols.contains(&"uniswap_v2".to_string()));
        assert!(profile.native_protocols.contains(&"uniswap_v3".to_string()));
        assert!(profile.native_protocols.contains(&"uniswap_v4".to_string()));
        assert!(profile
            .native_protocols
            .contains(&"pancakeswap_v3".to_string()));
        assert_eq!(profile.native_protocols.len(), 4);
        assert!(profile.vm_protocols.is_empty());
        assert!(profile.native_token_protocol_allowlist.is_empty());
        assert!(profile.reset_allowance_tokens.is_empty());
    }

    #[test]
    #[should_panic(expected = "Unsupported CHAIN_ID=999")]
    fn resolve_unsupported_chain_panics() {
        resolve_chain_profile(999);
    }
}
