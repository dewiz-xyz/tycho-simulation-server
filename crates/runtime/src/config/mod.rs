use std::collections::{HashMap, HashSet};
use std::env;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use tycho_simulation::tycho_common::{models::Chain, Bytes};

mod logging;
mod manifest;
mod memory;
use crate::models::erc4626::Erc4626PairPolicy;
pub use logging::init_logging;
pub(crate) use manifest::{load_manifest_registries, resolve_chain_config, MANIFEST_PATH};
pub use memory::MemoryConfig;

/// Per-chain runtime profile resolved from `CHAIN_ID`.
#[derive(Clone, Debug)]
pub struct ChainProfile {
    pub chain: Chain,
    pub native_protocols: Vec<String>,
    pub vm_protocols: Vec<String>,
    pub rfq_protocols: Vec<String>,
    /// Protocols allowed to swap with the native token (e.g. rocketpool on Ethereum).
    pub native_token_protocol_allowlist: Vec<String>,
    pub reset_allowance_tokens: HashMap<u64, HashSet<Bytes>>,
    pub erc4626_pair_policies: Vec<Erc4626PairPolicy>,
}

pub fn load_config() -> AppConfig {
    dotenv::dotenv().ok();

    let chain_id: u64 = match require_env("CHAIN_ID").parse() {
        Ok(chain_id) => chain_id,
        Err(_) => {
            eprintln!("CHAIN_ID must be a valid u64");
            std::process::exit(2);
        }
    };
    // The manifest owns the chain-specific wiring now. Env vars still cover runtime-only settings
    // and the handful of overrides we want to keep outside the checked-in defaults.
    let registries = match load_manifest_registries(std::path::Path::new(MANIFEST_PATH)) {
        Ok(registries) => registries,
        Err(message) => {
            eprintln!("Failed to load simulator-manifest.toml: {message}");
            std::process::exit(2);
        }
    };
    let network = load_network_config();
    let resolved_chain = match resolve_chain_config(
        &registries,
        chain_id,
        optional_trimmed_env("HASHFLOW_FILENAME_CSV"),
    ) {
        Ok(chain) => chain,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let timeouts = load_timeout_config();
    let concurrency = load_concurrency_config();
    let stream = load_stream_config();
    let slippage = load_slippage_config();
    // Keep the rest of the runtime on the same AppConfig shape. The manifest is just the new
    // source of truth for the chain-specific slice.
    let chain_profile = ChainProfile {
        chain: resolved_chain.chain_profile.chain,
        native_protocols: resolved_chain.chain_profile.native_protocols,
        vm_protocols: resolved_chain.chain_profile.vm_protocols,
        rfq_protocols: resolved_chain.chain_profile.rfq_protocols,
        native_token_protocol_allowlist: resolved_chain
            .chain_profile
            .native_token_protocol_allowlist,
        reset_allowance_tokens: resolved_chain.chain_profile.reset_allowance_tokens,
        erc4626_pair_policies: resolved_chain.chain_profile.erc4626_pair_policies,
    };
    let reset_allowance_tokens = Arc::new(chain_profile.reset_allowance_tokens.clone());
    let erc4626_pair_policies = Arc::new(chain_profile.erc4626_pair_policies.clone());
    let memory = MemoryConfig::from_env();
    let rfq_enabled = rfq_effectively_enabled(network.enable_rfq_pools, &chain_profile);
    let (bebop_user, bebop_key, hashflow_user, hashflow_key) = load_rfq_credentials(rfq_enabled);

    AppConfig {
        chain_profile,
        tycho_url: resolved_chain.tycho_url,
        bebop_url: resolved_chain.bebop_url,
        hashflow_filename: resolved_chain.hashflow_filename,
        api_key: network.api_key,
        rpc_url: network.rpc_url,
        tvl_threshold: network.tvl_threshold,
        tvl_keep_threshold: network.tvl_keep_threshold,
        port: network.port,
        host: network.host,
        quote_timeout_ms: timeouts.quote_timeout_ms,
        pool_timeout_native_ms: timeouts.pool_timeout_native_ms,
        pool_timeout_vm_ms: timeouts.pool_timeout_vm_ms,
        pool_timeout_rfq_ms: timeouts.pool_timeout_rfq_ms,
        request_timeout_ms: timeouts.request_timeout_ms,
        token_refresh_timeout_ms: timeouts.token_refresh_timeout_ms,
        enable_vm_pools: network.enable_vm_pools,
        enable_rfq_pools: network.enable_rfq_pools,
        global_native_sim_concurrency: concurrency.global_native_sim_concurrency,
        global_vm_sim_concurrency: concurrency.global_vm_sim_concurrency,
        global_rfq_sim_concurrency: concurrency.global_rfq_sim_concurrency,
        reset_allowance_tokens,
        erc4626_pair_policies,
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
        slippage,
        memory,
        bebop_key,
        bebop_user,
        hashflow_key,
        hashflow_user,
    }
}

pub fn load_broadcaster_config() -> BroadcasterConfig {
    dotenv::dotenv().ok();

    let chain_id: u64 = match require_env("CHAIN_ID").parse() {
        Ok(chain_id) => chain_id,
        Err(_) => {
            eprintln!("CHAIN_ID must be a valid u64");
            std::process::exit(2);
        }
    };
    let registries = match load_manifest_registries(std::path::Path::new(MANIFEST_PATH)) {
        Ok(registries) => registries,
        Err(message) => {
            eprintln!("Failed to load simulator-manifest.toml: {message}");
            std::process::exit(2);
        }
    };
    let network = load_network_config();
    let timeouts = load_timeout_config();
    let resolved_chain = match resolve_chain_config(&registries, chain_id, None) {
        Ok(chain) => chain,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let stream = load_stream_config();
    let memory = MemoryConfig::from_env();
    let tuning = load_broadcaster_tuning();
    let chain_profile = ChainProfile {
        chain: resolved_chain.chain_profile.chain,
        native_protocols: resolved_chain.chain_profile.native_protocols,
        vm_protocols: resolved_chain.chain_profile.vm_protocols,
        rfq_protocols: resolved_chain.chain_profile.rfq_protocols,
        native_token_protocol_allowlist: resolved_chain
            .chain_profile
            .native_token_protocol_allowlist,
        reset_allowance_tokens: resolved_chain.chain_profile.reset_allowance_tokens,
        erc4626_pair_policies: resolved_chain.chain_profile.erc4626_pair_policies,
    };

    BroadcasterConfig {
        chain_profile,
        tycho_url: resolved_chain.tycho_url,
        api_key: network.api_key,
        tvl_threshold: network.tvl_threshold,
        tvl_keep_threshold: network.tvl_keep_threshold,
        port: network.port,
        host: network.host,
        token_refresh_timeout_ms: timeouts.token_refresh_timeout_ms,
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
        tuning,
    }
}

fn rfq_effectively_enabled(enable_rfq_pools: bool, chain_profile: &ChainProfile) -> bool {
    enable_rfq_pools && !chain_profile.rfq_protocols.is_empty()
}

fn load_rfq_credentials(rfq_enabled: bool) -> (String, String, String, String) {
    if !rfq_enabled {
        return (String::new(), String::new(), String::new(), String::new());
    }

    let bebop_user = match env::var("BEBOP_USER") {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let bebop_key = match env::var("BEBOP_KEY") {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let hashflow_user = match env::var("HASHFLOW_USER") {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let hashflow_key = match env::var("HASHFLOW_KEY") {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    (bebop_user, bebop_key, hashflow_user, hashflow_key)
}

struct NetworkConfig {
    api_key: String,
    rpc_url: Option<String>,
    tvl_threshold: f64,
    tvl_keep_threshold: f64,
    port: u16,
    host: IpAddr,
    enable_vm_pools: bool,
    enable_rfq_pools: bool,
}

struct TimeoutConfig {
    quote_timeout_ms: u64,
    pool_timeout_native_ms: u64,
    pool_timeout_vm_ms: u64,
    pool_timeout_rfq_ms: u64,
    request_timeout_ms: u64,
    token_refresh_timeout_ms: u64,
}

struct ConcurrencyConfig {
    global_native_sim_concurrency: usize,
    global_vm_sim_concurrency: usize,
    global_rfq_sim_concurrency: usize,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BroadcasterTuning {
    pub snapshot_chunk_size: usize,
    pub subscriber_buffer_capacity: usize,
    pub heartbeat_interval_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlippageConfig {
    pub min_dynamic_slippage_bps: u32,
    pub max_dynamic_slippage_bps: u32,
    pub utilization_coefficient_bps: u32,
    pub sensitivity_coefficient_bps: u32,
    pub soft_ladder_utilization_cap_bps: u32,
    pub cumulative_degradation_coefficient_bps: u32,
    pub saturation_ramp_start_slippage_bps: u32,
}

impl Default for SlippageConfig {
    fn default() -> Self {
        Self {
            min_dynamic_slippage_bps: 1,
            max_dynamic_slippage_bps: 200,
            utilization_coefficient_bps: 60,
            sensitivity_coefficient_bps: 2_500,
            soft_ladder_utilization_cap_bps: 3_500,
            cumulative_degradation_coefficient_bps: 300,
            saturation_ramp_start_slippage_bps: 45,
        }
    }
}

fn load_network_config() -> NetworkConfig {
    let api_key = require_env("TYCHO_API_KEY");
    let rpc_url = optional_trimmed_env("RPC_URL");
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

    NetworkConfig {
        api_key,
        rpc_url,
        tvl_threshold,
        tvl_keep_threshold,
        port,
        host,
        enable_vm_pools: parse_env_or_default("ENABLE_VM_POOLS", "true"),
        enable_rfq_pools: parse_env_or_default("ENABLE_RFQ_POOLS", "false"),
    }
}

fn load_timeout_config() -> TimeoutConfig {
    let quote_timeout_ms = parse_env_or_default("QUOTE_TIMEOUT_MS", "150");
    let pool_timeout_native_ms = parse_env_or_default("POOL_TIMEOUT_NATIVE_MS", "20");
    let pool_timeout_vm_ms = parse_env_or_default("POOL_TIMEOUT_VM_MS", "150");
    let pool_timeout_rfq_ms = parse_env_or_default("POOL_TIMEOUT_RFQ_MS", "150");
    let request_timeout_ms = parse_env_or_default("REQUEST_TIMEOUT_MS", "4000");
    let token_refresh_timeout_ms = parse_env_or_default("TOKEN_REFRESH_TIMEOUT_MS", "1000");

    assert!(quote_timeout_ms > 0, "QUOTE_TIMEOUT_MS must be > 0");
    assert!(
        pool_timeout_native_ms > 0,
        "POOL_TIMEOUT_NATIVE_MS must be > 0"
    );
    assert!(pool_timeout_vm_ms > 0, "POOL_TIMEOUT_VM_MS must be > 0");
    assert!(pool_timeout_rfq_ms > 0, "POOL_TIMEOUT_RFQ_MS must be > 0");
    assert!(
        token_refresh_timeout_ms > 0,
        "TOKEN_REFRESH_TIMEOUT_MS must be > 0"
    );

    TimeoutConfig {
        quote_timeout_ms,
        pool_timeout_native_ms,
        pool_timeout_vm_ms,
        pool_timeout_rfq_ms,
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
    let default_rfq = cpu_count.max(1);

    ConcurrencyConfig {
        global_native_sim_concurrency: optional_parsed_env("GLOBAL_NATIVE_SIM_CONCURRENCY")
            .unwrap_or(default_native)
            .max(1),
        global_vm_sim_concurrency: optional_parsed_env("GLOBAL_VM_SIM_CONCURRENCY")
            .unwrap_or(default_vm)
            .max(1),
        global_rfq_sim_concurrency: optional_parsed_env("GLOBAL_RFQ_SIM_CONCURRENCY")
            .unwrap_or(default_rfq)
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
    let readiness_stale_secs = parse_env_or_default("READINESS_STALE_SECS", "300");

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

fn load_broadcaster_tuning() -> BroadcasterTuning {
    let snapshot_chunk_size = parse_env_or_default("BROADCASTER_SNAPSHOT_CHUNK_SIZE", "500");
    let subscriber_buffer_capacity =
        parse_env_or_default("BROADCASTER_SUBSCRIBER_BUFFER_CAPACITY", "128");
    let heartbeat_interval_secs = parse_env_or_default("BROADCASTER_HEARTBEAT_INTERVAL_SECS", "5");

    assert!(
        snapshot_chunk_size > 0,
        "BROADCASTER_SNAPSHOT_CHUNK_SIZE must be > 0"
    );
    assert!(
        subscriber_buffer_capacity > 0,
        "BROADCASTER_SUBSCRIBER_BUFFER_CAPACITY must be > 0"
    );
    assert!(
        heartbeat_interval_secs > 0,
        "BROADCASTER_HEARTBEAT_INTERVAL_SECS must be > 0"
    );

    BroadcasterTuning {
        snapshot_chunk_size,
        subscriber_buffer_capacity,
        heartbeat_interval_secs,
    }
}

fn load_slippage_config() -> SlippageConfig {
    let defaults = SlippageConfig::default();
    let config = SlippageConfig {
        min_dynamic_slippage_bps: optional_parsed_env("MIN_DYNAMIC_SLIPPAGE_BPS")
            .unwrap_or(defaults.min_dynamic_slippage_bps),
        max_dynamic_slippage_bps: optional_parsed_env("MAX_DYNAMIC_SLIPPAGE_BPS")
            .unwrap_or(defaults.max_dynamic_slippage_bps),
        utilization_coefficient_bps: optional_parsed_env("UTILIZATION_COEFFICIENT_BPS")
            .unwrap_or(defaults.utilization_coefficient_bps),
        sensitivity_coefficient_bps: optional_parsed_env("SENSITIVITY_COEFFICIENT_BPS")
            .unwrap_or(defaults.sensitivity_coefficient_bps),
        soft_ladder_utilization_cap_bps: optional_parsed_env("SOFT_LADDER_UTILIZATION_CAP_BPS")
            .unwrap_or(defaults.soft_ladder_utilization_cap_bps),
        cumulative_degradation_coefficient_bps: optional_parsed_env(
            "CUMULATIVE_DEGRADATION_COEFFICIENT_BPS",
        )
        .unwrap_or(defaults.cumulative_degradation_coefficient_bps),
        saturation_ramp_start_slippage_bps: optional_parsed_env(
            "SATURATION_RAMP_START_SLIPPAGE_BPS",
        )
        .unwrap_or(defaults.saturation_ramp_start_slippage_bps),
    };
    assert!(
        config.min_dynamic_slippage_bps <= config.max_dynamic_slippage_bps,
        "slippage min must be <= max"
    );
    assert!(
        config.saturation_ramp_start_slippage_bps <= config.max_dynamic_slippage_bps,
        "saturation ramp start slippage must be <= slippage max"
    );
    config
}

#[derive(Clone)]
pub struct AppConfig {
    pub chain_profile: ChainProfile,
    pub tycho_url: String,
    pub bebop_url: String,
    pub hashflow_filename: String,
    pub api_key: String,
    pub rpc_url: Option<String>,
    pub tvl_threshold: f64,
    pub tvl_keep_threshold: f64,
    pub port: u16,
    pub host: IpAddr,
    pub quote_timeout_ms: u64,
    pub pool_timeout_native_ms: u64,
    pub pool_timeout_vm_ms: u64,
    pub pool_timeout_rfq_ms: u64,
    pub request_timeout_ms: u64,
    pub token_refresh_timeout_ms: u64,
    pub enable_vm_pools: bool,
    pub enable_rfq_pools: bool,
    pub global_native_sim_concurrency: usize,
    pub global_vm_sim_concurrency: usize,
    pub global_rfq_sim_concurrency: usize,
    pub reset_allowance_tokens: Arc<HashMap<u64, HashSet<Bytes>>>,
    pub erc4626_pair_policies: Arc<Vec<Erc4626PairPolicy>>,
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
    pub slippage: SlippageConfig,
    pub memory: MemoryConfig,
    pub bebop_user: String,
    pub bebop_key: String,
    pub hashflow_user: String,
    pub hashflow_key: String,
}

#[derive(Clone)]
pub struct BroadcasterConfig {
    pub chain_profile: ChainProfile,
    pub tycho_url: String,
    pub api_key: String,
    pub tvl_threshold: f64,
    pub tvl_keep_threshold: f64,
    pub port: u16,
    pub host: IpAddr,
    pub token_refresh_timeout_ms: u64,
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
    pub tuning: BroadcasterTuning,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    const SLIPPAGE_ENV_KEYS: [&str; 7] = [
        "MIN_DYNAMIC_SLIPPAGE_BPS",
        "MAX_DYNAMIC_SLIPPAGE_BPS",
        "UTILIZATION_COEFFICIENT_BPS",
        "SENSITIVITY_COEFFICIENT_BPS",
        "SOFT_LADDER_UTILIZATION_CAP_BPS",
        "CUMULATIVE_DEGRADATION_COEFFICIENT_BPS",
        "SATURATION_RAMP_START_SLIPPAGE_BPS",
    ];
    const BROADCASTER_TUNING_ENV_KEYS: [&str; 3] = [
        "BROADCASTER_SNAPSHOT_CHUNK_SIZE",
        "BROADCASTER_SUBSCRIBER_BUFFER_CAPACITY",
        "BROADCASTER_HEARTBEAT_INTERVAL_SECS",
    ];

    fn clear_slippage_env() {
        for key in SLIPPAGE_ENV_KEYS {
            std::env::remove_var(key);
        }
    }

    fn clear_broadcaster_tuning_env() {
        for key in BROADCASTER_TUNING_ENV_KEYS {
            std::env::remove_var(key);
        }
    }

    fn manifest_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../simulator-manifest.toml")
    }

    fn load_test_registries() -> manifest::ManifestRegistries {
        let Ok(registries) = load_manifest_registries(&manifest_path()) else {
            unreachable!("expected checked-in simulator-manifest.toml to parse");
        };
        registries
    }

    #[test]
    fn resolve_chain_config_reads_ethereum_from_manifest() {
        let registries = load_test_registries();
        let Ok(chain) = resolve_chain_config(&registries, 1, None) else {
            unreachable!("expected ethereum manifest entry");
        };

        assert_eq!(chain.chain_profile.chain, Chain::Ethereum);
        assert_eq!(chain.tycho_url, "tycho-beta.propellerheads.xyz");
        assert_eq!(
            chain.bebop_url,
            "https://api.bebop.xyz/pmm/ethereum/v3/tokens"
        );
        assert_eq!(chain.hashflow_filename, "./hashflow_supported_tokens.csv");
        assert!(chain
            .chain_profile
            .native_protocols
            .contains(&"rocketpool".to_string()));
        assert!(chain
            .chain_profile
            .vm_protocols
            .contains(&"vm:curve".to_string()));
        assert!(chain
            .chain_profile
            .rfq_protocols
            .contains(&"rfq:bebop".to_string()));
        assert!(chain
            .chain_profile
            .native_token_protocol_allowlist
            .contains(&"rocketpool".to_string()));
        assert!(chain.chain_profile.reset_allowance_tokens.contains_key(&1));
        assert_eq!(chain.chain_profile.erc4626_pair_policies.len(), 3);
    }

    #[test]
    fn resolve_chain_config_reads_base_from_manifest() {
        let registries = load_test_registries();
        let Ok(chain) = resolve_chain_config(&registries, 8453, None) else {
            unreachable!("expected base manifest entry");
        };

        assert_eq!(chain.chain_profile.chain, Chain::Base);
        assert_eq!(chain.tycho_url, "tycho-base-beta.propellerheads.xyz");
        assert!(chain
            .chain_profile
            .native_protocols
            .contains(&"aerodrome_slipstreams".to_string()));
        assert!(chain.chain_profile.vm_protocols.is_empty());
        assert!(chain
            .chain_profile
            .rfq_protocols
            .contains(&"rfq:hashflow".to_string()));
        assert!(chain
            .chain_profile
            .native_token_protocol_allowlist
            .is_empty());
        assert!(chain.chain_profile.reset_allowance_tokens.is_empty());
        assert!(chain.chain_profile.erc4626_pair_policies.is_empty());
    }

    #[test]
    fn resolve_chain_config_prefers_hashflow_env_override() {
        let registries = load_test_registries();
        let Ok(chain) = resolve_chain_config(&registries, 8453, Some("/tmp/hashflow.csv".into()))
        else {
            unreachable!("expected base manifest entry");
        };

        assert_eq!(chain.hashflow_filename, "/tmp/hashflow.csv");
    }

    #[test]
    fn parse_manifest_rejects_duplicate_protocol_ids() {
        let manifest = r#"
[[protocols]]
id = "uniswap_v2"
backend = "native"

[[protocols]]
id = "uniswap_v2"
backend = "native"

[[route_policies]]
id = "default"
native_token_protocol_allowlist = []
reset_allowance_tokens = []

[[chains]]
chain_id = 1
tycho_url = "tycho"
bebop_url = "bebop"
hashflow_filename = "./hashflow.csv"
native_protocols = ["uniswap_v2"]
vm_protocols = []
rfq_protocols = []
route_policy = "default"
"#;

        let Err(err) = manifest::parse_manifest_registries(manifest) else {
            unreachable!("expected duplicate protocol ids to fail");
        };

        assert!(err.to_string().contains("duplicate protocol id"));
    }

    #[test]
    fn parse_manifest_rejects_unknown_route_policy_references() {
        let manifest = r#"
[[protocols]]
id = "uniswap_v2"
backend = "native"

[[route_policies]]
id = "default"
native_token_protocol_allowlist = []
reset_allowance_tokens = []

[[chains]]
chain_id = 1
tycho_url = "tycho"
bebop_url = "bebop"
hashflow_filename = "./hashflow.csv"
native_protocols = ["uniswap_v2"]
vm_protocols = []
rfq_protocols = []
route_policy = "missing"
"#;

        let Err(err) = manifest::parse_manifest_registries(manifest) else {
            unreachable!("expected missing route-policy reference to fail");
        };

        assert!(err.to_string().contains("unknown route policy"));
    }

    #[test]
    fn parse_manifest_rejects_unsupported_chain_ids() {
        let manifest = r#"
[[protocols]]
id = "uniswap_v2"
backend = "native"

[[route_policies]]
id = "default"
native_token_protocol_allowlist = []
reset_allowance_tokens = []

[[chains]]
chain_id = 999999
tycho_url = "tycho"
bebop_url = "bebop"
hashflow_filename = "./hashflow.csv"
native_protocols = ["uniswap_v2"]
vm_protocols = []
rfq_protocols = []
route_policy = "default"
"#;

        let Err(err) = manifest::parse_manifest_registries(manifest) else {
            unreachable!("expected unsupported chain id to fail");
        };

        assert!(err
            .to_string()
            .contains("not supported by the current runtime"));
    }

    #[test]
    fn parse_manifest_rejects_erc4626_pairs_without_enabled_directions() {
        let manifest = r#"
[[protocols]]
id = "erc4626"
backend = "native"

[[route_policies]]
id = "default"
native_token_protocol_allowlist = []
reset_allowance_tokens = []

[[route_policies.erc4626_pairs]]
asset_symbol = "USDS"
share_symbol = "sUSDS"
asset = "0xdC035D45d973E3EC169d2276DDab16f1e407384F"
share = "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd"
allow_asset_to_share = false
allow_share_to_asset = false

[[chains]]
chain_id = 1
tycho_url = "tycho"
bebop_url = "bebop"
hashflow_filename = "./hashflow.csv"
native_protocols = ["erc4626"]
vm_protocols = []
rfq_protocols = []
route_policy = "default"
"#;

        let Err(err) = manifest::parse_manifest_registries(manifest) else {
            unreachable!("expected invalid ERC4626 pair to fail");
        };

        assert!(err.to_string().contains("no enabled directions"));
    }

    #[test]
    fn parse_manifest_stores_trimmed_registry_values() {
        let manifest = r#"
[[protocols]]
id = " uniswap_v2 "
backend = "native"

[[protocols]]
id = " rfq:bebop "
backend = "rfq"

[[route_policies]]
id = " default "
native_token_protocol_allowlist = [" uniswap_v2 "]
reset_allowance_tokens = []

[[chains]]
chain_id = 1
tycho_url = " tycho "
bebop_url = " https://api.bebop.xyz/pmm/ethereum/v3/tokens "
hashflow_filename = " ./hashflow.csv "
native_protocols = [" uniswap_v2 "]
vm_protocols = []
rfq_protocols = [" rfq:bebop "]
route_policy = " default "
"#;

        let Ok(registries) = manifest::parse_manifest_registries(manifest) else {
            unreachable!("expected manifest with padded strings to parse");
        };
        let Ok(chain) = resolve_chain_config(&registries, 1, None) else {
            unreachable!("expected ethereum manifest entry");
        };

        assert_eq!(chain.tycho_url, "tycho");
        assert_eq!(
            chain.bebop_url,
            "https://api.bebop.xyz/pmm/ethereum/v3/tokens"
        );
        assert_eq!(chain.hashflow_filename, "./hashflow.csv");
        assert_eq!(chain.chain_profile.native_protocols, vec!["uniswap_v2"]);
        assert!(chain.chain_profile.vm_protocols.is_empty());
        assert_eq!(chain.chain_profile.rfq_protocols, vec!["rfq:bebop"]);
        assert_eq!(
            chain.chain_profile.native_token_protocol_allowlist,
            vec!["uniswap_v2"]
        );
    }

    #[test]
    fn rfq_effectively_enabled_requires_flag_and_protocols() {
        let registries = load_test_registries();
        let ethereum = resolve_chain_config(&registries, 1, None)
            .unwrap_or_else(|_| unreachable!("expected ethereum manifest entry"))
            .chain_profile;
        let base = resolve_chain_config(&registries, 8453, None)
            .unwrap_or_else(|_| unreachable!("expected base manifest entry"))
            .chain_profile;
        let ethereum = ChainProfile {
            chain: ethereum.chain,
            native_protocols: ethereum.native_protocols,
            vm_protocols: ethereum.vm_protocols,
            rfq_protocols: ethereum.rfq_protocols,
            native_token_protocol_allowlist: ethereum.native_token_protocol_allowlist,
            reset_allowance_tokens: ethereum.reset_allowance_tokens,
            erc4626_pair_policies: ethereum.erc4626_pair_policies,
        };
        let base = ChainProfile {
            chain: base.chain,
            native_protocols: base.native_protocols,
            vm_protocols: base.vm_protocols,
            rfq_protocols: base.rfq_protocols,
            native_token_protocol_allowlist: base.native_token_protocol_allowlist,
            reset_allowance_tokens: base.reset_allowance_tokens,
            erc4626_pair_policies: base.erc4626_pair_policies,
        };

        assert!(rfq_effectively_enabled(true, &ethereum));
        assert!(!rfq_effectively_enabled(false, &ethereum));
        assert!(rfq_effectively_enabled(true, &base));
        assert!(!rfq_effectively_enabled(false, &base));
    }

    #[test]
    fn load_rfq_credentials_skips_env_when_disabled() {
        let (bebop_user, bebop_key, hashflow_user, hashflow_key) = load_rfq_credentials(false);

        assert!(bebop_user.is_empty());
        assert!(bebop_key.is_empty());
        assert!(hashflow_user.is_empty());
        assert!(hashflow_key.is_empty());
    }

    #[test]
    fn load_slippage_config_uses_defaults_when_env_missing() {
        let _guard = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_slippage_env();

        let config = load_slippage_config();

        assert_eq!(config, SlippageConfig::default());
    }

    #[test]
    fn load_slippage_config_reads_env_overrides() {
        let _guard = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_slippage_env();
        std::env::set_var("MIN_DYNAMIC_SLIPPAGE_BPS", "3");
        std::env::set_var("MAX_DYNAMIC_SLIPPAGE_BPS", "250");
        std::env::set_var("UTILIZATION_COEFFICIENT_BPS", "75");
        std::env::set_var("SENSITIVITY_COEFFICIENT_BPS", "3000");
        std::env::set_var("SOFT_LADDER_UTILIZATION_CAP_BPS", "4200");
        std::env::set_var("CUMULATIVE_DEGRADATION_COEFFICIENT_BPS", "333");
        std::env::set_var("SATURATION_RAMP_START_SLIPPAGE_BPS", "55");

        let config = load_slippage_config();

        assert_eq!(
            config,
            SlippageConfig {
                min_dynamic_slippage_bps: 3,
                max_dynamic_slippage_bps: 250,
                utilization_coefficient_bps: 75,
                sensitivity_coefficient_bps: 3000,
                soft_ladder_utilization_cap_bps: 4200,
                cumulative_degradation_coefficient_bps: 333,
                saturation_ramp_start_slippage_bps: 55,
            }
        );

        clear_slippage_env();
    }

    #[test]
    fn load_broadcaster_tuning_uses_phase_four_defaults() {
        let _guard = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_broadcaster_tuning_env();

        let tuning = load_broadcaster_tuning();

        assert_eq!(tuning.snapshot_chunk_size, 500);
        assert_eq!(tuning.subscriber_buffer_capacity, 128);
        assert_eq!(tuning.heartbeat_interval_secs, 5);
    }

    #[test]
    fn load_broadcaster_tuning_reads_env_overrides() {
        let _guard = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_broadcaster_tuning_env();
        std::env::set_var("BROADCASTER_SNAPSHOT_CHUNK_SIZE", "64");
        std::env::set_var("BROADCASTER_SUBSCRIBER_BUFFER_CAPACITY", "32");
        std::env::set_var("BROADCASTER_HEARTBEAT_INTERVAL_SECS", "9");

        let tuning = load_broadcaster_tuning();

        assert_eq!(tuning.snapshot_chunk_size, 64);
        assert_eq!(tuning.subscriber_buffer_capacity, 32);
        assert_eq!(tuning.heartbeat_interval_secs, 9);

        clear_broadcaster_tuning_env();
    }
}
