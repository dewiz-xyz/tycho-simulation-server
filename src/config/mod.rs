mod logging;
use std::env;

pub use logging::init_logging;

pub fn load_config() -> AppConfig {
    dotenv::dotenv().ok();

    let tycho_url =
        env::var("TYCHO_URL").unwrap_or_else(|_| "tycho-beta.propellerheads.xyz".to_string());
    let api_key = env::var("TYCHO_API_KEY").expect("TYCHO_API_KEY must be set");
    let tvl_threshold: f64 = env::var("TVL_THRESHOLD")
        .unwrap_or_else(|_| "100".to_string())
        .parse()
        .expect("Invalid TVL_THRESHOLD");
    // Keep/remove ratio: defaults to 20% of add threshold; clamp to <= add
    let tvl_keep_ratio: f64 = env::var("TVL_KEEP_RATIO")
        .unwrap_or_else(|_| "0.2".to_string())
        .parse()
        .expect("Invalid TVL_KEEP_RATIO");
    let tvl_keep_threshold: f64 = (tvl_threshold * tvl_keep_ratio).min(tvl_threshold);
    let port = env::var("PORT")
        .unwrap_or_else(|_| "3000".to_string())
        .parse()
        .expect("Invalid PORT");
    let host = env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());

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

    let quote_timeout_ms: u64 = env::var("QUOTE_TIMEOUT_MS")
        .unwrap_or_else(|_| "150".to_string())
        .parse()
        .expect("Invalid QUOTE_TIMEOUT_MS");
    let pool_timeout_native_ms: u64 = env::var("POOL_TIMEOUT_NATIVE_MS")
        .unwrap_or_else(|_| "20".to_string())
        .parse()
        .expect("Invalid POOL_TIMEOUT_NATIVE_MS");
    let pool_timeout_vm_ms: u64 = env::var("POOL_TIMEOUT_VM_MS")
        .unwrap_or_else(|_| "150".to_string())
        .parse()
        .expect("Invalid POOL_TIMEOUT_VM_MS");
    let pool_timeout_rfq_ms: u64 = env::var("POOL_TIMEOUT_RFQ_MS")
        .unwrap_or_else(|_| "150".to_string())
        .parse()
        .expect("Invalid POOL_TIMEOUT_RFQ_MS");

    let request_timeout_ms: u64 = env::var("REQUEST_TIMEOUT_MS")
        .unwrap_or_else(|_| "4000".to_string())
        .parse()
        .expect("Invalid REQUEST_TIMEOUT_MS");
    assert!(quote_timeout_ms > 0, "QUOTE_TIMEOUT_MS must be > 0");
    assert!(
        pool_timeout_native_ms > 0,
        "POOL_TIMEOUT_NATIVE_MS must be > 0"
    );
    assert!(pool_timeout_vm_ms > 0, "POOL_TIMEOUT_VM_MS must be > 0");
    assert!(pool_timeout_rfq_ms > 0, "POOL_TIMEOUT_RFQ_MS must be > 0");
    let token_refresh_timeout_ms: u64 = env::var("TOKEN_REFRESH_TIMEOUT_MS")
        .unwrap_or_else(|_| "1000".to_string())
        .parse()
        .expect("Invalid TOKEN_REFRESH_TIMEOUT_MS");
    assert!(
        token_refresh_timeout_ms > 0,
        "TOKEN_REFRESH_TIMEOUT_MS must be > 0"
    );

    let enable_vm_pools: bool = env::var("ENABLE_VM_POOLS")
        .unwrap_or_else(|_| "true".to_string())
        .parse()
        .expect("Invalid ENABLE_VM_POOLS");
    let enable_rfq_pools: bool = env::var("ENABLE_RFQ_POOLS")
        .unwrap_or_else(|_| "true".to_string())
        .parse()
        .expect("Invalid ENABLE_RFQ_POOLS");

    let cpu_count = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);

    let default_native = (cpu_count.saturating_mul(4)).max(1);
    let default_vm: usize = cpu_count.max(1);
    let default_rfq: usize = cpu_count.max(1);

    let global_native_sim_concurrency: usize = env::var("GLOBAL_NATIVE_SIM_CONCURRENCY")
        .ok()
        .map(|value| {
            value
                .parse()
                .expect("Invalid GLOBAL_NATIVE_SIM_CONCURRENCY")
        })
        .unwrap_or(default_native)
        .max(1);
    let global_vm_sim_concurrency: usize = env::var("GLOBAL_VM_SIM_CONCURRENCY")
        .ok()
        .map(|value| value.parse().expect("Invalid GLOBAL_VM_SIM_CONCURRENCY"))
        .unwrap_or(default_vm)
        .max(1);
    let global_rfq_sim_concurrency: usize = env::var("GLOBAL_RFQ_SIM_CONCURRENCY")
        .ok()
        .map(|value| value.parse().expect("Invalid GLOBAL_RFQ_SIM_CONCURRENCY"))
        .unwrap_or(default_rfq)
        .max(1);

    let (bebop_user, bebop_key) = (
        env::var("BEBOP_USER").expect("BEBOP_USER must be set"),
        env::var("BEBOP_KEY").expect("BEBOP_KEY must be set"),
    );
    let (hashflow_user, hashflow_key) = (
        env::var("HASHFLOW_USER").expect("HASHFLOW_USER must be set"),
        env::var("HASHFLOW_KEY").expect("HASHFLOW_KEY must be set"),
    );

    AppConfig {
        tycho_url,
        api_key,
        tvl_threshold,
        tvl_keep_threshold,
        port,
        host,
        quote_timeout_ms,
        pool_timeout_native_ms,
        pool_timeout_vm_ms,
        pool_timeout_rfq_ms,
        request_timeout_ms,
        token_refresh_timeout_ms,
        enable_vm_pools,
        enable_rfq_pools,
        global_native_sim_concurrency,
        global_vm_sim_concurrency,
        global_rfq_sim_concurrency,
        bebop_user,
        bebop_key,
        hashflow_user,
        hashflow_key,
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub tycho_url: String,
    pub api_key: String,
    pub tvl_threshold: f64,
    pub tvl_keep_threshold: f64,
    pub port: u16,
    pub host: String,
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
    pub bebop_user: String,
    pub bebop_key: String,
    pub hashflow_user: String,
    pub hashflow_key: String,
}
