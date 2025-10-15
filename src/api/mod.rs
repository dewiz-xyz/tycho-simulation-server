use axum::{
    routing::{get, post},
    Router,
};

use crate::handlers::{quote::post_quote, readiness::status};
use crate::models::state::AppState;

pub fn create_router(app_state: AppState) -> Router {
    Router::new()
        .route("/quote", post(post_quote))
        .route("/status", get(status))
        .with_state(app_state)
}
