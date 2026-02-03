use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use futures::stream::StreamExt;
use tokio::sync::Semaphore;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};
use tycho_simulation::{
    evm::engine_db::SHARED_TYCHO_DB, protocol::models::Update as TychoUpdate,
    tycho_client::feed::SynchronizerState,
};

use crate::models::tokens::TokenStore;
use crate::models::{
    protocol::ProtocolKind,
    state::{StateStore, UpdateMetrics, VmStreamStatus},
};
use crate::services::stream_builder::build_vm_stream;

#[derive(Debug, Clone)]
pub enum StreamExitReason {
    Ended,
    ResyncRequested { protocols: Vec<String> },
    StreamError { error: String },
}

pub async fn process_stream(
    mut stream: impl futures::Stream<
            Item = Result<TychoUpdate, Box<dyn std::error::Error + Send + Sync + 'static>>,
        > + Unpin
        + Send,
    state_store: Arc<StateStore>,
    stream_name: &'static str,
    exit_on_advanced: bool,
) -> StreamExitReason {
    info!(stream = stream_name, "Starting stream processing...");

    let mut ready_logged = false;
    let mut previous_block = 0u64;
    let mut resync_active = false;
    let mut resync_counter = 0u64;
    let mut resync_started_at: Option<Instant> = None;
    let mut advanced_active: HashSet<ProtocolKind> = HashSet::new();

    while let Some(msg) = stream.next().await {
        match msg {
            Ok(update) => {
                let advanced_protocols: Vec<String> = update
                    .sync_states
                    .iter()
                    .filter_map(|(protocol, state)| {
                        matches!(state, SynchronizerState::Advanced(_)).then_some(protocol.clone())
                    })
                    .collect();
                let mut current_advanced = HashSet::new();
                let mut unknown_protocols = Vec::new();
                for protocol in advanced_protocols.iter() {
                    if let Some(kind) = ProtocolKind::from_sync_state_key(protocol) {
                        current_advanced.insert(kind);
                    } else {
                        unknown_protocols.push(protocol.clone());
                    }
                }

                if exit_on_advanced && !advanced_protocols.is_empty() {
                    warn!(
                        stream = stream_name,
                        protocols = ?advanced_protocols,
                        unknown_protocols = ?unknown_protocols,
                        "Resync requested; stream restart required"
                    );
                    return StreamExitReason::ResyncRequested {
                        protocols: advanced_protocols,
                    };
                }

                let newly_advanced: Vec<ProtocolKind> = current_advanced
                    .difference(&advanced_active)
                    .copied()
                    .collect();
                if !unknown_protocols.is_empty() {
                    warn!(
                        stream = stream_name,
                        event = "resync_unknown_protocols",
                        protocols = ?unknown_protocols,
                        "Resync requested for unknown protocol; skipping state reset"
                    );
                }
                if !advanced_protocols.is_empty() {
                    if !resync_active {
                        let pools_before = state_store.total_states().await;
                        resync_counter += 1;
                        resync_started_at = Some(Instant::now());
                        info!(
                            stream = stream_name,
                            event = "resync_start",
                            resync_id = resync_counter,
                            protocols = ?advanced_protocols,
                            pools_before,
                            block_before = previous_block,
                            "Resync detected via advanced synchronizer; clearing state"
                        );
                        resync_active = true;
                    }
                    if !newly_advanced.is_empty() {
                        state_store.reset_protocols(&newly_advanced).await;
                        ready_logged = false;
                        previous_block = 0;
                    }
                    advanced_active = current_advanced;
                } else if resync_active {
                    if let Some(started_at) = resync_started_at.take() {
                        info!(
                            stream = stream_name,
                            event = "resync_end",
                            resync_id = resync_counter,
                            duration_ms = started_at.elapsed().as_millis(),
                            "Resync completed; synchronizer steady state restored"
                        );
                    } else {
                        info!(
                            stream = stream_name,
                            event = "resync_end",
                            resync_id = resync_counter,
                            "Resync completed; synchronizer steady state restored"
                        );
                    }
                    resync_active = false;
                    advanced_active.clear();
                }

                let UpdateMetrics {
                    block_number,
                    updated_states,
                    new_pairs,
                    removed_pairs,
                    total_pairs,
                } = state_store.apply_update(update).await;

                info!(
                    stream = stream_name,
                    "Block update: {} -> {}, updated={}, new={}, removed={}",
                    previous_block,
                    block_number,
                    updated_states,
                    new_pairs,
                    removed_pairs
                );

                previous_block = block_number;

                if !ready_logged && total_pairs > 0 {
                    if stream_name == "native" {
                        info!(
                            stream = stream_name,
                            "Service ready: first pools ingested (states={}, block={})",
                            total_pairs,
                            block_number
                        );
                    } else {
                        info!(
                            stream = stream_name,
                            "VM ready: first pools ingested (states={}, block={})",
                            total_pairs,
                            block_number
                        );
                    }
                    ready_logged = true;
                }

                debug!(
                    stream = stream_name,
                    "State updated: total_pairs={} updated_states={} new_pairs={} removed_pairs={}",
                    total_pairs,
                    updated_states,
                    new_pairs,
                    removed_pairs
                );
            }
            Err(e) => {
                error!(stream = stream_name, "Stream error: {:?}", e);
                if exit_on_advanced {
                    return StreamExitReason::StreamError {
                        error: format!("{:?}", e),
                    };
                }
            }
        }
    }
    warn!(stream = stream_name, "Stream ended unexpectedly");
    StreamExitReason::Ended
}

pub struct VmStreamManagerArgs {
    pub tycho_url: String,
    pub api_key: String,
    pub tvl_add_threshold: f64,
    pub tvl_keep_threshold: f64,
    pub tokens: Arc<TokenStore>,
    pub vm_state_store: Arc<StateStore>,
    pub vm_stream: Arc<VmStreamStatus>,
    pub vm_sim_semaphore: Arc<Semaphore>,
    pub vm_sim_concurrency: usize,
}

pub async fn run_vm_stream_manager(args: VmStreamManagerArgs) {
    let VmStreamManagerArgs {
        tycho_url,
        api_key,
        tvl_add_threshold,
        tvl_keep_threshold,
        tokens,
        vm_state_store,
        vm_stream,
        vm_sim_semaphore,
        vm_sim_concurrency,
    } = args;
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);
    let drain_timeout = Duration::from_secs(30);

    let permit_count = u32::try_from(vm_sim_concurrency).unwrap_or(u32::MAX);

    loop {
        let rebuild_id = vm_stream.mark_rebuilding().await;
        info!(rebuild_id, "VM rebuild starting");

        // Reset the in-memory VM pool store immediately so we never serve stale VM pools
        // while we rebuild the shared Tycho DB snapshot.
        vm_state_store.reset().await;

        // Drain all VM simulation permits before clearing the shared Tycho DB; otherwise a
        // mid-flight VM simulation may observe a partially cleared or inconsistent DB.
        let drain = vm_sim_semaphore.clone().acquire_many_owned(permit_count);
        let drain_permit = match tokio::time::timeout(drain_timeout, drain).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(err)) => {
                vm_stream
                    .record_error(format!("VM semaphore closed while rebuilding: {}", err))
                    .await;
                warn!(error = %err, "VM semaphore closed while rebuilding; retrying");
                sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
                continue;
            }
            Err(_) => {
                vm_stream
                    .record_error(format!(
                        "Timed out draining VM simulations after {}s",
                        drain_timeout.as_secs()
                    ))
                    .await;
                warn!(
                    timeout_s = drain_timeout.as_secs(),
                    "Timed out draining VM simulations; retrying"
                );
                sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
                continue;
            }
        };

        if let Err(err) = SHARED_TYCHO_DB.clear() {
            warn!(error = %err, "Failed clearing TychoDB for VM rebuild");
        }

        match build_vm_stream(
            &tycho_url,
            &api_key,
            tvl_add_threshold,
            tvl_keep_threshold,
            Arc::clone(&tokens),
        )
        .await
        {
            Ok(stream) => {
                let _ = vm_stream.mark_running_if_current(rebuild_id).await;
                info!(rebuild_id, "VM protocol stream running (background)");
                backoff = Duration::from_secs(1);
                drop(drain_permit);

                match process_stream(stream, Arc::clone(&vm_state_store), "vm", true).await {
                    StreamExitReason::ResyncRequested { protocols } => {
                        vm_stream
                            .record_error(format!(
                                "VM resync requested (protocols={:?})",
                                protocols
                            ))
                            .await;
                    }
                    StreamExitReason::StreamError { error } => {
                        vm_stream
                            .record_error(format!("VM stream error: {}", error))
                            .await;
                    }
                    StreamExitReason::Ended => {
                        vm_stream
                            .record_error("VM stream ended unexpectedly".to_string())
                            .await;
                    }
                }
            }
            Err(err) => {
                vm_stream
                    .record_error(format!("Failed building VM stream: {:?}", err))
                    .await;
                warn!(error = ?err, "Failed building VM stream; retrying");
                drop(drain_permit);
            }
        }

        sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}
