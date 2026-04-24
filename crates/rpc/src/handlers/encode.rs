use std::time::Instant;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use crate::models::messages::{EncodeErrorResponse, RouteEncodeRequest, RouteEncodeResponse};
use crate::models::state::AppState;
use crate::services::encode::{
    encode_route, log_failure, log_handler_timeout, log_received, log_success,
};

pub async fn encode(
    State(state): State<AppState>,
    Json(request): Json<RouteEncodeRequest>,
) -> Response {
    let started_at = Instant::now();
    log_received(&request);

    let request_timeout = state.request_timeout();
    let state_for_computation = state.clone();
    let request_for_computation = request.clone();

    let computation_future = encode_route(state_for_computation, request_for_computation);

    let Ok(computation) = tokio::time::timeout(request_timeout, computation_future).await else {
        let timeout_ms = request_timeout.as_millis() as u64;
        log_handler_timeout(
            &request,
            timeout_ms,
            started_at.elapsed().as_millis() as u64,
        );

        let body = Json(EncodeErrorResponse {
            error: format!("Encode request timed out after {timeout_ms}ms"),
            request_id: request.request_id.clone(),
        });
        return (StatusCode::REQUEST_TIMEOUT, body).into_response();
    };

    let latency_ms = started_at.elapsed().as_millis() as u64;
    match computation {
        Ok(computation) => {
            log_success(&request, &computation, latency_ms);
            Json::<RouteEncodeResponse>(computation.response).into_response()
        }
        Err(err) => {
            let status = err.status_code();
            let body = Json(EncodeErrorResponse {
                error: err.message().to_string(),
                request_id: request.request_id.clone(),
            });
            log_failure(&request, status, err.kind(), err.message(), latency_ms);
            (status, body).into_response()
        }
    }
}
