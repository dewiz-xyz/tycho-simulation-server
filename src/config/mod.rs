mod logging;
pub use logging::init_logging;

pub fn load_config() -> AppConfig {
    dotenv::dotenv().ok();

    let tycho_url =
        std::env::var("TYCHO_URL").unwrap_or_else(|_| "tycho-beta.propellerheads.xyz".to_string());
    let api_key = std::env::var("TYCHO_API_KEY").expect("TYCHO_API_KEY must be set");
    let tvl_threshold: f64 = std::env::var("TVL_THRESHOLD")
        .unwrap_or_else(|_| "1000".to_string())
        .parse()
        .expect("Invalid TVL_THRESHOLD");
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "3000".to_string())
        .parse()
        .expect("Invalid PORT");
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());

    // Create socket address from host and port
    let addr = format!("{}", host);
    // Parse the address
    let _ = addr.parse::<std::net::IpAddr>().expect("Invalid HOST");

    AppConfig {
        tycho_url,
        api_key,
        tvl_threshold,
        port,
        host,
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub tycho_url: String,
    pub api_key: String,
    pub tvl_threshold: f64,
    pub port: u16,
    pub host: String,
}
