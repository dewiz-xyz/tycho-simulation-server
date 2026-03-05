use std::time::{Duration, Instant};

use tokio::sync::OwnedSemaphorePermit;
use tokio::task::spawn_blocking;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) enum PoolJobError {
    TimedOut,
    Cancelled,
    JoinFailed(tokio::task::JoinError),
}

pub(crate) async fn run_blocking_pool_job<T, F>(
    permit: OwnedSemaphorePermit,
    pool_timeout: Duration,
    cancel_token: Option<CancellationToken>,
    job: F,
) -> Result<T, PoolJobError>
where
    T: Send + 'static,
    F: FnOnce(Instant) -> T + Send + 'static,
{
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::Semaphore;

    use super::run_blocking_pool_job;

    #[test]
    fn timeout_starts_after_blocking_work_really_begins() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let (occupier_started_tx, occupier_started_rx) = tokio::sync::oneshot::channel();
            let occupier = tokio::task::spawn_blocking(move || {
                let _ = occupier_started_tx.send(());
                std::thread::sleep(Duration::from_millis(60));
            });
            occupier_started_rx.await.expect("occupier should start");

            let permit = Arc::new(Semaphore::new(1))
                .acquire_owned()
                .await
                .expect("permit should be available");
            let started = Arc::new(AtomicBool::new(false));
            let started_flag = Arc::clone(&started);
            let started_at = std::time::Instant::now();
            let result =
                run_blocking_pool_job(permit, Duration::from_millis(20), None, move |_| {
                    started_flag.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(5));
                    7usize
                })
                .await;

            occupier.await.expect("occupier should finish");

            assert_eq!(result.expect("queued job should succeed"), 7);
            assert!(started.load(Ordering::SeqCst));
            assert!(started_at.elapsed() >= Duration::from_millis(60));
        });
    }
}
