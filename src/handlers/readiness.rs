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
    vm_rebuild_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vm_last_update_age_ms: Option<u64>,
}

pub async fn status(State(state): State<AppState>) -> (StatusCode, Json<StatusPayload>) {
    let block = state.current_block().await;
    let pools = state.total_pools().await;
    let native_ready = state.is_ready();
    let native_update_age_ms = state.native_stream_health.last_update_age_ms().await;
    let readiness_stale_ms = state.readiness_stale.as_millis() as u64;
    let native_stale = native_update_age_ms
        .map(|age| age >= readiness_stale_ms)
        .unwrap_or(false);

    let vm_enabled = state.enable_vm_pools;
    let vm_status_snapshot = state.vm_stream.read().await.clone();
    let vm_rebuilding = vm_status_snapshot.rebuilding;
    let vm_ready = vm_enabled && state.vm_state_store.is_ready() && !vm_rebuilding;
    let vm_status = if !vm_enabled {
        "disabled"
    } else if vm_rebuilding {
        "rebuilding"
    } else if vm_ready {
        "ready"
    } else {
        "warming_up"
    };

    let vm_block = state.vm_block().await;
    let vm_pools = state.vm_pools().await;
    let vm_restarts = vm_status_snapshot.restart_count;
    let vm_last_error = vm_status_snapshot.last_error.clone();
    let vm_rebuild_duration_ms = if vm_rebuilding {
        vm_status_snapshot
            .rebuild_started_at
            .map(|instant| instant.elapsed().as_millis() as u64)
    } else {
        None
    };
    let vm_last_update_age_ms = state.vm_stream_health.last_update_age_ms().await;

    let status = if native_ready && !native_stale {
        "ready"
    } else {
        "warming_up"
    };

    let status_code = if status == "ready" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status_code,
        Json(StatusPayload {
            status,
            block,
            pools,
            vm_enabled,
            vm_status,
            vm_block,
            vm_pools,
            vm_restarts,
            vm_last_error,
            vm_rebuild_duration_ms,
            vm_last_update_age_ms,
        }),
    )
}
