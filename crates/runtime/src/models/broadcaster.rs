use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::RwLock;
use tokio::time::Instant;
use tycho_simulation::protocol::models::Update as TychoUpdate;

use simulator_core::broadcaster::{
    BroadcasterBackend, BroadcasterBackendHead, BroadcasterHeartbeat, BroadcasterPayload,
    BroadcasterProtocolSyncStatus, BroadcasterSnapshotChunk, BroadcasterSnapshotEnd,
    BroadcasterSnapshotPartition, BroadcasterSnapshotStart, BroadcasterStateDelta,
    BroadcasterStateEntry, BroadcasterUpdateMessage,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcasterReadiness {
    UpstreamDisconnected,
    SnapshotWarmingUp,
    Ready,
}

impl BroadcasterReadiness {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamDisconnected => "upstream_disconnected",
            Self::SnapshotWarmingUp => "snapshot_warming_up",
            Self::Ready => "ready",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BroadcasterStatusSnapshot {
    pub readiness: BroadcasterReadiness,
    pub chain_id: u64,
    pub upstream: BroadcasterUpstreamSnapshot,
    pub snapshot: BroadcasterSnapshotStatus,
    pub subscribers: BroadcasterSubscriberSnapshot,
    pub backends: BTreeMap<BroadcasterBackend, BroadcasterBackendStatus>,
}

#[derive(Debug, Clone)]
pub struct BroadcasterUpstreamSnapshot {
    pub connected: bool,
    pub restart_count: u64,
    pub last_error: Option<String>,
    pub last_disconnect_reason: Option<String>,
    pub last_update_age_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct BroadcasterSnapshotStatus {
    pub ready: bool,
    pub stream_id: String,
    pub snapshot_id: String,
    pub configured_backends: Vec<BroadcasterBackend>,
    pub total_states: usize,
    pub chunk_size: usize,
}

#[derive(Debug, Clone, Default)]
pub struct BroadcasterSubscriberSnapshot {
    pub active: usize,
    pub lag_disconnects: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BroadcasterBackendStatus {
    pub block_number: Option<u64>,
    pub pool_count: usize,
    pub sync_statuses: BTreeMap<String, BroadcasterProtocolSyncStatus>,
}

#[derive(Debug, Clone)]
pub struct BroadcasterSnapshotExport {
    pub stream_id: String,
    pub payloads: Vec<BroadcasterPayload>,
}

#[derive(Debug, Clone)]
pub struct BroadcasterLiveState {
    pub stream_id: String,
    pub snapshot_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct BroadcasterUpstreamState {
    inner: Arc<RwLock<BroadcasterUpstreamStateData>>,
}

#[derive(Debug, Default)]
struct BroadcasterUpstreamStateData {
    connected: bool,
    restart_count: u64,
    last_error: Option<String>,
    last_disconnect_reason: Option<String>,
    last_update_at: Option<Instant>,
}

impl BroadcasterUpstreamState {
    pub async fn mark_connected(&self) {
        let mut guard = self.inner.write().await;
        guard.connected = true;
        guard.last_disconnect_reason = None;
    }

    pub async fn record_update(&self) {
        let mut guard = self.inner.write().await;
        guard.connected = true;
        guard.last_update_at = Some(Instant::now());
    }

    pub async fn mark_disconnected(&self, reason: impl Into<String>, last_error: Option<String>) {
        let mut guard = self.inner.write().await;
        guard.connected = false;
        guard.restart_count = guard.restart_count.saturating_add(1);
        guard.last_disconnect_reason = Some(reason.into());
        guard.last_error = last_error;
    }

    pub async fn mark_build_failed(&self, error: impl Into<String>) {
        let error = error.into();
        let mut guard = self.inner.write().await;
        guard.connected = false;
        guard.last_error = Some(error.clone());
        guard.last_disconnect_reason = Some("build_failed".to_string());
    }

    pub async fn snapshot(&self) -> BroadcasterUpstreamSnapshot {
        let guard = self.inner.read().await;
        BroadcasterUpstreamSnapshot {
            connected: guard.connected,
            restart_count: guard.restart_count,
            last_error: guard.last_error.clone(),
            last_disconnect_reason: guard.last_disconnect_reason.clone(),
            last_update_age_ms: guard.last_update_at.map(|instant| {
                Instant::now()
                    .saturating_duration_since(instant)
                    .as_millis() as u64
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BroadcasterSnapshotCache {
    chain_id: u64,
    configured_backends: Vec<BroadcasterBackend>,
    inner: Arc<RwLock<BroadcasterSnapshotCacheData>>,
}

#[derive(Debug)]
struct BroadcasterSnapshotCacheData {
    generation: u64,
    stream_id: String,
    snapshot_id: String,
    partitions: BTreeMap<BroadcasterBackend, BroadcasterPartitionState>,
    known_backends: HashMap<String, BroadcasterBackend>,
}

#[derive(Debug, Clone, Default)]
struct BroadcasterPartitionState {
    block_number: Option<u64>,
    sync_statuses: BTreeMap<String, BroadcasterProtocolSyncStatus>,
    states: BTreeMap<String, BroadcasterStateEntry>,
}

impl BroadcasterSnapshotCache {
    pub fn new(chain_id: u64, mut configured_backends: Vec<BroadcasterBackend>) -> Self {
        configured_backends.sort();
        configured_backends.dedup();
        let generation = 1;

        Self {
            chain_id,
            configured_backends,
            inner: Arc::new(RwLock::new(BroadcasterSnapshotCacheData {
                generation,
                stream_id: format_stream_id(chain_id, generation),
                snapshot_id: format_snapshot_id(chain_id, generation),
                partitions: BTreeMap::new(),
                known_backends: HashMap::new(),
            })),
        }
    }

    pub async fn reset_generation(&self) -> BroadcasterLiveState {
        let mut guard = self.inner.write().await;
        guard.generation = guard.generation.saturating_add(1);
        guard.stream_id = format_stream_id(self.chain_id, guard.generation);
        guard.snapshot_id = format_snapshot_id(self.chain_id, guard.generation);
        guard.partitions.clear();
        guard.known_backends.clear();

        BroadcasterLiveState {
            stream_id: guard.stream_id.clone(),
            snapshot_id: guard.snapshot_id.clone(),
        }
    }

    pub async fn apply_update(&self, update: &TychoUpdate) -> Result<BroadcasterUpdateMessage> {
        let known_backends = {
            let guard = self.inner.read().await;
            guard.known_backends.clone()
        };
        let message = BroadcasterUpdateMessage::from_tycho_update(update, &known_backends)?;
        let mut guard = self.inner.write().await;
        apply_update_message(&mut guard, &message)?;
        Ok(message)
    }

    pub async fn export_snapshot(&self, chunk_size: usize) -> Result<BroadcasterSnapshotExport> {
        let guard = self.inner.read().await;
        let snapshot_id = guard.snapshot_id.clone();
        let stream_id = guard.stream_id.clone();
        let total_chunks = count_snapshot_chunks(&guard.partitions, chunk_size);
        let mut payloads = Vec::new();
        payloads.push(BroadcasterPayload::SnapshotStart(
            BroadcasterSnapshotStart::new(
                snapshot_id.clone(),
                self.chain_id,
                self.configured_backends.clone(),
                total_chunks,
            )?,
        ));

        let mut chunk_index = 0;
        let mut backend_emit_state: BTreeMap<BroadcasterBackend, usize> = BTreeMap::new();
        let mut chunk_payloads = BTreeMap::new();

        loop {
            chunk_payloads.clear();
            for backend in &self.configured_backends {
                let Some(partition) = guard.partitions.get(backend) else {
                    continue;
                };
                let state_offset = backend_emit_state.entry(*backend).or_insert(0);
                if *state_offset >= partition.states.len() && (*state_offset > 0 || chunk_index > 0)
                {
                    continue;
                }

                let partition_chunk = partition.snapshot_chunk(*backend, *state_offset, chunk_size);
                if !partition_chunk.states.is_empty() || *state_offset == 0 {
                    *state_offset = state_offset.saturating_add(partition_chunk.states.len());
                    chunk_payloads.insert(*backend, partition_chunk);
                }
            }

            if chunk_payloads.is_empty() {
                break;
            }

            payloads.push(BroadcasterPayload::SnapshotChunk(
                BroadcasterSnapshotChunk::new(
                    snapshot_id.clone(),
                    chunk_index,
                    chunk_payloads.values().cloned().collect(),
                )?,
            ));
            chunk_index = chunk_index.saturating_add(1);
        }

        payloads.push(BroadcasterPayload::SnapshotEnd(
            BroadcasterSnapshotEnd::new(snapshot_id),
        ));

        Ok(BroadcasterSnapshotExport {
            stream_id,
            payloads,
        })
    }

    pub async fn heartbeat(&self) -> Result<Option<BroadcasterPayload>> {
        let guard = self.inner.read().await;
        if !self.is_ready_locked(&guard) {
            return Ok(None);
        }

        let backend_heads = self
            .configured_backends
            .iter()
            .filter_map(|backend| {
                guard
                    .partitions
                    .get(backend)
                    .and_then(|partition| partition.block_number)
                    .map(|block_number| BroadcasterBackendHead::new(*backend, block_number))
            })
            .collect();

        Ok(Some(BroadcasterPayload::Heartbeat(
            BroadcasterHeartbeat::new(self.chain_id, guard.snapshot_id.clone(), backend_heads)?,
        )))
    }

    pub async fn live_state(&self) -> BroadcasterLiveState {
        let guard = self.inner.read().await;
        BroadcasterLiveState {
            stream_id: guard.stream_id.clone(),
            snapshot_id: guard.snapshot_id.clone(),
        }
    }

    pub async fn status_snapshot(
        &self,
        chunk_size: usize,
        upstream: BroadcasterUpstreamSnapshot,
        subscribers: BroadcasterSubscriberSnapshot,
    ) -> BroadcasterStatusSnapshot {
        let guard = self.inner.read().await;
        let ready = self.is_ready_locked(&guard);
        let readiness = if !upstream.connected {
            BroadcasterReadiness::UpstreamDisconnected
        } else if ready {
            BroadcasterReadiness::Ready
        } else {
            BroadcasterReadiness::SnapshotWarmingUp
        };

        let backends = self
            .configured_backends
            .iter()
            .map(|backend| {
                let status = guard.partitions.get(backend).cloned().unwrap_or_default();
                (
                    *backend,
                    BroadcasterBackendStatus {
                        block_number: status.block_number,
                        pool_count: status.states.len(),
                        sync_statuses: status.sync_statuses,
                    },
                )
            })
            .collect();

        BroadcasterStatusSnapshot {
            readiness,
            chain_id: self.chain_id,
            upstream,
            snapshot: BroadcasterSnapshotStatus {
                ready,
                stream_id: guard.stream_id.clone(),
                snapshot_id: guard.snapshot_id.clone(),
                configured_backends: self.configured_backends.clone(),
                total_states: guard
                    .partitions
                    .values()
                    .map(|partition| partition.states.len())
                    .sum(),
                chunk_size,
            },
            subscribers,
            backends,
        }
    }

    fn is_ready_locked(&self, guard: &BroadcasterSnapshotCacheData) -> bool {
        self.configured_backends.iter().all(|backend| {
            guard
                .partitions
                .get(backend)
                .and_then(|partition| partition.block_number)
                .is_some()
        })
    }
}

impl BroadcasterPartitionState {
    fn snapshot_chunk(
        &self,
        backend: BroadcasterBackend,
        offset: usize,
        chunk_size: usize,
    ) -> BroadcasterSnapshotPartition {
        let states = self
            .states
            .values()
            .skip(offset)
            .take(chunk_size)
            .cloned()
            .collect();
        let sync_statuses = if offset == 0 {
            self.sync_statuses.clone()
        } else {
            BTreeMap::new()
        };

        BroadcasterSnapshotPartition::new(
            backend,
            self.block_number.unwrap_or_default(),
            states,
            sync_statuses,
        )
    }
}

fn apply_update_message(
    guard: &mut BroadcasterSnapshotCacheData,
    message: &BroadcasterUpdateMessage,
) -> Result<()> {
    for partition in &message.partitions {
        let partition_state = guard.partitions.entry(partition.backend).or_default();
        partition_state.block_number = Some(partition.block_number);
        partition_state.sync_statuses = partition.sync_statuses.clone();

        for entry in &partition.new_pairs {
            guard
                .known_backends
                .insert(entry.component_id.clone(), partition.backend);
            partition_state
                .states
                .insert(entry.component_id.clone(), entry.clone());
        }

        for delta in &partition.updated_states {
            apply_state_delta(partition.backend, partition_state, delta)?;
        }

        for removed in &partition.removed_pairs {
            guard.known_backends.remove(&removed.component_id);
            partition_state.states.remove(&removed.component_id);
        }
    }

    Ok(())
}

fn apply_state_delta(
    backend: BroadcasterBackend,
    partition_state: &mut BroadcasterPartitionState,
    delta: &BroadcasterStateDelta,
) -> Result<()> {
    let Some(existing) = partition_state.states.get_mut(&delta.component_id) else {
        return Err(anyhow!(
            "missing tracked broadcaster state for {} on backend {}",
            delta.component_id,
            backend
        ));
    };
    if delta.backend != backend {
        return Err(anyhow!(
            "backend mismatch for {}: expected {}, found {}",
            delta.component_id,
            backend,
            delta.backend
        ));
    }
    existing.state = delta.state.clone();
    Ok(())
}

fn count_snapshot_chunks(
    partitions: &BTreeMap<BroadcasterBackend, BroadcasterPartitionState>,
    chunk_size: usize,
) -> u32 {
    let mut remaining = partitions
        .iter()
        .map(|(backend, partition)| (*backend, partition.states.len()))
        .collect::<BTreeMap<_, _>>();
    let mut total_chunks = 0u32;

    loop {
        let mut emitted = false;
        for states_remaining in remaining.values_mut() {
            if *states_remaining == 0 && emitted {
                continue;
            }
            if *states_remaining > 0 {
                *states_remaining = states_remaining.saturating_sub(chunk_size);
                emitted = true;
            } else if total_chunks == 0 {
                emitted = true;
            }
        }

        if !emitted {
            break;
        }

        total_chunks = total_chunks.saturating_add(1);
    }

    total_chunks
}

fn format_stream_id(chain_id: u64, generation: u64) -> String {
    format!("chain-{chain_id}-stream-{generation}")
}

fn format_snapshot_id(chain_id: u64, generation: u64) -> String {
    format!("chain-{chain_id}-snapshot-{generation}")
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::{
        count_snapshot_chunks, BroadcasterPartitionState, BroadcasterReadiness,
        BroadcasterSnapshotCache, BroadcasterSubscriberSnapshot, BroadcasterUpstreamState,
    };
    use anyhow::{anyhow, Result};
    use num_bigint::BigUint;
    use simulator_core::broadcaster::{
        BroadcasterBackend, BroadcasterEnvelope, BroadcasterPayload, BroadcasterProtocolSyncStatus,
        BroadcasterProtocolSyncStatusKind, BroadcasterStateEntry, BroadcasterSubscriptionEvent,
        BroadcasterSubscriptionTracker,
    };
    use tycho_simulation::tycho_common::dto::ProtocolStateDelta;
    use tycho_simulation::tycho_common::simulation::errors::{SimulationError, TransitionError};
    use tycho_simulation::{
        protocol::models::{ProtocolComponent, Update},
        tycho_client::feed::{BlockHeader, SynchronizerState},
        tycho_common::{
            models::{token::Token, Chain},
            simulation::protocol_sim::{Balances, GetAmountOutResult, ProtocolSim},
            Bytes,
        },
    };

    #[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
    struct DummySim(u8);

    #[typetag::serde]
    impl ProtocolSim for DummySim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(&self, _base: &Token, _quote: &Token) -> Result<f64, SimulationError> {
            Ok(0.0)
        }

        fn get_amount_out(
            &self,
            amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<GetAmountOutResult, SimulationError> {
            Ok(GetAmountOutResult::new(
                amount_in,
                BigUint::from(0u8),
                self.clone_box(),
            ))
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<(BigUint, BigUint), SimulationError> {
            Ok((BigUint::from(0u8), BigUint::from(0u8)))
        }

        fn delta_transition(
            &mut self,
            _delta: ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &Balances,
        ) -> Result<(), TransitionError> {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(self.clone())
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other
                .as_any()
                .downcast_ref::<DummySim>()
                .map(|value| value.0 == self.0)
                .unwrap_or(false)
        }
    }

    fn block_header(number: u64, seed: u8) -> BlockHeader {
        BlockHeader {
            hash: Bytes::from(vec![seed; 32]),
            number,
            parent_hash: Bytes::from(vec![seed.saturating_add(1); 32]),
            revert: false,
            timestamp: number * 10,
            partial_block_index: None,
        }
    }

    #[test]
    fn count_snapshot_chunks_handles_partition_spread() {
        let mut partitions = BTreeMap::new();
        partitions.insert(
            BroadcasterBackend::Native,
            BroadcasterPartitionState {
                block_number: Some(10),
                sync_statuses: BTreeMap::new(),
                states: (0..3)
                    .map(|index| (format!("native-{index}"), dummy_state(index)))
                    .collect(),
            },
        );
        partitions.insert(
            BroadcasterBackend::Vm,
            BroadcasterPartitionState {
                block_number: Some(11),
                sync_statuses: BTreeMap::new(),
                states: (0..1)
                    .map(|index| (format!("vm-{index}"), dummy_state(index + 10)))
                    .collect(),
            },
        );

        assert_eq!(count_snapshot_chunks(&partitions, 2), 2);
    }

    #[tokio::test]
    async fn cache_applies_updates_and_exports_snapshot() -> Result<()> {
        let cache = BroadcasterSnapshotCache::new(
            1,
            vec![BroadcasterBackend::Native, BroadcasterBackend::Vm],
        );
        let update = mixed_update();
        cache.apply_update(&update).await?;

        let export = cache.export_snapshot(1).await?;
        assert_eq!(export.stream_id, "chain-1-stream-1");
        assert!(matches!(
            export.payloads.first(),
            Some(BroadcasterPayload::SnapshotStart(_))
        ));
        assert!(matches!(
            export.payloads.last(),
            Some(BroadcasterPayload::SnapshotEnd(_))
        ));
        assert_eq!(
            export
                .payloads
                .iter()
                .filter(|payload| matches!(payload, BroadcasterPayload::SnapshotChunk(_)))
                .count(),
            1
        );

        let heartbeat = cache.heartbeat().await?;
        assert!(matches!(heartbeat, Some(BroadcasterPayload::Heartbeat(_))));
        Ok(())
    }

    #[tokio::test]
    async fn cache_exports_empty_backend_partition_in_first_snapshot_chunk() -> Result<()> {
        let cache = BroadcasterSnapshotCache::new(
            1,
            vec![BroadcasterBackend::Native, BroadcasterBackend::Vm],
        );
        cache.apply_update(&mixed_update()).await?;
        cache.apply_update(&vm_sync_only_update()).await?;

        let export = cache.export_snapshot(1).await?;
        let Some(BroadcasterPayload::SnapshotChunk(chunk)) = export
            .payloads
            .iter()
            .find(|payload| matches!(payload, BroadcasterPayload::SnapshotChunk(_)))
        else {
            return Err(anyhow!("expected snapshot_chunk payload"));
        };

        let Some(vm_partition) = chunk
            .partitions
            .iter()
            .find(|partition| partition.backend == BroadcasterBackend::Vm)
        else {
            return Err(anyhow!("expected vm snapshot partition"));
        };
        assert!(vm_partition.states.is_empty());
        assert_eq!(vm_partition.block_number, 11);
        assert_eq!(
            vm_partition.sync_statuses["vm:curve"].kind,
            BroadcasterProtocolSyncStatusKind::Ready
        );

        let mut tracker = BroadcasterSubscriptionTracker::new();
        let mut observed_events = Vec::new();
        for (message_seq, payload) in export.payloads.iter().cloned().enumerate() {
            let envelope =
                BroadcasterEnvelope::new(export.stream_id.clone(), message_seq as u64 + 1, payload);
            observed_events.push(tracker.observe(&envelope)?);
        }
        assert_eq!(
            observed_events,
            vec![
                BroadcasterSubscriptionEvent::SnapshotStarted {
                    snapshot_id: "chain-1-snapshot-1".to_string(),
                },
                BroadcasterSubscriptionEvent::SnapshotChunkAccepted {
                    snapshot_id: "chain-1-snapshot-1".to_string(),
                    chunk_index: 0,
                },
                BroadcasterSubscriptionEvent::SnapshotCompleted {
                    snapshot_id: "chain-1-snapshot-1".to_string(),
                },
            ]
        );

        Ok(())
    }

    #[tokio::test]
    async fn cache_resets_generation_on_reset() -> Result<()> {
        let cache = BroadcasterSnapshotCache::new(1, vec![BroadcasterBackend::Native]);
        cache.apply_update(&native_only_update()).await?;
        let live_before = cache.live_state().await;
        assert_eq!(live_before.snapshot_id, "chain-1-snapshot-1");

        let live_after = cache.reset_generation().await;
        assert_eq!(live_after.stream_id, "chain-1-stream-2");
        assert_eq!(live_after.snapshot_id, "chain-1-snapshot-2");
        assert!(cache.heartbeat().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn status_snapshot_distinguishes_warming_and_ready() -> Result<()> {
        let cache = BroadcasterSnapshotCache::new(1, vec![BroadcasterBackend::Native]);
        let upstream_state = BroadcasterUpstreamState::default();
        let disconnected = cache
            .status_snapshot(
                500,
                upstream_state.snapshot().await,
                BroadcasterSubscriberSnapshot::default(),
            )
            .await;
        assert_eq!(
            disconnected.readiness,
            BroadcasterReadiness::UpstreamDisconnected
        );

        upstream_state.mark_connected().await;
        let warming = cache
            .status_snapshot(
                500,
                upstream_state.snapshot().await,
                BroadcasterSubscriberSnapshot::default(),
            )
            .await;
        assert_eq!(warming.readiness, BroadcasterReadiness::SnapshotWarmingUp);

        cache.apply_update(&native_only_update()).await?;
        upstream_state.record_update().await;
        let ready = cache
            .status_snapshot(
                500,
                upstream_state.snapshot().await,
                BroadcasterSubscriberSnapshot::default(),
            )
            .await;
        assert_eq!(ready.readiness, BroadcasterReadiness::Ready);
        Ok(())
    }

    fn mixed_update() -> Update {
        let mut new_pairs = HashMap::new();
        new_pairs.insert(
            "native-1".to_string(),
            native_component("native-1", "uniswap_v2"),
        );
        new_pairs.insert("vm-1".to_string(), vm_component("vm-1", "vm:curve"));

        let mut states = HashMap::new();
        states.insert(
            "native-1".to_string(),
            Box::new(DummySim(1)) as Box<dyn ProtocolSim>,
        );
        states.insert(
            "vm-1".to_string(),
            Box::new(DummySim(2)) as Box<dyn ProtocolSim>,
        );

        Update::new(10, states, new_pairs).set_sync_states(HashMap::from([
            (
                "uniswap_v2".to_string(),
                SynchronizerState::Ready(block_header(10, 1)),
            ),
            (
                "vm:curve".to_string(),
                SynchronizerState::Ready(block_header(10, 2)),
            ),
        ]))
    }

    fn native_only_update() -> Update {
        let mut new_pairs = HashMap::new();
        new_pairs.insert(
            "native-1".to_string(),
            native_component("native-1", "uniswap_v2"),
        );

        let mut states = HashMap::new();
        states.insert(
            "native-1".to_string(),
            Box::new(DummySim(1)) as Box<dyn ProtocolSim>,
        );

        Update::new(10, states, new_pairs).set_sync_states(HashMap::from([(
            "uniswap_v2".to_string(),
            SynchronizerState::Ready(block_header(10, 1)),
        )]))
    }

    fn vm_sync_only_update() -> Update {
        Update::new(11, HashMap::new(), HashMap::new())
            .set_removed_pairs(HashMap::from([(
                "vm-1".to_string(),
                vm_component("vm-1", "vm:curve"),
            )]))
            .set_sync_states(HashMap::from([(
                "vm:curve".to_string(),
                SynchronizerState::Ready(block_header(11, 3)),
            )]))
    }

    fn native_component(_id: &str, protocol: &str) -> ProtocolComponent {
        ProtocolComponent::new(
            Bytes::from([3u8; 20]),
            protocol.to_string(),
            protocol.to_string(),
            Chain::Ethereum,
            vec![dummy_token(1, "TKNA"), dummy_token(2, "TKNB")],
            Vec::new(),
            HashMap::new(),
            Bytes::from([9u8; 32]),
            chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
                .unwrap_or_else(|| unreachable!("unix epoch"))
                .naive_utc(),
        )
    }

    fn vm_component(_id: &str, protocol: &str) -> ProtocolComponent {
        ProtocolComponent::new(
            Bytes::from([4u8; 20]),
            protocol.to_string(),
            protocol.to_string(),
            Chain::Ethereum,
            vec![dummy_token(3, "TKNC"), dummy_token(4, "TKND")],
            Vec::new(),
            HashMap::new(),
            Bytes::from([8u8; 32]),
            chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
                .unwrap_or_else(|| unreachable!("unix epoch"))
                .naive_utc(),
        )
    }

    fn dummy_state(seed: usize) -> BroadcasterStateEntry {
        BroadcasterStateEntry::new(
            format!("component-{seed}"),
            native_component(&format!("component-{seed}"), "uniswap_v2"),
            Box::new(DummySim(seed as u8)),
        )
    }

    fn dummy_token(seed: u8, symbol: &str) -> Token {
        Token::new(
            &Bytes::from([seed; 20]),
            symbol,
            18,
            0,
            &[],
            Chain::Ethereum,
            1,
        )
    }

    #[test]
    fn upstream_state_reports_disconnect_details() {
        let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|_| unreachable!("runtime"));
        runtime.block_on(async {
            let upstream = BroadcasterUpstreamState::default();
            upstream.mark_build_failed("boom").await;
            let snapshot = upstream.snapshot().await;
            assert!(!snapshot.connected);
            assert_eq!(snapshot.last_error.as_deref(), Some("boom"));
            assert_eq!(
                snapshot.last_disconnect_reason.as_deref(),
                Some("build_failed")
            );
        });
    }

    #[test]
    fn sync_status_clone_keeps_repo_owned_shape() {
        let status = BroadcasterProtocolSyncStatus {
            kind: BroadcasterProtocolSyncStatusKind::Ready,
            block: None,
            reason: None,
        };
        assert_eq!(status.kind, BroadcasterProtocolSyncStatusKind::Ready);
    }
}
