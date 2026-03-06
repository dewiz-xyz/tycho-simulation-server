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

struct CancelOnDrop {
    token: CancellationToken,
    armed: bool,
}

impl CancelOnDrop {
    fn new(token: CancellationToken) -> Self {
        Self { token, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.token.cancel();
        }
    }
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
    let mut cancel_guard = CancelOnDrop::new(request_cancel.clone());

    let computation =
        match await_encode_computation(computation_task, request_timeout, request_cancel).await {
            EncodeComputation::Completed(result) => {
                cancel_guard.disarm();
                result
            }
            EncodeComputation::JoinFailed(join_err) => {
                cancel_guard.disarm();
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
    let mut computation_task = computation_task;

    tokio::select! {
        result = &mut computation_task => match result {
            Ok(result) => EncodeComputation::Completed(result),
            Err(join_err) => EncodeComputation::JoinFailed(join_err),
        },
        _ = &mut timeout => {
            request_cancel.cancel();
            // Keep awaiting the spawned task in the background so it can propagate cancellation
            // into any in-flight pool jobs instead of getting detached here.
            tokio::spawn(async move {
                let _ = computation_task.await;
            });
            EncodeComputation::TimedOut
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::Semaphore;
    use tokio_util::sync::CancellationToken;

    use super::{await_encode_computation, CancelOnDrop, EncodeComputation};
    use crate::models::messages::RouteEncodeResponse;
    use crate::services::encode::EncodeError;
    use crate::services::pool_runtime::{run_blocking_pool_job, PoolJobError};

    #[tokio::test]
    async fn await_encode_computation_cancels_timed_out_task_without_aborting_it() {
        let task_finished = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::new(AtomicBool::new(false));
        let task_finished_flag = Arc::clone(&task_finished);
        let task_cancelled_flag = Arc::clone(&task_cancelled);
        let request_cancel = CancellationToken::new();
        let request_cancel_for_task = request_cancel.clone();
        let computation_task = tokio::spawn(async move {
            request_cancel_for_task.cancelled().await;
            task_cancelled_flag.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
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

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(task_cancelled.load(Ordering::SeqCst));
        assert!(task_finished.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancel_on_drop_cancels_work_when_handler_exits_early() {
        let task_finished = Arc::new(AtomicBool::new(false));
        let task_finished_flag = Arc::clone(&task_finished);
        let request_cancel = CancellationToken::new();
        let request_cancel_for_task = request_cancel.clone();
        let computation_task = tokio::spawn(async move {
            request_cancel_for_task.cancelled().await;
            task_finished_flag.store(true, Ordering::SeqCst);
            Ok::<_, EncodeError>(RouteEncodeResponse {
                interactions: Vec::new(),
                debug: None,
            })
        });
        let guard = CancelOnDrop::new(request_cancel.clone());

        drop(guard);
        drop(computation_task);

        assert!(request_cancel.is_cancelled());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(task_finished.load(Ordering::SeqCst));
    }

    #[test]
    fn await_encode_computation_lets_cancellation_reach_blocking_pool_job() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let request_cancel = CancellationToken::new();
            let blocking_permit = Arc::new(Semaphore::new(1))
                .acquire_owned()
                .await
                .expect("permit");
            let job_started = Arc::new(AtomicBool::new(false));
            let job_started_flag = Arc::clone(&job_started);

            let (occupier_started_tx, occupier_started_rx) = tokio::sync::oneshot::channel();
            let occupier = tokio::task::spawn_blocking(move || {
                let _ = occupier_started_tx.send(());
                std::thread::sleep(Duration::from_millis(80));
            });
            occupier_started_rx.await.expect("occupier should start");

            let computation_task = tokio::spawn({
                let request_cancel = request_cancel.clone();
                async move {
                    match run_blocking_pool_job(
                        blocking_permit,
                        Duration::from_millis(200),
                        Some(request_cancel),
                        move |_| {
                            job_started_flag.store(true, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(20));
                            Ok::<_, EncodeError>(RouteEncodeResponse {
                                interactions: Vec::new(),
                                debug: None,
                            })
                        },
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(PoolJobError::Cancelled) => Err(EncodeError::simulation("cancelled")),
                        Err(PoolJobError::TimedOut) => Err(EncodeError::simulation("timed out")),
                        Err(PoolJobError::JoinFailed(join_err)) => {
                            Err(EncodeError::internal(format!("join failed: {join_err}")))
                        }
                    }
                }
            });

            let result = await_encode_computation(
                computation_task,
                Duration::from_millis(10),
                request_cancel.clone(),
            )
            .await;

            assert!(matches!(result, EncodeComputation::TimedOut));
            assert!(request_cancel.is_cancelled());

            occupier.await.expect("occupier should finish");
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert!(!job_started.load(Ordering::SeqCst));
        });
    }
}
