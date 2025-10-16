use axum::routing::{get, post};
use axum::Router;

use crate::handlers::{quote::simulate, readiness::status};
use crate::models::state::AppState;

pub fn create_router(app_state: AppState) -> Router {
    Router::new()
        .route("/simulate", post(simulate))
        .route("/status", get(status))
        .with_state(app_state)
}
