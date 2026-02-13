use axum::{
    error_handling::HandleErrorLayer,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use std::time::Duration;
use tower::{
    timeout::{error::Elapsed, TimeoutLayer},
    BoxError,
};
use tracing::warn;

use crate::handlers::{pools::pools, quote::simulate, readiness::status};
use crate::metrics::{emit_simulate_completion, emit_simulate_timeout, TimeoutKind};
use crate::models::factories::router_timeout_result;
use crate::models::messages::QuoteStatus;
use crate::models::state::AppState;

pub fn create_router(app_state: AppState) -> Router {
    let request_timeout = app_state.request_timeout();
    let router_timeout = request_timeout + Duration::from_millis(250);
    let router_timeout_ms = router_timeout.as_millis() as u64;

    Router::new()
        .route(
            "/simulate",
            post(simulate)
                .layer(TimeoutLayer::new(router_timeout))
                .layer(HandleErrorLayer::new({
                    let timeout_ms = router_timeout_ms;
                    move |err: BoxError| async move { handle_timeout_error(err, timeout_ms).await }
                })),
        )
        .route("/status", get(status))
        .route("/pools", get(pools))
        .with_state(app_state)
}

async fn handle_timeout_error(err: BoxError, timeout_ms: u64) -> Response {
    if err.is::<Elapsed>() {
        warn!(
            scope = "router_timeout",
            timeout_ms, "Request-level timeout triggered at router boundary: {}", err
        );
        emit_simulate_completion(QuoteStatus::PartialFailure, true);
        emit_simulate_timeout(TimeoutKind::RouterBoundary);
        return (StatusCode::OK, Json(router_timeout_result())).into_response();
    }

    warn!(scope = "router_timeout", "Unhandled service error: {}", err);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}
