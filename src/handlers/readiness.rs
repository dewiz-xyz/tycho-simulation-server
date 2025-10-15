use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;

use crate::models::state::AppState;

#[derive(Serialize)]
pub struct StatusPayload {
    status: &'static str,
    block: u64,
    pools: usize,
}

pub async fn status(State(state): State<AppState>) -> (StatusCode, Json<StatusPayload>) {
    let block = state.current_block().await;
    let pools = state.total_pools().await;
    if state.is_ready() {
        (
            StatusCode::OK,
            Json(StatusPayload {
                status: "ready",
                block,
                pools,
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(StatusPayload {
                status: "warming_up",
                block,
                pools,
            }),
        )
    }
}
