use std::collections::{HashMap, HashSet};
use std::env;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use tycho_simulation::tycho_common::{models::Chain, Bytes};
use tycho_simulation::utils::get_default_url;

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
    pub rfq_protocols: Vec<String>,
    /// Protocols allowed to swap with the native token (e.g. rocketpool on Ethereum).
    pub native_token_protocol_allowlist: Vec<String>,
    pub reset_allowance_tokens: HashMap<u64, HashSet<Bytes>>,
}

pub(crate) const ETHEREUM_NATIVE_PROTOCOLS: &[&str] = &[
    "uniswap_v2",
    "sushiswap_v2",
    "pancakeswap_v2",
    "uniswap_v3",
    "pancakeswap_v3",
    "uniswap_v4",
    "ekubo_v2",
    "fluid_v1",
    "rocketpool",
    "ekubo_v3",
    "erc4626",
];
pub(crate) const ETHEREUM_VM_PROTOCOLS: &[&str] = &["vm:curve", "vm:balancer_v2", "vm:maverick_v2"];
pub(crate) const ETHEREUM_RFQ_PROTOCOLS: &[&str] = &["rfq:bebop", "rfq:hashflow", "rfq:liquorice"];
pub(crate) const BASE_NATIVE_PROTOCOLS: &[&str] = &[
    "uniswap_v2",
    "uniswap_v3",
    "uniswap_v4",
    "pancakeswap_v3",
    "aerodrome_slipstreams",
];
pub(crate) const BASE_VM_PROTOCOLS: &[&str] = &[];
pub(crate) const BASE_RFQ_PROTOCOLS: &[&str] = &["rfq:bebop", "rfq:hashflow"];

/// Get the default Bebop URL for the given chain.
pub fn get_default_bebop_url(chain: &Chain) -> Option<String> {
    match chain {
        Chain::Ethereum => Some("https://api.bebop.xyz/pmm/ethereum/v3/tokens".to_string()),
        Chain::Base => Some("https://api.bebop.xyz/pmm/base/v3/tokens".to_string()),
        _ => None,
    }
}

/// Get the default Hashflow Filename for the given chain.
pub fn get_default_hashflow_filename(chain: &Chain) -> Option<String> {
    match chain {
        Chain::Ethereum | Chain::Base => Some("./hashflow_supported_tokens.csv".to_string()),
        _ => None,
    }
}

/// Get the default Liquorice filename for the given chain.
pub fn get_default_liquorice_filename(chain: &Chain) -> Option<String> {
    match chain {
        Chain::Ethereum => Some("./liquorice_supported_tokens.csv".to_string()),
        _ => None,
    }
}

fn profile_protocols(protocols: &[&str]) -> Vec<String> {
    protocols
        .iter()
        .map(|protocol| (*protocol).to_string())
        .collect()
}

fn ethereum_profile() -> ChainProfile {
    let mut reset_tokens = HashMap::new();
    let mut mainnet = HashSet::new();
    mainnet.insert(parse_address(ETHEREUM_USDT));
    reset_tokens.insert(ETHEREUM_CHAIN_ID, mainnet);

    ChainProfile {
        chain: Chain::Ethereum,
        native_protocols: profile_protocols(ETHEREUM_NATIVE_PROTOCOLS),
        vm_protocols: profile_protocols(ETHEREUM_VM_PROTOCOLS),
        rfq_protocols: profile_protocols(ETHEREUM_RFQ_PROTOCOLS),
        native_token_protocol_allowlist: vec!["rocketpool".into()],
        reset_allowance_tokens: reset_tokens,
    }
}

fn base_profile() -> ChainProfile {
    ChainProfile {
        chain: Chain::Base,
        native_protocols: profile_protocols(BASE_NATIVE_PROTOCOLS),
        vm_protocols: profile_protocols(BASE_VM_PROTOCOLS),
        rfq_protocols: profile_protocols(BASE_RFQ_PROTOCOLS),
        native_token_protocol_allowlist: vec![],
        reset_allowance_tokens: HashMap::new(),
    }
}

fn resolve_chain_profile(chain_id: u64) -> Result<ChainProfile, String> {
    match chain_id {
        1 => Ok(ethereum_profile()),
        8453 => Ok(base_profile()),
        other => Err(format!(
            "Unsupported CHAIN_ID={}: supported values are 1 (Ethereum), 8453 (Base)",
            other
        )),
    }
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
    let chain_profile = match resolve_chain_profile(chain_id) {
        Ok(profile) => profile,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let network = load_network_config();
    let timeouts = load_timeout_config();
    let concurrency = load_concurrency_config();
    let stream = load_stream_config();
    let slippage = load_slippage_config();
    let reset_allowance_tokens = Arc::new(chain_profile.reset_allowance_tokens.clone());
    let memory = MemoryConfig::from_env();
    let rfq_enabled = rfq_effectively_enabled(network.enable_rfq_pools, &chain_profile);
    let (bebop_user, bebop_key, hashflow_user, hashflow_key, liquorice_user, liquorice_key) =
        load_rfq_credentials(rfq_enabled, &chain_profile.rfq_protocols);

    AppConfig {
        chain_profile,
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
        liquorice_key,
        liquorice_user,
    }
}

fn rfq_effectively_enabled(enable_rfq_pools: bool, chain_profile: &ChainProfile) -> bool {
    enable_rfq_pools && !chain_profile.rfq_protocols.is_empty()
}

fn load_rfq_credentials(
    rfq_enabled: bool,
    rfq_protocols: &[String],
) -> (String, String, String, String, String, String) {
    match try_load_rfq_credentials(rfq_enabled, rfq_protocols) {
        Ok(credentials) => credentials,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    }
}

fn try_load_rfq_credentials(
    rfq_enabled: bool,
    rfq_protocols: &[String],
) -> Result<(String, String, String, String, String, String), String> {
    if !rfq_enabled || rfq_protocols.is_empty() {
        return Ok((
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ));
    }

    let (bebop_user, bebop_key) = load_provider_credentials(
        rfq_protocol_enabled(rfq_protocols, "rfq:bebop"),
        "BEBOP_USER",
        "BEBOP_KEY",
    )?;
    let (hashflow_user, hashflow_key) = load_provider_credentials(
        rfq_protocol_enabled(rfq_protocols, "rfq:hashflow"),
        "HASHFLOW_USER",
        "HASHFLOW_KEY",
    )?;
    let (liquorice_user, liquorice_key) = load_provider_credentials(
        rfq_protocol_enabled(rfq_protocols, "rfq:liquorice"),
        "LIQUORICE_USER",
        "LIQUORICE_KEY",
    )?;

    Ok((
        bebop_user,
        bebop_key,
        hashflow_user,
        hashflow_key,
        liquorice_user,
        liquorice_key,
    ))
}

fn load_provider_credentials(
    enabled: bool,
    user_key: &str,
    auth_key: &str,
) -> Result<(String, String), String> {
    if !enabled {
        return Ok((String::new(), String::new()));
    }

    let user = env::var(user_key).map_err(|message| message.to_string())?;
    let key = env::var(auth_key).map_err(|message| message.to_string())?;
    Ok((user, key))
}

fn rfq_protocol_enabled(protocols: &[String], protocol: &str) -> bool {
    protocols.iter().any(|configured| configured == protocol)
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

/// Resolve the hosted Tycho endpoint for a supported runtime chain.
pub fn hosted_tycho_url(chain: Chain) -> Result<String, String> {
    get_default_url(&chain)
        .ok_or_else(|| format!("No default Tycho URL configured for supported chain {chain}"))
}

/// Resolve the hosted Bebop endpoint for a supported runtime chain.
pub fn hosted_bebop_url(chain: Chain) -> Result<String, String> {
    get_default_bebop_url(&chain)
        .ok_or_else(|| format!("No default Bebop URL configured for supported chain {chain}"))
}

/// Resolve the hosted Hashflow endpoint for a supported runtime chain.
pub fn hosted_hashflow_filename(chain: Chain) -> Result<String, String> {
    if let Some(filename) = optional_trimmed_env("HASHFLOW_FILENAME_CSV") {
        return Ok(filename);
    }

    get_default_hashflow_filename(&chain).ok_or_else(|| {
        format!("No default Hashflow filename configured for supported chain {chain}")
    })
}

/// Resolve the hosted Liquorice token filename for a supported runtime chain.
pub fn hosted_liquorice_filename(chain: Chain) -> Result<String, String> {
    if let Some(filename) = optional_trimmed_env("LIQUORICE_FILENAME_CSV") {
        return Ok(filename);
    }

    get_default_liquorice_filename(&chain).ok_or_else(|| {
        format!("No default Liquorice filename configured for supported chain {chain}")
    })
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
    pub liquorice_user: String,
    pub liquorice_key: String,
}

const ETHEREUM_CHAIN_ID: u64 = 1;
const ETHEREUM_USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";

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

    fn clear_slippage_env() {
        for key in SLIPPAGE_ENV_KEYS {
            std::env::remove_var(key);
        }
    }

    fn clear_rfq_env() {
        for key in [
            "BEBOP_USER",
            "BEBOP_KEY",
            "HASHFLOW_USER",
            "HASHFLOW_KEY",
            "LIQUORICE_USER",
            "LIQUORICE_KEY",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn resolve_ethereum_profile() {
        let Ok(profile) = resolve_chain_profile(1) else {
            unreachable!("expected ethereum profile");
        };
        assert_eq!(profile.chain, Chain::Ethereum);
        assert!(profile.native_protocols.contains(&"uniswap_v2".to_string()));
        assert!(profile.native_protocols.contains(&"rocketpool".to_string()));
        assert!(profile.native_protocols.contains(&"erc4626".to_string()));
        assert!(profile.vm_protocols.contains(&"vm:curve".to_string()));
        assert!(profile.rfq_protocols.contains(&"rfq:bebop".to_string()));
        assert!(profile.rfq_protocols.contains(&"rfq:liquorice".to_string()));
        assert!(profile
            .native_token_protocol_allowlist
            .contains(&"rocketpool".to_string()));
        assert!(profile.reset_allowance_tokens.contains_key(&1));
    }

    #[test]
    fn resolve_base_profile() {
        let Ok(profile) = resolve_chain_profile(8453) else {
            unreachable!("expected base profile");
        };
        assert_eq!(profile.chain, Chain::Base);
        assert!(profile.native_protocols.contains(&"uniswap_v2".to_string()));
        assert!(profile.native_protocols.contains(&"uniswap_v3".to_string()));
        assert!(profile.native_protocols.contains(&"uniswap_v4".to_string()));
        assert!(profile
            .native_protocols
            .contains(&"pancakeswap_v3".to_string()));
        assert!(profile
            .native_protocols
            .contains(&"aerodrome_slipstreams".to_string()));
        assert_eq!(profile.native_protocols.len(), 5);
        assert!(profile.vm_protocols.is_empty());
        assert!(profile.rfq_protocols.contains(&"rfq:bebop".to_string()));
        assert!(profile.rfq_protocols.contains(&"rfq:hashflow".to_string()));
        assert!(profile.native_token_protocol_allowlist.is_empty());
        assert!(profile.reset_allowance_tokens.is_empty());
    }

    #[test]
    fn resolve_unsupported_chain_errors() {
        let Err(err) = resolve_chain_profile(999) else {
            unreachable!("expected unsupported chain to error");
        };
        assert!(err.contains("Unsupported CHAIN_ID=999"));
    }

    #[test]
    fn hosted_tycho_url_uses_ethereum_default() {
        let Ok(url) = hosted_tycho_url(Chain::Ethereum) else {
            unreachable!("expected ethereum hosted Tycho URL");
        };
        assert_eq!(url, "tycho-beta.propellerheads.xyz");
    }

    #[test]
    fn hosted_tycho_url_uses_base_default() {
        let Ok(url) = hosted_tycho_url(Chain::Base) else {
            unreachable!("expected base hosted Tycho URL");
        };
        assert_eq!(url, "tycho-base-beta.propellerheads.xyz");
    }

    #[test]
    fn hosted_bebop_url_uses_ethereum_default() {
        let Ok(url) = hosted_bebop_url(Chain::Ethereum) else {
            unreachable!("expected ethereum hosted Bebop URL");
        };
        assert_eq!(url, "https://api.bebop.xyz/pmm/ethereum/v3/tokens");
    }

    #[test]
    fn hosted_bebop_url_uses_base_default() {
        let Ok(url) = hosted_bebop_url(Chain::Base) else {
            unreachable!("expected base hosted Bebop URL");
        };
        assert_eq!(url, "https://api.bebop.xyz/pmm/base/v3/tokens");
    }

    #[test]
    fn hosted_hashflow_filename_uses_ethereum_default() {
        let Ok(filename) = hosted_hashflow_filename(Chain::Ethereum) else {
            unreachable!("expected ethereum hosted Hashflow filename");
        };
        assert_eq!(filename, "./hashflow_supported_tokens.csv");
    }

    #[test]
    fn hosted_hashflow_filename_uses_base_default() {
        let Ok(filename) = hosted_hashflow_filename(Chain::Base) else {
            unreachable!("expected base hosted Hashflow filename");
        };
        assert_eq!(filename, "./hashflow_supported_tokens.csv");
    }

    #[test]
    fn hosted_hashflow_filename_prefers_env_override() {
        let _guard = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("HASHFLOW_FILENAME_CSV", "/tmp/hashflow.csv");

        let Ok(filename) = hosted_hashflow_filename(Chain::Base) else {
            unreachable!("expected base hosted Hashflow filename");
        };

        std::env::remove_var("HASHFLOW_FILENAME_CSV");
        assert_eq!(filename, "/tmp/hashflow.csv");
    }

    #[test]
    fn hosted_liquorice_filename_uses_ethereum_default() {
        let Ok(filename) = hosted_liquorice_filename(Chain::Ethereum) else {
            unreachable!("expected ethereum hosted Liquorice filename");
        };
        assert_eq!(filename, "./liquorice_supported_tokens.csv");
    }

    #[test]
    fn hosted_liquorice_filename_prefers_env_override() {
        let _guard = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("LIQUORICE_FILENAME_CSV", "/tmp/liquorice.csv");

        let Ok(filename) = hosted_liquorice_filename(Chain::Ethereum) else {
            unreachable!("expected ethereum hosted Liquorice filename");
        };

        std::env::remove_var("LIQUORICE_FILENAME_CSV");
        assert_eq!(filename, "/tmp/liquorice.csv");
    }

    #[test]
    fn rfq_effectively_enabled_requires_flag_and_protocols() {
        let ethereum = ethereum_profile();
        let base = base_profile();

        assert!(rfq_effectively_enabled(true, &ethereum));
        assert!(!rfq_effectively_enabled(false, &ethereum));
        assert!(rfq_effectively_enabled(true, &base));
        assert!(!rfq_effectively_enabled(false, &base));
    }

    #[test]
    fn load_rfq_credentials_skips_env_when_disabled() {
        let _guard = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (bebop_user, bebop_key, hashflow_user, hashflow_key, liquorice_user, liquorice_key) =
            load_rfq_credentials(false, &[]);

        assert!(bebop_user.is_empty());
        assert!(bebop_key.is_empty());
        assert!(hashflow_user.is_empty());
        assert!(hashflow_key.is_empty());
        assert!(liquorice_user.is_empty());
        assert!(liquorice_key.is_empty());
    }

    #[test]
    fn load_rfq_credentials_only_reads_enabled_protocols() -> Result<(), String> {
        let _guard = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_rfq_env();
        std::env::set_var("HASHFLOW_USER", "hashflow-user");
        std::env::set_var("HASHFLOW_KEY", "hashflow-key");

        let protocols = vec!["rfq:hashflow".to_string()];
        let (bebop_user, bebop_key, hashflow_user, hashflow_key, liquorice_user, liquorice_key) =
            try_load_rfq_credentials(true, &protocols)?;

        clear_rfq_env();
        assert!(bebop_user.is_empty());
        assert!(bebop_key.is_empty());
        assert_eq!(hashflow_user, "hashflow-user");
        assert_eq!(hashflow_key, "hashflow-key");
        assert!(liquorice_user.is_empty());
        assert!(liquorice_key.is_empty());
        Ok(())
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
}
