use std::sync::Arc;

use futures::stream::StreamExt;
use tracing::{debug, error, info, warn};
use tycho_simulation::protocol::models::Update as TychoUpdate;

use crate::models::state::{StateStore, UpdateMetrics};

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
