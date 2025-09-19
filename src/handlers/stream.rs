use std::sync::Arc;

use futures::stream::StreamExt;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};
use tycho_simulation::protocol::models::BlockUpdate;

use crate::models::{
    messages::UpdateMessage,
    state::{StateStore, UpdateMetrics},
};

pub async fn process_stream(
    mut stream: impl futures::Stream<
            Item = Result<BlockUpdate, Box<dyn std::error::Error + Send + Sync + 'static>>,
        > + Unpin
        + Send,
    state_store: Arc<StateStore>,
    update_tx: broadcast::Sender<UpdateMessage>,
) {
    info!("Starting stream processing...");

    let mut ready_logged = false;
    let mut previous_block = 0u64;

    while let Some(msg) = stream.next().await {
        match msg {
            Ok(update) => {
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
                    "Broadcasting block update to {} clients",
                    update_tx.receiver_count()
                );

                let _ = update_tx.send(UpdateMessage::BlockUpdate {
                    block_number,
                    states_count: updated_states,
                    new_pairs_count: new_pairs,
                    removed_pairs_count: removed_pairs,
                });
            }
            Err(e) => {
                error!("Stream error: {:?}", e);
            }
        }
    }
    warn!("Stream ended unexpectedly");
}
