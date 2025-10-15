use axum::{extract::{State, Query}, Json};
use tracing::info;

use crate::{
    models::{
        messages::{AmountOutRequest, QuoteResult},
        state::AppState,
    },
    services::quotes::get_amounts_out,
};

pub async fn get_quote(
    State(state): State<AppState>,
    Query(request): Query<AmountOutRequest>,
) -> Json<QuoteResult> {
    info!(
        request_id = request.request_id.as_str(),
        token_in = request.token_in.as_str(),
        token_out = request.token_out.as_str(),
        amounts = request.amounts.len(),
        "Received quote request"
    );

    let computation = get_amounts_out(state, request.clone(), None).await;

    info!(
        request_id = request.request_id.as_str(),
        status = ?computation.meta.status,
        responses = computation.responses.len(),
        failures = computation.meta.failures.len(),
        "Quote computation completed"
    );

    Json(QuoteResult {
        request_id: request.request_id,
        data: computation.responses,
        meta: computation.meta,
    })
}
