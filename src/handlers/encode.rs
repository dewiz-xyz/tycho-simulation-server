use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::models::messages::{EncodeErrorResponse, RouteEncodeRequest, RouteEncodeResponse};
use crate::models::state::AppState;
use crate::services::encode::{encode_route, EncodeError, EncodeErrorKind};

enum EncodeComputation {
    Completed(Result<RouteEncodeResponse, EncodeError>),
    JoinFailed(tokio::task::JoinError),
    TimedOut,
}

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

    let request_cancel = CancellationToken::new();
    let computation_task = tokio::spawn({
        let request_cancel = request_cancel.clone();
        async move {
            encode_route(
                state_for_computation,
                request_for_computation,
                Some(request_cancel),
            )
            .await
        }
    });

    let computation =
        match await_encode_computation(computation_task, request_timeout, request_cancel).await {
            EncodeComputation::Completed(result) => result,
            EncodeComputation::JoinFailed(join_err) => {
                warn!(
                    scope = "handler_internal",
                    request_id,
                    latency_ms = started_at.elapsed().as_millis() as u64,
                    error = %join_err,
                    "Encode task failed"
                );
                let body = Json(EncodeErrorResponse {
                    error: "Encode task failed".to_string(),
                    request_id: request.request_id.clone(),
                });
                return (StatusCode::INTERNAL_SERVER_ERROR, body).into_response();
            }
            EncodeComputation::TimedOut => {
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

async fn await_encode_computation(
    computation_task: JoinHandle<Result<RouteEncodeResponse, EncodeError>>,
    request_timeout: Duration,
    request_cancel: CancellationToken,
) -> EncodeComputation {
    let timeout = tokio::time::sleep(request_timeout);
    tokio::pin!(timeout);
    tokio::pin!(computation_task);

    tokio::select! {
        result = computation_task.as_mut() => match result {
            Ok(result) => EncodeComputation::Completed(result),
            Err(join_err) => EncodeComputation::JoinFailed(join_err),
        },
        _ = &mut timeout => {
            // `tokio::time::timeout` would drop the JoinHandle here, which detaches the task.
            request_cancel.cancel();
            computation_task.as_mut().abort();
            EncodeComputation::TimedOut
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use super::{await_encode_computation, EncodeComputation};
    use crate::models::messages::RouteEncodeResponse;
    use crate::services::encode::EncodeError;

    #[tokio::test]
    async fn await_encode_computation_aborts_timed_out_task() {
        let task_finished = Arc::new(AtomicBool::new(false));
        let task_finished_flag = Arc::clone(&task_finished);
        let request_cancel = CancellationToken::new();
        let computation_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            task_finished_flag.store(true, Ordering::SeqCst);
            Ok::<_, EncodeError>(RouteEncodeResponse {
                interactions: Vec::new(),
                debug: None,
            })
        });

        let result = await_encode_computation(
            computation_task,
            Duration::from_millis(10),
            request_cancel.clone(),
        )
        .await;

        assert!(matches!(result, EncodeComputation::TimedOut));
        assert!(request_cancel.is_cancelled());

        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(!task_finished.load(Ordering::SeqCst));
    }
}
