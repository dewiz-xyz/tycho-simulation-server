use num_bigint::BigUint;
use num_traits::Zero;
use tracing::info;

use crate::models::messages::{ResimulationDebug, RouteDebug, RouteEncodeRequest};
use crate::models::state::AppState;

use super::model::ResimulatedRouteInternal;
use super::wire::format_address;

pub(super) fn compute_expected_total(resimulated: &ResimulatedRouteInternal) -> BigUint {
    let mut expected_total = BigUint::zero();
    for segment in &resimulated.segments {
        expected_total += segment.expected_amount_out.clone();
    }
    expected_total
}

pub(super) fn log_resimulation_amounts(
    request_id: Option<&str>,
    resimulated: &ResimulatedRouteInternal,
) {
    for (segment_index, segment) in resimulated.segments.iter().enumerate() {
        info!(
            request_id,
            segment_index,
            share_bps = segment.share_bps,
            amount_in = %segment.amount_in,
            expected_amount_out = %segment.expected_amount_out,
            "Resimulated segment"
        );

        for (hop_index, hop) in segment.hops.iter().enumerate() {
            info!(
                request_id,
                segment_index,
                hop_index,
                token_in = %format_address(&hop.token_in),
                token_out = %format_address(&hop.token_out),
                amount_in = %hop.amount_in,
                expected_amount_out = %hop.expected_amount_out,
                "Resimulated hop"
            );

            for (swap_index, swap) in hop.swaps.iter().enumerate() {
                info!(
                    request_id,
                    segment_index,
                    hop_index,
                    swap_index,
                    split_bps = swap.split_bps,
                    pool_id = %swap.pool.component_id,
                    amount_in = %swap.amount_in,
                    expected_amount_out = %swap.expected_amount_out,
                    "Resimulated swap"
                );
            }
        }
    }
}

pub(super) async fn build_debug(
    state: &AppState,
    request: &RouteEncodeRequest,
) -> Option<RouteDebug> {
    let request_id = request.request_id.clone()?;
    let block_number = state.current_block().await;
    Some(RouteDebug {
        request_id: Some(request_id),
        resimulation: Some(ResimulationDebug {
            block_number: Some(block_number),
            tycho_state_tag: None,
        }),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use tokio::sync::Semaphore;
    use tycho_simulation::protocol::models::Update;
    use tycho_simulation::tycho_common::models::Chain;

    use super::*;
    use crate::models::messages::SwapKind;
    use crate::models::state::{StateStore, VmStreamStatus};
    use crate::models::stream_health::StreamHealth;
    use crate::models::tokens::TokenStore;

    #[tokio::test]
    async fn build_debug_omits_when_request_id_missing() {
        let tokens_store = std::sync::Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_millis(10),
        ));
        let native_state_store =
            std::sync::Arc::new(StateStore::new(std::sync::Arc::clone(&tokens_store)));
        let vm_state_store =
            std::sync::Arc::new(StateStore::new(std::sync::Arc::clone(&tokens_store)));

        native_state_store
            .apply_update(Update::new(42, HashMap::new(), HashMap::new()))
            .await;

        let state = AppState {
            tokens: std::sync::Arc::clone(&tokens_store),
            native_state_store: std::sync::Arc::clone(&native_state_store),
            vm_state_store: std::sync::Arc::clone(&vm_state_store),
            native_stream_health: std::sync::Arc::new(StreamHealth::new()),
            vm_stream_health: std::sync::Arc::new(StreamHealth::new()),
            vm_stream: std::sync::Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(10),
            pool_timeout_native: Duration::from_millis(10),
            pool_timeout_vm: Duration::from_millis(10),
            request_timeout: Duration::from_millis(10),
            native_sim_semaphore: std::sync::Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: std::sync::Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: std::sync::Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "10".to_string(),
            min_amount_out: "8".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: "0x0000000000000000000000000000000000000004".to_string(),
            swap_kind: SwapKind::SimpleSwap,
            segments: Vec::new(),
            request_id: None,
        };

        assert!(build_debug(&state, &request).await.is_none());
    }

    #[tokio::test]
    async fn build_debug_includes_block_when_request_id_present() {
        let tokens_store = std::sync::Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_millis(10),
        ));
        let native_state_store =
            std::sync::Arc::new(StateStore::new(std::sync::Arc::clone(&tokens_store)));
        let vm_state_store =
            std::sync::Arc::new(StateStore::new(std::sync::Arc::clone(&tokens_store)));

        native_state_store
            .apply_update(Update::new(42, HashMap::new(), HashMap::new()))
            .await;

        let state = AppState {
            tokens: std::sync::Arc::clone(&tokens_store),
            native_state_store: std::sync::Arc::clone(&native_state_store),
            vm_state_store: std::sync::Arc::clone(&vm_state_store),
            native_stream_health: std::sync::Arc::new(StreamHealth::new()),
            vm_stream_health: std::sync::Arc::new(StreamHealth::new()),
            vm_stream: std::sync::Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(10),
            pool_timeout_native: Duration::from_millis(10),
            pool_timeout_vm: Duration::from_millis(10),
            request_timeout: Duration::from_millis(10),
            native_sim_semaphore: std::sync::Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: std::sync::Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: std::sync::Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let request = RouteEncodeRequest {
            chain_id: 1,
            token_in: "0x0000000000000000000000000000000000000001".to_string(),
            token_out: "0x0000000000000000000000000000000000000002".to_string(),
            amount_in: "10".to_string(),
            min_amount_out: "8".to_string(),
            settlement_address: "0x0000000000000000000000000000000000000003".to_string(),
            tycho_router_address: "0x0000000000000000000000000000000000000004".to_string(),
            swap_kind: SwapKind::SimpleSwap,
            segments: Vec::new(),
            request_id: Some("req-1".to_string()),
        };

        let debug = build_debug(&state, &request).await.expect("debug present");
        assert_eq!(debug.request_id.as_deref(), Some("req-1"));
        assert_eq!(
            debug
                .resimulation
                .as_ref()
                .and_then(|resim| resim.block_number),
            Some(42)
        );
    }
}
