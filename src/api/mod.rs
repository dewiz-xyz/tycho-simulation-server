use axum::routing::get;
use axum::Router;

use crate::handlers::{quote::get_quote, readiness::status};
use crate::models::state::AppState;

pub fn create_router(app_state: AppState) -> Router {
    Router::new()
        .route("/quote", get(get_quote))
        .route("/status", get(status))
        .with_state(app_state)
}
