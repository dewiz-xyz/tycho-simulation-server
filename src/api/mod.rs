use axum::{routing::get, Router};

use crate::handlers::websocket::handle_ws_upgrade;
use crate::models::state::AppState;

pub fn create_router(app_state: AppState) -> Router {
    Router::new()
        .route("/ws", get(handle_ws_upgrade))
        .with_state(app_state)
}
