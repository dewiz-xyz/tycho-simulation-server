use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;

use crate::models::state::AppState;

#[derive(Serialize)]
struct ReadinessPayload {
    status: &'static str,
    block: u64,
    pools: usize,
}

pub async fn readiness(State(state): State<AppState>) -> (StatusCode, Json<ReadinessPayload>) {
    let block = state.current_block().await;
    let pools = state.total_pools().await;
    if state.is_ready() {
        (
            StatusCode::OK,
            Json(ReadinessPayload {
                status: "ready",
                block,
                pools,
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadinessPayload {
                status: "warming_up",
                block,
                pools,
            }),
        )
    }
}
