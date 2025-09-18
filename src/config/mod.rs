mod logging;
pub use logging::init_logging;

pub fn load_config() -> AppConfig {
    dotenv::dotenv().ok();

    let tycho_url =
        std::env::var("TYCHO_URL").unwrap_or_else(|_| "tycho-beta.propellerheads.xyz".to_string());
    let api_key = std::env::var("TYCHO_API_KEY").expect("TYCHO_API_KEY must be set");
    let tvl_threshold: f64 = std::env::var("TVL_THRESHOLD")
        .unwrap_or_else(|_| "300".to_string())
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
    let addr = format!("{}", host);
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
        .unwrap_or_else(|_| "50".to_string())
        .parse()
        .expect("Invalid QUOTE_TIMEOUT_MS");
    let pool_timeout_ms: u64 = std::env::var("POOL_TIMEOUT_MS")
        .unwrap_or_else(|_| "5".to_string())
        .parse()
        .expect("Invalid POOL_TIMEOUT_MS");

    assert!(quote_timeout_ms > 0, "QUOTE_TIMEOUT_MS must be > 0");
    assert!(pool_timeout_ms > 0, "POOL_TIMEOUT_MS must be > 0");

    AppConfig {
        tycho_url,
        api_key,
        tvl_threshold,
        tvl_keep_threshold,
        port,
        host,
        quote_timeout_ms,
        pool_timeout_ms,
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
    pub pool_timeout_ms: u64,
}
