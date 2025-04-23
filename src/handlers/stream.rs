use futures::stream::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tycho_simulation::protocol::{models::BlockUpdate, state::ProtocolSim};

use crate::models::messages::UpdateMessage;

pub async fn process_stream(
    mut stream: impl futures::Stream<Item = Result<BlockUpdate, impl std::error::Error>> + Unpin + Send,
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
    println!("Starting stream processing...");

    while let Some(msg) = stream.next().await {
        match msg {
            Ok(update) => {
                let mut states_map = states.write().await;
                let mut block = current_block.write().await;
                let prev_block = *block;
                *block = update.block_number;

                let new_pairs_count = update.new_pairs.len();

                println!(
                    "Block update: {} -> {}, states: {}",
                    prev_block,
                    update.block_number,
                    update.states.len(),
                );

                // Update existing states
                for (id, state) in &update.states {
                    if let Some((existing_state, _)) = states_map.get_mut(id) {
                        *existing_state = state.clone();
                    }
                }

                // Add new pairs
                for (id, comp) in update.new_pairs {
                    if let Some(state) = update.states.get(&id) {
                        states_map.insert(id, (state.clone(), comp));
                    }
                }

                // Broadcast update
                let _ = update_tx.send(UpdateMessage::BlockUpdate {
                    block_number: update.block_number,
                    states_count: update.states.len(),
                    new_pairs_count,
                    removed_pairs_count: update.removed_pairs.len(),
                });
            }
            Err(e) => {
                println!("Stream error: {:?}", e);
            }
        }
    }
    println!("Stream ended");
}
