use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use runtime::broadcaster::{
    app::BroadcasterAppState, service::SnapshotSessionError, state::BroadcasterReadiness,
};
use serde_json::json;
use simulator_core::broadcaster::BroadcasterTokenLookupRequest;
use tracing::warn;

use crate::models::broadcaster_rpc::BroadcasterStatusPayload;

pub async fn status(
    State(state): State<BroadcasterAppState>,
) -> (StatusCode, Json<BroadcasterStatusPayload>) {
    let snapshot = state.status_snapshot().await;
    (
        StatusCode::OK,
        Json(BroadcasterStatusPayload::from(snapshot)),
    )
}

pub async fn ready(
    State(state): State<BroadcasterAppState>,
) -> (StatusCode, Json<BroadcasterStatusPayload>) {
    let snapshot = state.readiness_snapshot().await;
    let status_code = if snapshot.readiness == BroadcasterReadiness::Ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status_code, Json(BroadcasterStatusPayload::from(snapshot)))
}

pub async fn deployment_ready(
    State(state): State<BroadcasterAppState>,
) -> (StatusCode, Json<BroadcasterStatusPayload>) {
    let admission = state.deployment_admission_snapshot();
    let mut snapshot = state.status_snapshot().await;
    snapshot.deployment_admission = admission.clone();
    let status_code = if admission.admitted {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status_code, Json(BroadcasterStatusPayload::from(snapshot)))
}

pub async fn create_snapshot_session(State(state): State<BroadcasterAppState>) -> Response {
    snapshot_session_create_response(state.create_snapshot_session().await, &state).await
}

pub async fn snapshot_session_payload(
    State(state): State<BroadcasterAppState>,
    Path((session_id, index)): Path<(u64, u32)>,
) -> Response {
    match state.snapshot_session_payload(session_id, index).await {
        Ok(envelope) => (StatusCode::OK, Json(envelope)).into_response(),
        Err(error) => snapshot_session_error_response(error),
    }
}

pub async fn token_lookup(
    State(state): State<BroadcasterAppState>,
    payload: std::result::Result<Json<BroadcasterTokenLookupRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(payload) => payload,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("invalid token lookup request: {error}") })),
            )
                .into_response()
        }
    };

    if request.chain_id != state.chain_id() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "token lookup chain_id {} does not match broadcaster chain_id {}",
                    request.chain_id,
                    state.chain_id()
                )
            })),
        )
            .into_response();
    }

    if let Some(address) = request.addresses.iter().find(|address| address.len() != 20) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("token lookup address {address} is not a 20-byte EVM address")
            })),
        )
            .into_response();
    }

    match state.lookup_tokens(request.addresses).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => {
            warn!(error = %error, "Broadcaster token lookup failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

pub async fn token_snapshot(State(state): State<BroadcasterAppState>) -> Response {
    (StatusCode::OK, Json(state.token_snapshot().await)).into_response()
}

async fn snapshot_session_create_response(
    result: std::result::Result<
        Option<simulator_core::broadcaster::BroadcasterSnapshotSessionResponse>,
        impl std::fmt::Display,
    >,
    state: &BroadcasterAppState,
) -> Response {
    match result {
        Ok(Some(session)) => (StatusCode::CREATED, Json(session)).into_response(),
        Ok(None) => {
            let snapshot = state.status_snapshot().await;
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(BroadcasterStatusPayload::from(snapshot)),
            )
                .into_response()
        }
        Err(error) => {
            warn!(error = %error, "Failed to create broadcaster snapshot session");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn snapshot_session_error_response(error: SnapshotSessionError) -> Response {
    let (status, message) = match error {
        SnapshotSessionError::NotFound => (StatusCode::NOT_FOUND, "snapshot session not found"),
        SnapshotSessionError::Expired => (StatusCode::GONE, "snapshot session expired"),
        SnapshotSessionError::PayloadOutOfRange => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            "snapshot payload index out of range",
        ),
        SnapshotSessionError::Busy => (
            StatusCode::TOO_MANY_REQUESTS,
            "snapshot payload fetch capacity reached",
        ),
    };
    (status, Json(json!({ "error": message }))).into_response()
}
