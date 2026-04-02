use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::models::{
    protocol::ProtocolKind,
    state::{AppState, PoolSnapshot, ProtocolPoolsSnapshot},
};

#[derive(Debug, Deserialize)]
pub struct PoolsQuery {
    protocol: Option<String>,
}

#[derive(Serialize)]
struct PoolsErrorPayload {
    error: String,
}

#[derive(Serialize)]
struct PoolTokenPayload {
    address: String,
    symbol: String,
    decimals: u32,
}

#[derive(Serialize)]
struct PoolPayload {
    id: String,
    address: String,
    protocol_system: String,
    protocol_type_name: String,
    chain: String,
    contract_ids: Vec<String>,
    tokens: Vec<PoolTokenPayload>,
}

#[derive(Serialize)]
struct ProtocolPoolsPayload {
    protocol: String,
    count: usize,
    pools: Vec<PoolPayload>,
}

#[derive(Serialize)]
struct AllPoolsPayload {
    protocols: Vec<ProtocolPoolsPayload>,
}

pub async fn pools(
    State(state): State<AppState>,
    Query(query): Query<PoolsQuery>,
) -> Response {
    let protocol = match query.protocol.as_deref() {
        Some(value) => match ProtocolKind::from_api_param(value) {
            Some(kind) => Some(kind),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(PoolsErrorPayload {
                        error: format!("Unknown protocol: {value}"),
                    }),
                )
                    .into_response();
            }
        },
        None => None,
    };

    let groups = state.pools_by_protocol(protocol).await;
    if let Some(protocol) = protocol {
        let payload = groups
            .into_iter()
            .next()
            .map(protocol_group_payload)
            .unwrap_or_else(|| ProtocolPoolsPayload {
                protocol: protocol.to_string(),
                count: 0,
                pools: Vec::new(),
            });
        return (StatusCode::OK, Json(payload)).into_response();
    }

    (
        StatusCode::OK,
        Json(AllPoolsPayload {
            protocols: groups.into_iter().map(protocol_group_payload).collect(),
        }),
    )
        .into_response()
}

fn protocol_group_payload(group: ProtocolPoolsSnapshot) -> ProtocolPoolsPayload {
    ProtocolPoolsPayload {
        protocol: group.protocol.to_string(),
        count: group.pools.len(),
        pools: group.pools.into_iter().map(pool_payload).collect(),
    }
}

fn pool_payload(pool: PoolSnapshot) -> PoolPayload {
    let component = pool.component;
    PoolPayload {
        id: pool.id,
        address: component.id.to_string(),
        protocol_system: component.protocol_system.clone(),
        protocol_type_name: component.protocol_type_name.clone(),
        chain: format!("{:?}", component.chain).to_ascii_lowercase(),
        contract_ids: component
            .contract_ids
            .iter()
            .map(ToString::to_string)
            .collect(),
        tokens: component
            .tokens
            .iter()
            .map(|token| PoolTokenPayload {
                address: token.address.to_string(),
                symbol: token.symbol.clone(),
                decimals: token.decimals,
            })
            .collect(),
    }
}
