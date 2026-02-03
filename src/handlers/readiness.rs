use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;

use crate::models::state::AppState;

#[derive(Serialize)]
pub struct StatusPayload {
    status: &'static str,
    block: u64,
    pools: usize,
    vm_enabled: bool,
    vm_status: &'static str,
    vm_block: u64,
    vm_pools: usize,
    vm_restarts: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    vm_last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vm_rebuild_duration_ms: Option<u128>,
}

pub async fn status(State(state): State<AppState>) -> (StatusCode, Json<StatusPayload>) {
    let block = state.current_block().await;
    let pools = state.total_pools().await;

    let vm_enabled = state.enable_vm_pools;
    let (vm_status, vm_block, vm_pools, vm_restarts, vm_last_error, vm_rebuild_duration_ms) =
        if vm_enabled {
            let status = if state.vm_rebuilding() {
                "rebuilding"
            } else if state.vm_ready() {
                "ready"
            } else {
                "warming_up"
            };
            let vm_block = state.vm_block().await;
            let vm_pools = state.vm_pools().await;
            let restarts = state.vm_stream.restart_count();
            let last_error = state.vm_stream.last_error().await;
            let rebuild_duration_ms = state.vm_stream.rebuild_duration_ms().await;
            (
                status,
                vm_block,
                vm_pools,
                restarts,
                last_error,
                rebuild_duration_ms,
            )
        } else {
            ("disabled", 0, 0, 0, None, None)
        };

    if state.is_ready() {
        (
            StatusCode::OK,
            Json(StatusPayload {
                status: "ready",
                block,
                pools,
                vm_enabled,
                vm_status,
                vm_block,
                vm_pools,
                vm_restarts,
                vm_last_error,
                vm_rebuild_duration_ms,
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(StatusPayload {
                status: "warming_up",
                block,
                pools,
                vm_enabled,
                vm_status,
                vm_block,
                vm_pools,
                vm_restarts,
                vm_last_error,
                vm_rebuild_duration_ms,
            }),
        )
    }
}
