use std::time::{Duration, Instant};

use tokio::sync::{AcquireError, TryAcquireError};
use tokio::task::spawn_blocking;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::models::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermitAcquire {
    Acquire,
    TryAcquire,
}

#[derive(Debug)]
pub(crate) enum PoolJobError {
    NoPermit,
    SemaphoreClosed,
    TimedOut,
    Cancelled,
    JoinFailed(tokio::task::JoinError),
}

pub(crate) async fn run_blocking_pool_job<T, F>(
    state: &AppState,
    pool_protocol: &str,
    pool_timeout: Duration,
    acquire_mode: PermitAcquire,
    cancel_token: Option<CancellationToken>,
    job: F,
) -> Result<T, PoolJobError>
where
    T: Send + 'static,
    F: FnOnce(Instant) -> T + Send + 'static,
{
    let permit = match acquire_mode {
        PermitAcquire::Acquire => {
            acquire_permit(state, pool_protocol, cancel_token.as_ref()).await?
        }
        PermitAcquire::TryAcquire => try_acquire_permit(state, pool_protocol)?,
    };

    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let handle = spawn_blocking(move || {
        let _permit = permit;
        let started_at = Instant::now();
        let _ = started_tx.send(started_at);
        job(started_at)
    });
    tokio::pin!(handle);

    wait_for_start(&mut handle, started_rx, cancel_token.as_ref()).await?;
    let timeout_sleep = sleep(pool_timeout);
    tokio::pin!(timeout_sleep);

    if let Some(cancel_token) = cancel_token.as_ref() {
        tokio::select! {
            result = handle.as_mut() => result.map_err(PoolJobError::JoinFailed),
            _ = &mut timeout_sleep => {
                // Best-effort only: started `spawn_blocking` work can keep running after timeout.
                handle.as_mut().abort();
                Err(PoolJobError::TimedOut)
            }
            _ = cancel_token.cancelled() => {
                // Started blocking jobs can keep running after this future stops waiting on them.
                handle.as_mut().abort();
                Err(PoolJobError::Cancelled)
            }
        }
    } else {
        tokio::select! {
            result = handle.as_mut() => result.map_err(PoolJobError::JoinFailed),
            _ = &mut timeout_sleep => {
                // Best-effort only: started `spawn_blocking` work can keep running after timeout.
                handle.as_mut().abort();
                Err(PoolJobError::TimedOut)
            }
        }
    }
}

async fn acquire_permit(
    state: &AppState,
    pool_protocol: &str,
    cancel_token: Option<&CancellationToken>,
) -> Result<tokio::sync::OwnedSemaphorePermit, PoolJobError> {
    let semaphore = semaphore_for_protocol(state, pool_protocol);
    if let Some(cancel_token) = cancel_token {
        tokio::select! {
            permit = semaphore.acquire_owned() => permit.map_err(map_acquire_error),
            _ = cancel_token.cancelled() => Err(PoolJobError::Cancelled),
        }
    } else {
        semaphore.acquire_owned().await.map_err(map_acquire_error)
    }
}

fn try_acquire_permit(
    state: &AppState,
    pool_protocol: &str,
) -> Result<tokio::sync::OwnedSemaphorePermit, PoolJobError> {
    semaphore_for_protocol(state, pool_protocol)
        .try_acquire_owned()
        .map_err(map_try_acquire_error)
}

fn semaphore_for_protocol(
    state: &AppState,
    pool_protocol: &str,
) -> std::sync::Arc<tokio::sync::Semaphore> {
    if pool_protocol.starts_with("vm:") {
        state.vm_sim_semaphore()
    } else {
        state.native_sim_semaphore()
    }
}

fn map_acquire_error(_: AcquireError) -> PoolJobError {
    PoolJobError::SemaphoreClosed
}

fn map_try_acquire_error(error: TryAcquireError) -> PoolJobError {
    match error {
        TryAcquireError::NoPermits => PoolJobError::NoPermit,
        TryAcquireError::Closed => PoolJobError::SemaphoreClosed,
    }
}

async fn wait_for_start<T>(
    handle: &mut std::pin::Pin<&mut tokio::task::JoinHandle<T>>,
    started_rx: tokio::sync::oneshot::Receiver<Instant>,
    cancel_token: Option<&CancellationToken>,
) -> Result<(), PoolJobError>
where
    T: Send + 'static,
{
    if let Some(cancel_token) = cancel_token {
        tokio::select! {
            biased;
            started = started_rx => started
                .map(|_| ())
                .map_err(|_| panic!("blocking job ended before sending a start signal")),
            result = handle.as_mut() => {
                match result {
                    Ok(_) => panic!("blocking job completed before sending a start signal"),
                    Err(join_err) => Err(PoolJobError::JoinFailed(join_err)),
                }
            }
            _ = cancel_token.cancelled() => {
                handle.as_mut().abort();
                Err(PoolJobError::Cancelled)
            }
        }
    } else {
        tokio::select! {
            biased;
            started = started_rx => started
                .map(|_| ())
                .map_err(|_| panic!("blocking job ended before sending a start signal")),
            result = handle.as_mut() => {
                match result {
                    Ok(_) => panic!("blocking job completed before sending a start signal"),
                    Err(join_err) => Err(PoolJobError::JoinFailed(join_err)),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::{RwLock, Semaphore};
    use tycho_simulation::tycho_common::models::Chain;

    use super::{run_blocking_pool_job, PermitAcquire, PoolJobError};
    use crate::models::state::{AppState, StateStore, VmStreamStatus};
    use crate::models::stream_health::StreamHealth;
    use crate::models::tokens::TokenStore;

    fn test_app_state(native_permits: usize, vm_permits: usize) -> AppState {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_secs(1),
        ));
        let native_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&token_store)));

        AppState {
            tokens: token_store,
            native_state_store,
            vm_state_store,
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            latest_native_gas_price_wei: Arc::new(RwLock::new(None)),
            native_gas_price_reporting_enabled: Arc::new(RwLock::new(false)),
            enable_vm_pools: true,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_secs(1),
            pool_timeout_native: Duration::from_millis(50),
            pool_timeout_vm: Duration::from_millis(50),
            request_timeout: Duration::from_secs(1),
            native_sim_semaphore: Arc::new(Semaphore::new(native_permits)),
            vm_sim_semaphore: Arc::new(Semaphore::new(vm_permits)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: native_permits,
            vm_sim_concurrency: vm_permits,
        }
    }

    #[test]
    fn timeout_starts_after_blocking_work_really_begins() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let state = test_app_state(1, 1);
            let (occupier_started_tx, occupier_started_rx) = tokio::sync::oneshot::channel();
            let occupier = tokio::task::spawn_blocking(move || {
                let _ = occupier_started_tx.send(());
                std::thread::sleep(Duration::from_millis(60));
            });
            occupier_started_rx.await.expect("occupier should start");

            let started = Arc::new(AtomicBool::new(false));
            let started_flag = Arc::clone(&started);
            let started_at = std::time::Instant::now();
            let result = run_blocking_pool_job(
                &state,
                "uniswap_v2",
                Duration::from_millis(20),
                PermitAcquire::TryAcquire,
                None,
                move |_| {
                    started_flag.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(5));
                    7usize
                },
            )
            .await;

            occupier.await.expect("occupier should finish");

            assert_eq!(result.expect("queued job should succeed"), 7);
            assert!(started.load(Ordering::SeqCst));
            assert!(started_at.elapsed() >= Duration::from_millis(60));
        });
    }

    #[tokio::test]
    async fn try_acquire_reports_no_permit() {
        let state = test_app_state(0, 1);
        let result = run_blocking_pool_job(
            &state,
            "uniswap_v2",
            Duration::from_millis(10),
            PermitAcquire::TryAcquire,
            None,
            |_| 1usize,
        )
        .await;

        assert!(matches!(result, Err(PoolJobError::NoPermit)));
    }

    #[tokio::test]
    async fn helper_uses_protocol_specific_semaphore() {
        let state = test_app_state(0, 1);
        let vm_runs = Arc::new(AtomicUsize::new(0));
        let vm_runs_clone = Arc::clone(&vm_runs);
        let vm_result = run_blocking_pool_job(
            &state,
            "vm:curve",
            Duration::from_millis(10),
            PermitAcquire::TryAcquire,
            None,
            move |_| {
                vm_runs_clone.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await;

        assert!(vm_result.is_ok());
        assert_eq!(vm_runs.load(Ordering::SeqCst), 1);

        let native_result = run_blocking_pool_job(
            &state,
            "uniswap_v2",
            Duration::from_millis(10),
            PermitAcquire::TryAcquire,
            None,
            |_| (),
        )
        .await;

        assert!(matches!(native_result, Err(PoolJobError::NoPermit)));
    }

    #[tokio::test]
    async fn acquire_mode_waits_for_permit_before_starting_timeout() {
        let state = test_app_state(0, 1);
        let semaphore = state.native_sim_semaphore();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            semaphore.add_permits(1);
        });

        let started_at = std::time::Instant::now();
        let result = run_blocking_pool_job(
            &state,
            "uniswap_v2",
            Duration::from_millis(10),
            PermitAcquire::Acquire,
            None,
            |_| {
                std::thread::sleep(Duration::from_millis(5));
                11usize
            },
        )
        .await;

        assert_eq!(result.expect("job should succeed after permit release"), 11);
        assert!(started_at.elapsed() >= Duration::from_millis(40));
    }
}
