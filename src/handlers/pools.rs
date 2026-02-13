use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tycho_simulation::protocol::models::ProtocolComponent;

use crate::models::{protocol::ProtocolKind, state::AppState};

#[derive(Serialize)]
pub struct PoolsPayload {
    source: String,
    vm_enabled: bool,
    vm_status: &'static str,
    vm_block: u64,
    vm_pools: usize,
    native_status: &'static str,
    native_block: u64,
    native_pools: usize,
    protocol: String,
    protocol_pools: usize,
    data: Vec<PoolEntry>,
}

#[derive(Serialize)]
pub struct PoolEntry {
    source: &'static str,
    pair_id: String,
    pool_address: String,
    pool_name: Option<String>,
    protocol_system: String,
    protocol_type_name: String,
    token_addresses: Vec<String>,
    token_symbols: Vec<String>,
    limit_in: Option<String>,
    limit_out: Option<String>,
}

#[derive(Deserialize)]
pub struct PoolsQuery {
    protocol: Option<String>,
    source: Option<String>,
}

#[derive(Serialize)]
pub struct PoolsError {
    error: String,
}

enum PoolsSource {
    Vm,
    Native,
    Both,
}

impl PoolsSource {
    fn as_str(&self) -> &'static str {
        match self {
            PoolsSource::Vm => "vm",
            PoolsSource::Native => "native",
            PoolsSource::Both => "both",
        }
    }
}

pub async fn pools(
    State(state): State<AppState>,
    Query(query): Query<PoolsQuery>,
) -> Result<Json<PoolsPayload>, (StatusCode, Json<PoolsError>)> {
    let protocol = query.protocol;
    let source = parse_source(query.source)?;
    let protocol_kind = match protocol.as_deref() {
        None => None,
        Some(value) => Some(ProtocolKind::from_sync_state_key(value).ok_or((
            StatusCode::BAD_REQUEST,
            Json(PoolsError {
                error: format!(
                    "Unknown protocol '{}'. Expected one of: {}",
                    value,
                    ProtocolKind::ALL
                        .iter()
                        .map(ProtocolKind::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
        ))?),
    };

    Ok(Json(
        build_pools_payload(state, protocol_kind, source).await,
    ))
}

fn parse_source(value: Option<String>) -> Result<PoolsSource, (StatusCode, Json<PoolsError>)> {
    let source = value.unwrap_or_else(|| "both".to_string());
    let normalized = source.to_ascii_lowercase();
    let parsed = match normalized.as_str() {
        "vm" => PoolsSource::Vm,
        "native" => PoolsSource::Native,
        "both" => PoolsSource::Both,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(PoolsError {
                    error: format!(
                        "Unknown source '{}'. Expected one of: vm, native, both",
                        source
                    ),
                }),
            ))
        }
    };
    Ok(parsed)
}

async fn build_pools_payload(
    state: AppState,
    protocol_kind: Option<ProtocolKind>,
    source: PoolsSource,
) -> PoolsPayload {
    let vm_enabled = state.enable_vm_pools;
    let vm_status_snapshot = state.vm_stream.read().await.clone();
    let vm_rebuilding = vm_status_snapshot.rebuilding;
    let vm_ready = vm_enabled && state.vm_state_store.is_ready() && !vm_rebuilding;
    let vm_status = if !vm_enabled {
        "disabled"
    } else if vm_rebuilding {
        "rebuilding"
    } else if vm_ready {
        "ready"
    } else {
        "warming_up"
    };

    let vm_block = state.vm_block().await;
    let vm_pools = state.vm_pools().await;
    let native_block = state.current_block().await;
    let native_pools = state.native_state_store.total_states().await;
    let native_ready = state.is_ready();
    let native_update_age_ms = state.native_stream_health.last_update_age_ms().await;
    let readiness_stale_ms = state.readiness_stale.as_millis() as u64;
    let native_stale = native_update_age_ms
        .map(|age| age >= readiness_stale_ms)
        .unwrap_or(false);
    let native_status = if native_ready && !native_stale {
        "ready"
    } else {
        "warming_up"
    };

    let kinds: Vec<ProtocolKind> = match protocol_kind {
        Some(kind) => vec![kind],
        None => ProtocolKind::ALL.to_vec(),
    };
    let mut data: Vec<PoolEntry> = Vec::new();
    if matches!(source, PoolsSource::Vm | PoolsSource::Both) {
        data.extend(collect_pools(&state.vm_state_store, &kinds, "vm").await);
    }
    if matches!(source, PoolsSource::Native | PoolsSource::Both) {
        data.extend(collect_pools(&state.native_state_store, &kinds, "native").await);
    }

    data.sort_by(|a, b| {
        a.pool_address
            .cmp(&b.pool_address)
            .then(a.pair_id.cmp(&b.pair_id))
            .then(a.source.cmp(b.source))
    });

    let protocol_pools = data.len();
    PoolsPayload {
        source: source.as_str().to_string(),
        vm_enabled,
        vm_status,
        vm_block,
        vm_pools,
        native_status,
        native_block,
        native_pools,
        protocol: protocol_kind
            .map(|kind| kind.as_str().to_string())
            .unwrap_or_else(|| "all".to_string()),
        protocol_pools,
        data,
    }
}

async fn collect_pools(
    store: &crate::models::state::StateStore,
    kinds: &[ProtocolKind],
    source: &'static str,
) -> Vec<PoolEntry> {
    let mut data = Vec::new();
    for kind in kinds {
        let entries = store.pool_entries_by_kind(*kind).await;
        data.extend(entries.into_iter().map(|(pair_id, state, component)| {
            let (limit_in, limit_out) = compute_limits(&state, &component);
            PoolEntry {
                source,
                pair_id,
                pool_address: component.id.to_string(),
                pool_name: derive_pool_name(&component),
                protocol_system: component.protocol_system.clone(),
                protocol_type_name: component.protocol_type_name.clone(),
                token_addresses: component
                    .tokens
                    .iter()
                    .map(|token| token.address.to_string())
                    .collect(),
                token_symbols: component
                    .tokens
                    .iter()
                    .map(|token| token.symbol.clone())
                    .collect(),
                limit_in,
                limit_out,
            }
        }));
    }
    data
}

fn compute_limits(
    state: &Arc<dyn tycho_simulation::tycho_common::simulation::protocol_sim::ProtocolSim>,
    component: &ProtocolComponent,
) -> (Option<String>, Option<String>) {
    let Some((token_in, token_out)) = component
        .tokens
        .get(0)
        .and_then(|first| component.tokens.get(1).map(|second| (first, second)))
    else {
        return (None, None);
    };

    match state.get_limits(token_in.address.clone(), token_out.address.clone()) {
        Ok((limit_in, limit_out)) => (Some(limit_in.to_string()), Some(limit_out.to_string())),
        Err(_) => (None, None),
    }
}

fn decode_attribute(value: &[u8]) -> Option<String> {
    if value.is_empty() {
        return None;
    }

    let mut bytes = value.to_vec();
    while matches!(bytes.last(), Some(&0)) {
        bytes.pop();
    }

    std::str::from_utf8(&bytes)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn derive_pool_name(component: &ProtocolComponent) -> Option<String> {
    for key in ["pool_name", "name", "label"] {
        if let Some(label) = component
            .static_attributes
            .get(key)
            .and_then(|value| decode_attribute(value.as_ref()))
        {
            return Some(label);
        }
    }
    None
}
