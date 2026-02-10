use std::time::Instant;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use tracing::{info, warn};

use crate::models::messages::{EncodeErrorResponse, RouteEncodeRequest, RouteEncodeResponse};
use crate::models::state::AppState;
use crate::services::encode::{encode_route, EncodeErrorKind};

pub async fn encode(
    State(state): State<AppState>,
    Json(request): Json<RouteEncodeRequest>,
) -> Response {
    let started_at = Instant::now();
    let request_id = request.request_id.as_deref();

    info!(
        request_id,
        segments = request.segments.len(),
        "Received encode request"
    );

    let request_timeout = state.request_timeout();
    let state_for_computation = state.clone();
    let request_for_computation = request.clone();

    let computation_future = encode_route(state_for_computation, request_for_computation);

    let computation = match tokio::time::timeout(request_timeout, computation_future).await {
        Ok(result) => result,
        Err(_) => {
            let timeout_ms = request_timeout.as_millis() as u64;
            warn!(
                scope = "handler_timeout",
                request_id,
                timeout_ms,
                latency_ms = started_at.elapsed().as_millis() as u64,
                "Encode request timed out at request-level guard"
            );

            let body = Json(EncodeErrorResponse {
                error: format!("Encode request timed out after {}ms", timeout_ms),
                request_id: request.request_id.clone(),
            });
            return (StatusCode::REQUEST_TIMEOUT, body).into_response();
        }
    };

    let latency_ms = started_at.elapsed().as_millis() as u64;
    match computation {
        Ok(response) => {
            info!(
                request_id,
                latency_ms,
                interactions = response.interactions.len(),
                "Encode request completed"
            );
            Json::<RouteEncodeResponse>(response).into_response()
        }
        Err(err) => {
            let status = err.status_code();
            let body = Json(EncodeErrorResponse {
                error: err.message().to_string(),
                request_id: request.request_id.clone(),
            });
            match err.kind() {
                EncodeErrorKind::InvalidRequest => {
                    warn!(
                        request_id,
                        latency_ms,
                        error = err.message(),
                        "Invalid encode request"
                    );
                }
                EncodeErrorKind::Simulation | EncodeErrorKind::Encoding => {
                    warn!(
                        request_id,
                        latency_ms,
                        error = err.message(),
                        "Encode failed"
                    );
                }
                EncodeErrorKind::Internal | EncodeErrorKind::NotFound => {
                    warn!(
                        request_id,
                        latency_ms,
                        error = err.message(),
                        "Encode failed with internal error"
                    );
                }
            }
            (status, body).into_response()
        }
    }
}
