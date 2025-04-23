use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tycho_simulation::{
    models::Token,
    protocol::{models::ProtocolComponent, state::ProtocolSim},
    tycho_core::Bytes,
};

use super::messages::UpdateMessage;

#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<HashMap<Bytes, Token>>,
    pub states: Arc<RwLock<HashMap<String, (Box<dyn ProtocolSim>, ProtocolComponent)>>>,
    pub current_block: Arc<RwLock<u64>>,
    pub update_tx: broadcast::Sender<UpdateMessage>,
}
