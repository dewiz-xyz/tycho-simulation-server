use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use futures::stream::StreamExt;
use tracing::{debug, error, info, warn};
use tycho_simulation::{
    evm::engine_db::SHARED_TYCHO_DB, protocol::models::Update as TychoUpdate,
    tycho_client::feed::SynchronizerState,
};

use crate::models::{
    protocol::ProtocolKind,
    state::{StateStore, UpdateMetrics},
};

pub async fn process_stream(
    mut stream: impl futures::Stream<
            Item = Result<TychoUpdate, Box<dyn std::error::Error + Send + Sync + 'static>>,
        > + Unpin
        + Send,
    state_store: Arc<StateStore>,
) {
    info!("Starting stream processing...");

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
                let newly_advanced: Vec<ProtocolKind> = current_advanced
                    .difference(&advanced_active)
                    .copied()
                    .collect();
                if !unknown_protocols.is_empty() {
                    warn!(
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
                        if let Err(err) = SHARED_TYCHO_DB.clear() {
                            warn!(error = %err, "Failed clearing TychoDB on resync");
                        }
                        state_store.reset_protocols(&newly_advanced).await;
                        ready_logged = false;
                        previous_block = 0;
                    }
                    advanced_active = current_advanced;
                } else if resync_active {
                    if let Some(started_at) = resync_started_at.take() {
                        info!(
                            event = "resync_end",
                            resync_id = resync_counter,
                            duration_ms = started_at.elapsed().as_millis(),
                            "Resync completed; synchronizer steady state restored"
                        );
                    } else {
                        info!(
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
                    "Block update: {} -> {}, updated={}, new={}, removed={}",
                    previous_block, block_number, updated_states, new_pairs, removed_pairs
                );

                previous_block = block_number;

                if !ready_logged && total_pairs > 0 {
                    info!(
                        "Service ready: first pools ingested (states={}, block={})",
                        total_pairs, block_number
                    );
                    ready_logged = true;
                }

                debug!(
                    "State updated: total_pairs={} updated_states={} new_pairs={} removed_pairs={}",
                    total_pairs, updated_states, new_pairs, removed_pairs
                );
            }
            Err(e) => {
                error!("Stream error: {:?}", e);
            }
        }
    }
    warn!("Stream ended unexpectedly");
}
