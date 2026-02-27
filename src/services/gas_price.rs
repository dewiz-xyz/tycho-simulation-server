use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{error, info, warn};

use crate::models::state::AppState;

const ETH_GAS_PRICE_TIMEOUT: Duration = Duration::from_secs(3);
const DISABLED_REMINDER_EVERY_FAILURES: u64 = 10;

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
        .timeout(ETH_GAS_PRICE_TIMEOUT)
        .json(&request)
        .send()
        .await
        .map_err(|err| anyhow!("failed to call eth_gasPrice: {}", err.without_url()))?;

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

pub async fn run_native_gas_price_refresh_loop(
    app_state: AppState,
    rpc_url: String,
    refresh_interval: Duration,
    failure_tolerance: u64,
    client: Client,
) {
    let mut consecutive_failures = 0_u64;
    let mut disabled_failure_counter = 0_u64;

    process_refresh_result(
        &app_state,
        failure_tolerance,
        &mut consecutive_failures,
        &mut disabled_failure_counter,
        fetch_eth_gas_price_wei(&rpc_url, &client).await,
    )
    .await;

    let mut ticker = interval(refresh_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // Skip immediate first tick because we already did an eager refresh above.
    ticker.tick().await;

    loop {
        ticker.tick().await;
        process_refresh_result(
            &app_state,
            failure_tolerance,
            &mut consecutive_failures,
            &mut disabled_failure_counter,
            fetch_eth_gas_price_wei(&rpc_url, &client).await,
        )
        .await;
    }
}

async fn process_refresh_result(
    app_state: &AppState,
    failure_tolerance: u64,
    consecutive_failures: &mut u64,
    disabled_failure_counter: &mut u64,
    result: Result<u128>,
) {
    match result {
        Ok(value) => {
            app_state.set_latest_native_gas_price_wei(Some(value)).await;
            let was_disabled = !app_state.native_gas_price_reporting_enabled().await;
            app_state.set_native_gas_price_reporting_enabled(true).await;

            let recovered_from_failures = *consecutive_failures > 0;
            *consecutive_failures = 0;
            *disabled_failure_counter = 0;

            if was_disabled {
                info!(
                    event = "native_gas_price_reporting_reenabled",
                    gas_price_wei = value.to_string(),
                    "Native gas price reporting re-enabled after successful RPC refresh"
                );
            } else if recovered_from_failures {
                info!(
                    event = "native_gas_price_refresh_recovered",
                    gas_price_wei = value.to_string(),
                    "Native gas price refresh recovered after transient RPC failures"
                );
            }
        }
        Err(err) => {
            *consecutive_failures = consecutive_failures.saturating_add(1);
            let exceeded_tolerance = *consecutive_failures > failure_tolerance;

            warn!(
                event = "native_gas_price_refresh_failed",
                error = %err,
                consecutive_failures = *consecutive_failures,
                failure_tolerance,
                exceeded_tolerance,
                "Failed to refresh native gas price via RPC"
            );

            if exceeded_tolerance {
                let reporting_enabled = app_state.native_gas_price_reporting_enabled().await;
                if reporting_enabled {
                    app_state
                        .set_native_gas_price_reporting_enabled(false)
                        .await;
                    *disabled_failure_counter = 0;
                    error!(
                        event = "native_gas_price_failure_tolerance_exceeded",
                        consecutive_failures = *consecutive_failures,
                        failure_tolerance,
                        "gas_in_sell reporting disabled due to consecutive RPC failures"
                    );
                    return;
                }

                *disabled_failure_counter = disabled_failure_counter.saturating_add(1);
                if *disabled_failure_counter % DISABLED_REMINDER_EVERY_FAILURES == 0 {
                    warn!(
                        event = "native_gas_price_reporting_still_disabled",
                        consecutive_failures = *consecutive_failures,
                        failure_tolerance,
                        reminder_every_failures = DISABLED_REMINDER_EVERY_FAILURES,
                        "gas_in_sell reporting remains disabled while RPC failures continue"
                    );
                }
            }
        }
    }
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
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::anyhow;
    use tokio::sync::{RwLock, Semaphore};
    use tycho_simulation::tycho_common::models::Chain;

    use crate::models::{
        state::{AppState, StateStore, VmStreamStatus},
        stream_health::StreamHealth,
        tokens::TokenStore,
    };

    use super::{parse_hex_u128, process_refresh_result};

    fn build_test_app_state() -> AppState {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_millis(10),
        ));

        AppState {
            chain: Chain::Ethereum,
            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
            tokens: Arc::clone(&token_store),
            native_state_store: Arc::new(StateStore::new(Arc::clone(&token_store))),
            vm_state_store: Arc::new(StateStore::new(token_store)),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(RwLock::new(false)),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(100),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_millis(1000),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        }
    }

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

    #[tokio::test]
    async fn refresh_success_sets_cache_and_enables_reporting() {
        let app_state = build_test_app_state();
        let mut consecutive_failures = 0_u64;
        let mut disabled_failures = 0_u64;

        process_refresh_result(
            &app_state,
            50,
            &mut consecutive_failures,
            &mut disabled_failures,
            Ok(123),
        )
        .await;

        assert_eq!(app_state.latest_native_gas_price_wei().await, Some(123));
        assert!(app_state.native_gas_price_reporting_enabled().await);
        assert_eq!(consecutive_failures, 0);
        assert_eq!(disabled_failures, 0);
    }

    #[tokio::test]
    async fn refresh_failures_at_or_below_tolerance_keep_reporting_enabled() {
        let app_state = build_test_app_state();
        app_state.set_latest_native_gas_price_wei(Some(999)).await;
        app_state.set_native_gas_price_reporting_enabled(true).await;

        let mut consecutive_failures = 49_u64;
        let mut disabled_failures = 0_u64;

        process_refresh_result(
            &app_state,
            50,
            &mut consecutive_failures,
            &mut disabled_failures,
            Err(anyhow!("temporary timeout")),
        )
        .await;

        assert_eq!(consecutive_failures, 50);
        assert!(app_state.native_gas_price_reporting_enabled().await);
        assert_eq!(app_state.latest_native_gas_price_wei().await, Some(999));
    }

    #[tokio::test]
    async fn refresh_failure_above_tolerance_disables_reporting() {
        let app_state = build_test_app_state();
        app_state.set_latest_native_gas_price_wei(Some(999)).await;
        app_state.set_native_gas_price_reporting_enabled(true).await;

        let mut consecutive_failures = 50_u64;
        let mut disabled_failures = 0_u64;

        process_refresh_result(
            &app_state,
            50,
            &mut consecutive_failures,
            &mut disabled_failures,
            Err(anyhow!("rpc unavailable")),
        )
        .await;

        assert_eq!(consecutive_failures, 51);
        assert!(!app_state.native_gas_price_reporting_enabled().await);
        assert_eq!(app_state.latest_native_gas_price_wei().await, Some(999));
    }

    #[tokio::test]
    async fn refresh_success_after_disable_resets_counter_and_reenables_reporting() {
        let app_state = build_test_app_state();
        app_state.set_latest_native_gas_price_wei(Some(100)).await;
        app_state
            .set_native_gas_price_reporting_enabled(false)
            .await;

        let mut consecutive_failures = 90_u64;
        let mut disabled_failures = 7_u64;

        process_refresh_result(
            &app_state,
            50,
            &mut consecutive_failures,
            &mut disabled_failures,
            Ok(321),
        )
        .await;

        assert_eq!(app_state.latest_native_gas_price_wei().await, Some(321));
        assert!(app_state.native_gas_price_reporting_enabled().await);
        assert_eq!(consecutive_failures, 0);
        assert_eq!(disabled_failures, 0);
    }
}
