use futures::stream::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};
use tycho_simulation::protocol::{models::BlockUpdate, state::ProtocolSim};

use crate::models::messages::UpdateMessage;

pub async fn process_stream(
    mut stream: impl futures::Stream<
            Item = Result<
                BlockUpdate,
                Box<dyn std::error::Error + Send + Sync + 'static>,
            >,
        > + Unpin
        + Send,
    states: Arc<
        RwLock<
            HashMap<
                String,
                (
                    Box<dyn ProtocolSim>,
                    tycho_simulation::protocol::models::ProtocolComponent,
                ),
            >,
        >,
    >,
    current_block: Arc<RwLock<u64>>,
    update_tx: broadcast::Sender<UpdateMessage>,
) {
    info!("Starting stream processing...");

    // Emit a one-time readiness log when pools are first ingested
    let mut ready_logged = false;

    while let Some(msg) = stream.next().await {
        match msg {
            Ok(update) => {
                let mut states_map = states.write().await;
                let initial_len = states_map.len();
                let mut block = current_block.write().await;
                let prev_block = *block;
                *block = update.block_number;

                let new_pairs_count = update.new_pairs.len();

                info!(
                    "Block update: {} -> {}, states: {}",
                    prev_block,
                    update.block_number,
                    update.states.len(),
                );

                debug!("Updating {} existing states", update.states.len());
                // Update existing states
                for (id, state) in &update.states {
                    if let Some((existing_state, _)) = states_map.get_mut(id) {
                        *existing_state = state.clone();
                    }
                }

                debug!("Adding {} new pairs", new_pairs_count);
                // Add new pairs
                for (id, comp) in update.new_pairs {
                    debug!("New pair ingested: {}", id);
                    if let Some(state) = update.states.get(&id) {
                        states_map.insert(id, (state.clone(), comp));
                    }
                }

                let new_len = states_map.len();
                debug!("States map size after update: {}", new_len);

                if !ready_logged && initial_len == 0 && new_len > 0 {
                    info!(
                        "Service ready: first pools ingested (states={}, block={})",
                        new_len,
                        *block
                    );
                    ready_logged = true;
                }
                debug!(
                    "Broadcasting block update to {} clients",
                    update_tx.receiver_count()
                );
                // Broadcast update
                let _ = update_tx.send(UpdateMessage::BlockUpdate {
                    block_number: update.block_number,
                    states_count: update.states.len(),
                    new_pairs_count,
                    removed_pairs_count: update.removed_pairs.len(),
                });
            }
            Err(e) => {
                error!("Stream error: {:?}", e);
            }
        }
    }
    warn!("Stream ended unexpectedly");
}
