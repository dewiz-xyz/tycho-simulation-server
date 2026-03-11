use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use tycho_simulation::tycho_common::Bytes;

mod logging;
mod memory;
pub use logging::init_logging;
pub use memory::MemoryConfig;

pub fn load_config() -> AppConfig {
    dotenv::dotenv().ok();

    let network = load_network_config();
    let timeouts = load_timeout_config();
    let concurrency = load_concurrency_config();
    let stream = load_stream_config();
    let reset_allowance_tokens = Arc::new(default_reset_allowance_tokens());
    let memory = MemoryConfig::from_env();

    AppConfig {
        tycho_url: network.tycho_url,
        api_key: network.api_key,
        rpc_url: network.rpc_url,
        gas_price_refresh_interval_ms: network.gas_price_refresh_interval_ms,
        gas_price_failure_tolerance: network.gas_price_failure_tolerance,
        tvl_threshold: network.tvl_threshold,
        tvl_keep_threshold: network.tvl_keep_threshold,
        port: network.port,
        host: network.host,
        quote_timeout_ms: timeouts.quote_timeout_ms,
        pool_timeout_native_ms: timeouts.pool_timeout_native_ms,
        pool_timeout_vm_ms: timeouts.pool_timeout_vm_ms,
        request_timeout_ms: timeouts.request_timeout_ms,
        token_refresh_timeout_ms: timeouts.token_refresh_timeout_ms,
        enable_vm_pools: network.enable_vm_pools,
        global_native_sim_concurrency: concurrency.global_native_sim_concurrency,
        global_vm_sim_concurrency: concurrency.global_vm_sim_concurrency,
        reset_allowance_tokens,
        stream_stale_secs: stream.stream_stale_secs,
        stream_missing_block_burst: stream.stream_missing_block_burst,
        stream_missing_block_window_secs: stream.stream_missing_block_window_secs,
        stream_error_burst: stream.stream_error_burst,
        stream_error_window_secs: stream.stream_error_window_secs,
        resync_grace_secs: stream.resync_grace_secs,
        stream_restart_backoff_min_ms: stream.stream_restart_backoff_min_ms,
        stream_restart_backoff_max_ms: stream.stream_restart_backoff_max_ms,
        stream_restart_backoff_jitter_pct: stream.stream_restart_backoff_jitter_pct,
        readiness_stale_secs: stream.readiness_stale_secs,
        memory,
    }
}

struct NetworkConfig {
    tycho_url: String,
    api_key: String,
    rpc_url: Option<String>,
    gas_price_refresh_interval_ms: u64,
    gas_price_failure_tolerance: u64,
    tvl_threshold: f64,
    tvl_keep_threshold: f64,
    port: u16,
    host: IpAddr,
    enable_vm_pools: bool,
}

struct TimeoutConfig {
    quote_timeout_ms: u64,
    pool_timeout_native_ms: u64,
    pool_timeout_vm_ms: u64,
    request_timeout_ms: u64,
    token_refresh_timeout_ms: u64,
}

struct ConcurrencyConfig {
    global_native_sim_concurrency: usize,
    global_vm_sim_concurrency: usize,
}

struct StreamConfig {
    stream_stale_secs: u64,
    stream_missing_block_burst: u64,
    stream_missing_block_window_secs: u64,
    stream_error_burst: u64,
    stream_error_window_secs: u64,
    resync_grace_secs: u64,
    stream_restart_backoff_min_ms: u64,
    stream_restart_backoff_max_ms: u64,
    stream_restart_backoff_jitter_pct: f64,
    readiness_stale_secs: u64,
}

fn load_network_config() -> NetworkConfig {
    let tycho_url = env_or_default("TYCHO_URL", "tycho-beta.propellerheads.xyz");
    let api_key = require_env("TYCHO_API_KEY");
    let rpc_url = optional_trimmed_env("RPC_URL");
    let gas_price_refresh_interval_ms: u64 =
        parse_env_or_default("GAS_PRICE_REFRESH_INTERVAL_MS", "5000");
    let gas_price_failure_tolerance: u64 =
        parse_env_or_default("GAS_PRICE_FAILURE_TOLERANCE", "50");
    let tvl_threshold: f64 = parse_env_or_default("TVL_THRESHOLD", "100");
    let tvl_keep_ratio: f64 = parse_env_or_default("TVL_KEEP_RATIO", "0.2");
    let tvl_keep_threshold: f64 = (tvl_threshold * tvl_keep_ratio).min(tvl_threshold);
    let port: u16 = parse_env_or_default("PORT", "3000");
    let host = parse_host_env();

    assert!(tvl_threshold > 0.0, "TVL_THRESHOLD must be > 0");
    assert!(
        tvl_keep_ratio > 0.0 && tvl_keep_ratio <= 1.0,
        "TVL_KEEP_RATIO must be in (0, 1]"
    );
    assert!(
        tvl_keep_threshold > 0.0,
        "Derived TVL keep threshold must be > 0"
    );
    assert!(
        gas_price_refresh_interval_ms > 0,
        "GAS_PRICE_REFRESH_INTERVAL_MS must be > 0"
    );

    NetworkConfig {
        tycho_url,
        api_key,
        rpc_url,
        gas_price_refresh_interval_ms,
        gas_price_failure_tolerance,
        tvl_threshold,
        tvl_keep_threshold,
        port,
        host,
        enable_vm_pools: parse_env_or_default("ENABLE_VM_POOLS", "true"),
    }
}

fn load_timeout_config() -> TimeoutConfig {
    let quote_timeout_ms = parse_env_or_default("QUOTE_TIMEOUT_MS", "150");
    let pool_timeout_native_ms = parse_env_or_default("POOL_TIMEOUT_NATIVE_MS", "20");
    let pool_timeout_vm_ms = parse_env_or_default("POOL_TIMEOUT_VM_MS", "150");
    let request_timeout_ms = parse_env_or_default("REQUEST_TIMEOUT_MS", "4000");
    let token_refresh_timeout_ms = parse_env_or_default("TOKEN_REFRESH_TIMEOUT_MS", "1000");

    assert!(quote_timeout_ms > 0, "QUOTE_TIMEOUT_MS must be > 0");
    assert!(
        pool_timeout_native_ms > 0,
        "POOL_TIMEOUT_NATIVE_MS must be > 0"
    );
    assert!(pool_timeout_vm_ms > 0, "POOL_TIMEOUT_VM_MS must be > 0");
    assert!(
        token_refresh_timeout_ms > 0,
        "TOKEN_REFRESH_TIMEOUT_MS must be > 0"
    );

    TimeoutConfig {
        quote_timeout_ms,
        pool_timeout_native_ms,
        pool_timeout_vm_ms,
        request_timeout_ms,
        token_refresh_timeout_ms,
    }
}

fn load_concurrency_config() -> ConcurrencyConfig {
    let cpu_count = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(1);
    let default_native = (cpu_count.saturating_mul(4)).max(1);
    let default_vm = cpu_count.max(1);

    ConcurrencyConfig {
        global_native_sim_concurrency: optional_parsed_env("GLOBAL_NATIVE_SIM_CONCURRENCY")
            .unwrap_or(default_native)
            .max(1),
        global_vm_sim_concurrency: optional_parsed_env("GLOBAL_VM_SIM_CONCURRENCY")
            .unwrap_or(default_vm)
            .max(1),
    }
}

fn load_stream_config() -> StreamConfig {
    let stream_stale_secs = parse_env_or_default("STREAM_STALE_SECS", "120");
    let stream_missing_block_burst = parse_env_or_default("STREAM_MISSING_BLOCK_BURST", "3");
    let stream_missing_block_window_secs =
        parse_env_or_default("STREAM_MISSING_BLOCK_WINDOW_SECS", "60");
    let stream_error_burst = parse_env_or_default("STREAM_ERROR_BURST", "3");
    let stream_error_window_secs = parse_env_or_default("STREAM_ERROR_WINDOW_SECS", "60");
    let resync_grace_secs = parse_env_or_default("RESYNC_GRACE_SECS", "60");
    let stream_restart_backoff_min_ms =
        parse_env_or_default("STREAM_RESTART_BACKOFF_MIN_MS", "500");
    let stream_restart_backoff_max_ms =
        parse_env_or_default("STREAM_RESTART_BACKOFF_MAX_MS", "30000");
    let stream_restart_backoff_jitter_pct =
        parse_env_or_default("STREAM_RESTART_BACKOFF_JITTER_PCT", "0.2");
    let readiness_stale_secs = parse_env_or_default("READINESS_STALE_SECS", "120");

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

    StreamConfig {
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
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub tycho_url: String,
    pub api_key: String,
    pub rpc_url: Option<String>,
    pub gas_price_refresh_interval_ms: u64,
    pub gas_price_failure_tolerance: u64,
    pub tvl_threshold: f64,
    pub tvl_keep_threshold: f64,
    pub port: u16,
    pub host: IpAddr,
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

const ETHEREUM_CHAIN_ID: u64 = 1;
const ETHEREUM_USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";

fn default_reset_allowance_tokens() -> HashMap<u64, HashSet<Bytes>> {
    // Tokens that require approve(0) before approve(amount).
    let mut tokens = HashMap::new();
    let mut mainnet = HashSet::new();
    mainnet.insert(parse_address(ETHEREUM_USDT));
    tokens.insert(ETHEREUM_CHAIN_ID, mainnet);
    tokens
}

fn parse_address(value: &str) -> Bytes {
    let bytes: Bytes = parse_value_or_panic("reset_allowance_tokens address", value);
    assert!(
        bytes.len() == 20,
        "reset_allowance_tokens address must be 20 bytes"
    );
    bytes
}

fn env_or_default(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn parse_host_env() -> IpAddr {
    let host = env_or_default("HOST", "127.0.0.1");
    parse_value_or_panic("HOST", &host)
}

fn optional_trimmed_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn optional_parsed_env<T>(key: &str) -> Option<T>
where
    T: FromStr,
{
    std::env::var(key)
        .ok()
        .map(|value| parse_value_or_panic(key, &value))
}

fn parse_env_or_default<T>(key: &str, default: &str) -> T
where
    T: FromStr,
{
    let value = env_or_default(key, default);
    parse_value_or_panic(key, &value)
}

#[expect(
    clippy::panic,
    reason = "startup config remains fail-fast on missing required env"
)]
fn require_env(key: &str) -> String {
    match std::env::var(key) {
        Ok(value) => value,
        Err(_) => panic!("{key} must be set"),
    }
}

#[expect(
    clippy::panic,
    reason = "startup config remains fail-fast on invalid env"
)]
fn parse_value_or_panic<T>(key: &str, value: &str) -> T
where
    T: FromStr,
{
    match value.parse() {
        Ok(parsed) => parsed,
        Err(_) => panic!("Invalid {key}"),
    }
}
