use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use jemalloc_ctl::{epoch, stats};
use rand::Rng;
use tokio::sync::{RwLock, Semaphore};
use tokio::time::{sleep, timeout, Instant};
use tracing::{info, warn};
use tycho_simulation::{
    evm::engine_db::SHARED_TYCHO_DB, protocol::models::Update as TychoUpdate,
    tycho_client::feed::SynchronizerState,
};

use crate::memory::maybe_purge_allocator;
use crate::models::{
    state::{StateStore, VmStreamStatus},
    stream_health::StreamHealth,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Native,
    Vm,
}

impl StreamKind {
    fn as_str(self) -> &'static str {
        match self {
            StreamKind::Native => "native",
            StreamKind::Vm => "vm",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRestartReason {
    MissingBlock,
    Error,
    Advanced,
    Stale,
    Ended,
    BuildFailed,
}

impl StreamRestartReason {
    fn as_str(self) -> &'static str {
        match self {
            StreamRestartReason::MissingBlock => "missing_block",
            StreamRestartReason::Error => "error",
            StreamRestartReason::Advanced => "advanced",
            StreamRestartReason::Stale => "stale",
            StreamRestartReason::Ended => "ended",
            StreamRestartReason::BuildFailed => "build_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StreamExit {
    pub reason: StreamRestartReason,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy)]
struct MemLogConfig {
    enabled: bool,
    min_new_pairs: usize,
    min_interval_secs: u64,
}

static MEM_LOG_CONFIG: OnceLock<MemLogConfig> = OnceLock::new();
static LAST_MEM_LOG_TS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub struct StreamSupervisorConfig {
    pub stream_stale: Duration,
    pub missing_block_burst: u64,
    pub missing_block_window: Duration,
    pub error_burst: u64,
    pub error_window: Duration,
    pub resync_grace: Duration,
    pub restart_backoff_min: Duration,
    pub restart_backoff_max: Duration,
    pub restart_backoff_jitter_pct: f64,
}

pub struct VmStreamControls {
    pub vm_stream: Arc<RwLock<VmStreamStatus>>,
    pub vm_sim_semaphore: Arc<Semaphore>,
    pub vm_sim_concurrency: u32,
}

pub async fn process_stream(
    kind: StreamKind,
    mut stream: impl futures::Stream<
            Item = Result<TychoUpdate, Box<dyn std::error::Error + Send + Sync + 'static>>,
        > + Unpin
        + Send,
    state_store: Arc<StateStore>,
    health: Arc<StreamHealth>,
    cfg: StreamSupervisorConfig,
) -> StreamExit {
    info!(stream = kind.as_str(), "Starting stream processing");
    health.mark_started().await;

    let mut ready_logged = false;

    loop {
        let next_item = timeout(cfg.stream_stale, stream.next()).await;
        match next_item {
            Err(_) => {
                let last_update_age_ms = health.last_update_age_ms().await.unwrap_or(0);
                let last_block = health.last_block().await;
                warn!(
                    event = "stream_stale",
                    stream = kind.as_str(),
                    last_update_age_ms,
                    last_block,
                    "Stream stale; triggering restart"
                );
                return StreamExit {
                    reason: StreamRestartReason::Stale,
                    last_error: None,
                };
            }
            Ok(None) => {
                warn!(
                    event = "stream_ended",
                    stream = kind.as_str(),
                    "Stream ended unexpectedly"
                );
                return StreamExit {
                    reason: StreamRestartReason::Ended,
                    last_error: None,
                };
            }
            Ok(Some(msg)) => match msg {
                Ok(update) => {
                    let now = Instant::now();
                    let has_advanced = update
                        .sync_states
                        .values()
                        .any(|state| matches!(state, SynchronizerState::Advanced(_)));

                    if has_advanced {
                        let advanced = health.record_advanced(now).await;
                        if advanced.window_started {
                            info!(
                                event = "stream_advanced",
                                stream = kind.as_str(),
                                advanced_total = advanced.total_count,
                                burst_count = advanced.burst_count,
                                window_secs = cfg.resync_grace.as_secs(),
                                "Advanced synchronizer state detected"
                            );
                        }
                        if advanced.elapsed >= cfg.resync_grace {
                            return StreamExit {
                                reason: StreamRestartReason::Advanced,
                                last_error: Some("advanced_state_grace_exceeded".to_string()),
                            };
                        }
                    } else {
                        health.clear_advanced().await;
                    }

                    let metrics = state_store.apply_update(update).await;
                    health.record_update(metrics.block_number).await;

                    info!(
                        event = "stream_update",
                        stream = kind.as_str(),
                        block = metrics.block_number,
                        updated_states = metrics.updated_states,
                        new_pairs = metrics.new_pairs,
                        removed_pairs = metrics.removed_pairs,
                        total_pairs = metrics.total_pairs,
                        "Stream update processed"
                    );
                    maybe_log_mem(kind, "stream_update", Some(metrics.new_pairs));

                    if !ready_logged && metrics.total_pairs > 0 {
                        info!(
                            stream = kind.as_str(),
                            block = metrics.block_number,
                            total_pairs = metrics.total_pairs,
                            "Service ready: first pools ingested"
                        );
                        ready_logged = true;
                    }
                }
                Err(err) => {
                    let err_msg = err.to_string();
                    if is_missing_block_error(&err_msg) {
                        health.set_last_error(Some(err_msg.clone())).await;
                        let now = Instant::now();
                        let burst = health
                            .record_missing_block(now, cfg.missing_block_window)
                            .await;
                        if burst.window_started {
                            info!(
                                event = "stream_missing_block",
                                stream = kind.as_str(),
                                missing_block_total = burst.total_count,
                                burst_count = burst.burst_count,
                                window_secs = cfg.missing_block_window.as_secs(),
                                "Missing block detected"
                            );
                        }
                        if burst.burst_count >= cfg.missing_block_burst {
                            return StreamExit {
                                reason: StreamRestartReason::MissingBlock,
                                last_error: Some(err_msg),
                            };
                        }
                    } else {
                        let now = Instant::now();
                        let burst = health.record_error(now, cfg.error_window).await;
                        health.set_last_error(Some(err_msg.clone())).await;
                        warn!(
                            stream = kind.as_str(),
                            error = %err_msg,
                            error_total = burst.total_count,
                            burst_count = burst.burst_count,
                            "Stream error"
                        );
                        if burst.burst_count >= cfg.error_burst {
                            return StreamExit {
                                reason: StreamRestartReason::Error,
                                last_error: Some(err_msg),
                            };
                        }
                    }
                }
            },
        }
    }
}

pub async fn supervise_native_stream<F, Fut, S>(
    build_stream: F,
    state_store: Arc<StateStore>,
    health: Arc<StreamHealth>,
    cfg: StreamSupervisorConfig,
) where
    F: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = anyhow::Result<S>> + Send,
    S: futures::Stream<
            Item = Result<TychoUpdate, Box<dyn std::error::Error + Send + Sync + 'static>>,
        > + Unpin
        + Send,
{
    let mut backoff = cfg.restart_backoff_min;

    loop {
        let stream = match build_stream().await {
            Ok(stream) => stream,
            Err(err) => {
                warn!(
                    event = "stream_build_failed",
                    stream = StreamKind::Native.as_str(),
                    error = %err,
                    "Failed to build native stream"
                );
                let backoff_ms = jittered_backoff_ms(backoff, cfg.restart_backoff_jitter_pct);
                sleep(Duration::from_millis(backoff_ms)).await;
                backoff = next_backoff(backoff, cfg.restart_backoff_max);
                continue;
            }
        };

        let exit = process_stream(
            StreamKind::Native,
            stream,
            Arc::clone(&state_store),
            Arc::clone(&health),
            cfg,
        )
        .await;

        let restart_count = health.increment_restart().await;
        health.reset_bursts().await;
        let last_update_age_ms = health.last_update_age_ms().await.unwrap_or(0);
        let last_block = health.last_block().await;

        let backoff_ms = jittered_backoff_ms(backoff, cfg.restart_backoff_jitter_pct);
        warn!(
            event = "stream_restart",
            stream = StreamKind::Native.as_str(),
            reason = exit.reason.as_str(),
            restart_count,
            backoff_ms,
            last_block,
            last_update_age_ms,
            "Restarting native stream"
        );

        state_store.reset().await;
        maybe_purge_allocator("native_restart");

        sleep(Duration::from_millis(backoff_ms)).await;
        backoff = next_backoff(backoff, cfg.restart_backoff_max);
    }
}

pub async fn supervise_vm_stream<F, Fut, S>(
    build_stream: F,
    state_store: Arc<StateStore>,
    health: Arc<StreamHealth>,
    cfg: StreamSupervisorConfig,
    controls: VmStreamControls,
) where
    F: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = anyhow::Result<S>> + Send,
    S: futures::Stream<
            Item = Result<TychoUpdate, Box<dyn std::error::Error + Send + Sync + 'static>>,
        > + Unpin
        + Send,
{
    let mut backoff = cfg.restart_backoff_min;
    let mut pending_rebuild: Option<VmRebuildState> = None;

    loop {
        let stream = match build_stream().await {
            Ok(stream) => {
                if let Some(rebuild) = pending_rebuild.take() {
                    finish_vm_rebuild(&controls, rebuild).await;
                }
                stream
            }
            Err(err) => {
                warn!(
                    event = "stream_build_failed",
                    stream = StreamKind::Vm.as_str(),
                    error = %err,
                    "Failed to build VM stream"
                );
                let backoff_ms = jittered_backoff_ms(backoff, cfg.restart_backoff_jitter_pct);
                sleep(Duration::from_millis(backoff_ms)).await;
                backoff = next_backoff(backoff, cfg.restart_backoff_max);
                continue;
            }
        };

        let exit = process_stream(
            StreamKind::Vm,
            stream,
            Arc::clone(&state_store),
            Arc::clone(&health),
            cfg,
        )
        .await;

        let restart_count = health.increment_restart().await;
        health.reset_bursts().await;
        let last_update_age_ms = health.last_update_age_ms().await.unwrap_or(0);
        let last_block = health.last_block().await;

        let rebuild_state = begin_vm_rebuild(
            &controls,
            Arc::clone(&state_store),
            exit.reason,
            exit.last_error,
        )
        .await;
        pending_rebuild = Some(rebuild_state);

        let backoff_ms = jittered_backoff_ms(backoff, cfg.restart_backoff_jitter_pct);
        warn!(
            event = "stream_restart",
            stream = StreamKind::Vm.as_str(),
            reason = exit.reason.as_str(),
            restart_count,
            backoff_ms,
            last_block,
            last_update_age_ms,
            "Restarting VM stream"
        );

        sleep(Duration::from_millis(backoff_ms)).await;
        backoff = next_backoff(backoff, cfg.restart_backoff_max);
    }
}

struct VmRebuildState {
    guard: tokio::sync::OwnedSemaphorePermit,
    rebuild_id: u64,
    started_at: Instant,
}

async fn begin_vm_rebuild(
    controls: &VmStreamControls,
    state_store: Arc<StateStore>,
    reason: StreamRestartReason,
    last_error: Option<String>,
) -> VmRebuildState {
    let rebuild_id = {
        let mut status = controls.vm_stream.write().await;
        status.rebuilding = true;
        status.restart_count = status.restart_count.saturating_add(1);
        status.rebuild_started_at = Some(Instant::now());
        status.last_error = last_error.or_else(|| Some(reason.as_str().to_string()));
        status.restart_count
    };

    info!(event = "vm_rebuild_start", rebuild_id, "VM rebuild started");
    maybe_log_mem(StreamKind::Vm, "vm_rebuild_start", None);

    state_store.reset().await;

    let guard = controls
        .vm_sim_semaphore
        .clone()
        .acquire_many_owned(controls.vm_sim_concurrency)
        .await
        .expect("vm semaphore closed during rebuild");

    if let Err(err) = SHARED_TYCHO_DB.clear() {
        warn!(error = %err, "Failed clearing TychoDB during VM rebuild");
    }
    maybe_purge_allocator("vm_rebuild");
    maybe_log_mem(StreamKind::Vm, "vm_rebuild_after_clear", None);

    VmRebuildState {
        guard,
        rebuild_id,
        started_at: Instant::now(),
    }
}

async fn finish_vm_rebuild(controls: &VmStreamControls, rebuild: VmRebuildState) {
    drop(rebuild.guard);

    let duration_ms = rebuild.started_at.elapsed().as_millis() as u64;
    {
        let mut status = controls.vm_stream.write().await;
        status.rebuilding = false;
        status.rebuild_started_at = None;
    }

    info!(
        event = "vm_rebuild_success",
        rebuild_id = rebuild.rebuild_id,
        duration_ms,
        "VM rebuild completed"
    );
    maybe_log_mem(StreamKind::Vm, "vm_rebuild_success", None);
}

fn is_missing_block_error(message: &str) -> bool {
    message.to_ascii_lowercase().contains("missing block")
}

fn next_backoff(current: Duration, max: Duration) -> Duration {
    let doubled = current.saturating_mul(2);
    if doubled > max {
        max
    } else {
        doubled
    }
}

fn jittered_backoff_ms(base: Duration, jitter_pct: f64) -> u64 {
    let base_ms = base.as_millis().max(1) as f64;
    let clamped = jitter_pct.clamp(0.0, 1.0);
    let jitter = rand::thread_rng().gen_range(-clamped..=clamped);
    let adjusted = (base_ms * (1.0 + jitter)).max(1.0);
    adjusted.round() as u64
}

fn mem_log_config() -> MemLogConfig {
    *MEM_LOG_CONFIG.get_or_init(|| MemLogConfig {
        enabled: parse_env_bool("STREAM_MEM_LOG", false),
        min_new_pairs: std::env::var("STREAM_MEM_LOG_MIN_NEW_PAIRS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1000),
        min_interval_secs: std::env::var("STREAM_MEM_LOG_MIN_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(30),
    })
}

fn maybe_log_mem(kind: StreamKind, label: &str, new_pairs: Option<usize>) {
    let cfg = mem_log_config();
    if !cfg.enabled {
        return;
    }
    if let Some(count) = new_pairs {
        if count < cfg.min_new_pairs {
            return;
        }
    }

    let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(_) => return,
    };
    let last = LAST_MEM_LOG_TS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < cfg.min_interval_secs {
        return;
    }
    LAST_MEM_LOG_TS.store(now, Ordering::Relaxed);

    if let Err(err) = epoch::advance() {
        warn!(error = %err, "Failed advancing jemalloc epoch");
        return;
    }

    let allocated = match stats::allocated::read() {
        Ok(value) => value,
        Err(err) => {
            warn!(error = %err, "Failed reading jemalloc allocated bytes");
            return;
        }
    };
    let resident = match stats::resident::read() {
        Ok(value) => value,
        Err(err) => {
            warn!(error = %err, "Failed reading jemalloc resident bytes");
            return;
        }
    };

    info!(
        event = "stream_mem",
        stream = kind.as_str(),
        label,
        new_pairs = new_pairs.unwrap_or(0),
        jemalloc_allocated_bytes = allocated,
        jemalloc_resident_bytes = resident,
        "Memory snapshot"
    );
}

fn parse_env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(value) => matches!(
            value.as_str(),
            "1" | "true" | "TRUE" | "True" | "yes" | "YES"
        ),
        Err(_) => default,
    }
}
