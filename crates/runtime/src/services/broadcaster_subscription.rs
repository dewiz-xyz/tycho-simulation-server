use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::StreamExt;
use rand::Rng;
use tokio::sync::{OwnedSemaphorePermit, RwLock, Semaphore};
use tokio::time::{timeout, Instant};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Error as WsError, Message},
};
use tracing::{info, warn};
use tycho_simulation::{
    evm::engine_db::SHARED_TYCHO_DB,
    protocol::models::{ProtocolComponent, Update},
    tycho_common::simulation::protocol_sim::ProtocolSim,
};

use simulator_core::broadcaster::{
    BroadcasterBackend, BroadcasterBackendHead, BroadcasterEnvelope, BroadcasterPayload,
    BroadcasterSnapshotPartition, BroadcasterSubscriptionTracker, BroadcasterUpdatePartition,
};

use crate::memory::maybe_purge_allocator;
use crate::models::state::{BroadcasterSubscriptionStatus, StateStore, VmStreamStatus};
use crate::models::stream_health::StreamHealth;
use crate::stream::StreamSupervisorConfig;

#[derive(Clone)]
pub struct BroadcasterSubscriptionControls {
    pub broadcaster_subscription: BroadcasterSubscriptionStatus,
    pub native_state_store: Arc<StateStore>,
    pub vm_state_store: Arc<StateStore>,
    pub native_stream_health: Arc<StreamHealth>,
    pub vm_stream_health: Arc<StreamHealth>,
    pub vm_stream: Arc<RwLock<VmStreamStatus>>,
    pub vm_sim_semaphore: Arc<Semaphore>,
    pub vm_sim_concurrency: u32,
    pub enable_vm_pools: bool,
}

pub async fn supervise_broadcaster_subscription(
    ws_url: String,
    cfg: StreamSupervisorConfig,
    controls: BroadcasterSubscriptionControls,
) {
    let mut backoff = cfg.restart_backoff_min;
    let mut vm_rebuild = None;

    loop {
        let stream = match connect_async(ws_url.as_str()).await {
            Ok((stream, _response)) => {
                controls.broadcaster_subscription.mark_connected().await;
                controls.native_stream_health.mark_started().await;
                if controls.enable_vm_pools {
                    controls.vm_stream_health.mark_started().await;
                }
                stream
            }
            Err(error) => {
                let message = format!("Failed to connect to broadcaster websocket: {error}");
                vm_rebuild =
                    handle_subscription_reset(&controls, Some(message.clone()), vm_rebuild).await;
                warn!(
                    event = "broadcaster_subscription_connect_failed",
                    error = %message,
                    "Failed to connect to broadcaster subscription"
                );
                let backoff_ms = jittered_backoff_ms(backoff, cfg.restart_backoff_jitter_pct);
                sleep_backoff(backoff_ms, cfg.memory).await;
                backoff = next_backoff(backoff, cfg.restart_backoff_max);
                continue;
            }
        };

        info!(ws_url, "Connected to broadcaster subscription");
        let (exit, next_vm_rebuild) =
            process_broadcaster_subscription(stream, &controls, &cfg, vm_rebuild).await;
        vm_rebuild =
            handle_subscription_reset(&controls, exit.last_error.clone(), next_vm_rebuild).await;

        let backoff_ms = jittered_backoff_ms(backoff, cfg.restart_backoff_jitter_pct);
        warn!(
            event = "broadcaster_subscription_restart",
            reason = exit.reason,
            backoff_ms,
            last_error = exit.last_error.as_deref(),
            "Restarting broadcaster subscription"
        );
        sleep_backoff(backoff_ms, cfg.memory).await;
        backoff = next_backoff(backoff, cfg.restart_backoff_max);
    }
}

struct BroadcasterSubscriptionProcessor {
    controls: BroadcasterSubscriptionControls,
    tracker: BroadcasterSubscriptionTracker,
    bootstrap_blocks: HashMap<BroadcasterBackend, u64>,
    vm_rebuild: Option<VmSubscriptionRebuildState>,
}

impl BroadcasterSubscriptionProcessor {
    fn new(
        controls: BroadcasterSubscriptionControls,
        vm_rebuild: Option<VmSubscriptionRebuildState>,
    ) -> Self {
        Self {
            controls,
            tracker: BroadcasterSubscriptionTracker::new(),
            bootstrap_blocks: HashMap::new(),
            vm_rebuild,
        }
    }

    fn bootstrap_complete(&self) -> bool {
        matches!(
            self.tracker.state(),
            simulator_core::broadcaster::BroadcasterSubscriptionState::Live { .. }
        )
    }

    async fn observe(&mut self, envelope: BroadcasterEnvelope) -> Result<()> {
        self.tracker
            .observe(&envelope)
            .map_err(|error| anyhow!("invalid broadcaster envelope: {error}"))?;

        match envelope.payload {
            BroadcasterPayload::SnapshotStart(start) => {
                self.bootstrap_blocks.clear();
                self.controls
                    .broadcaster_subscription
                    .mark_snapshot_started(envelope.stream_id, start.snapshot_id)
                    .await;
            }
            BroadcasterPayload::SnapshotChunk(chunk) => {
                for partition in chunk.partitions {
                    self.bootstrap_blocks
                        .insert(partition.backend, partition.block_number);
                    self.apply_snapshot_partition(partition).await;
                }
            }
            BroadcasterPayload::SnapshotEnd(_end) => {
                self.refresh_bootstrap_health().await;
                self.controls
                    .broadcaster_subscription
                    .mark_bootstrap_complete()
                    .await;
                self.finish_vm_rebuild().await;
            }
            BroadcasterPayload::Update(update) => {
                for partition in update.partitions {
                    self.apply_live_update_partition(partition).await;
                }
            }
            BroadcasterPayload::Heartbeat(heartbeat) => {
                for head in heartbeat.backend_heads {
                    self.apply_heartbeat(head).await;
                }
            }
        }

        Ok(())
    }

    async fn finish_vm_rebuild(&mut self) {
        let Some(rebuild) = self.vm_rebuild.take() else {
            return;
        };

        drop(rebuild.guard);

        let mut vm_stream = self.controls.vm_stream.write().await;
        vm_stream.rebuilding = false;
        vm_stream.rebuild_started_at = None;
    }

    async fn apply_snapshot_partition(&self, partition: BroadcasterSnapshotPartition) {
        if let Some(target) = self.target(partition.backend) {
            let update = snapshot_partition_update(partition);
            target.store.apply_update(update).await;
        }
    }

    async fn apply_live_update_partition(&self, partition: BroadcasterUpdatePartition) {
        if let Some(target) = self.target(partition.backend) {
            let block_number = partition.block_number;
            let update = live_partition_update(partition);
            target.store.apply_update(update).await;
            target.health.record_update(block_number).await;
        }
    }

    async fn apply_heartbeat(&self, head: BroadcasterBackendHead) {
        if let Some(target) = self.target(head.backend) {
            target
                .store
                .apply_update(Update::new(
                    head.block_number,
                    HashMap::new(),
                    HashMap::new(),
                ))
                .await;
            target.health.record_update(head.block_number).await;
        }
    }

    async fn refresh_bootstrap_health(&self) {
        for (backend, block_number) in &self.bootstrap_blocks {
            if let Some(target) = self.target(*backend) {
                target.health.record_update(*block_number).await;
            }
        }
    }

    fn target(&self, backend: BroadcasterBackend) -> Option<BackendTarget<'_>> {
        match backend {
            BroadcasterBackend::Native => Some(BackendTarget {
                store: &self.controls.native_state_store,
                health: &self.controls.native_stream_health,
            }),
            BroadcasterBackend::Vm if self.controls.enable_vm_pools => Some(BackendTarget {
                store: &self.controls.vm_state_store,
                health: &self.controls.vm_stream_health,
            }),
            BroadcasterBackend::Vm => None,
        }
    }
}

struct BackendTarget<'a> {
    store: &'a Arc<StateStore>,
    health: &'a Arc<StreamHealth>,
}

async fn process_broadcaster_subscription(
    mut stream: impl futures::Stream<Item = Result<Message, WsError>> + Unpin,
    controls: &BroadcasterSubscriptionControls,
    cfg: &StreamSupervisorConfig,
    vm_rebuild: Option<VmSubscriptionRebuildState>,
) -> (SubscriptionExit, Option<VmSubscriptionRebuildState>) {
    let mut processor = BroadcasterSubscriptionProcessor::new(controls.clone(), vm_rebuild);

    loop {
        let next_timeout = if processor.bootstrap_complete() {
            cfg.stream_stale
        } else {
            cfg.readiness_stale
        };

        let Ok(next_message) = timeout(next_timeout, stream.next()).await else {
            let reason = if processor.bootstrap_complete() {
                "live message timeout from broadcaster"
            } else {
                "bootstrap timeout from broadcaster"
            };
            return (SubscriptionExit::error(reason), processor.vm_rebuild);
        };

        match next_message {
            None => {
                return (
                    SubscriptionExit::error("broadcaster websocket ended"),
                    processor.vm_rebuild,
                )
            }
            Some(Ok(Message::Text(text))) => {
                let envelope: BroadcasterEnvelope = match serde_json::from_str(text.as_ref()) {
                    Ok(envelope) => envelope,
                    Err(error) => {
                        return (
                            SubscriptionExit::error(format!(
                                "failed to decode broadcaster payload: {error}"
                            )),
                            processor.vm_rebuild,
                        )
                    }
                };

                if let Err(error) = processor.observe(envelope).await {
                    return (
                        SubscriptionExit::error(error.to_string()),
                        processor.vm_rebuild,
                    );
                }
            }
            Some(Ok(Message::Binary(_))) => {
                return (
                    SubscriptionExit::error("unexpected binary broadcaster payload"),
                    processor.vm_rebuild,
                );
            }
            Some(Ok(Message::Close(_))) => {
                return (
                    SubscriptionExit::error("broadcaster websocket closed"),
                    processor.vm_rebuild,
                );
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => {
                return (
                    SubscriptionExit::error(error.to_string()),
                    processor.vm_rebuild,
                )
            }
        }
    }
}

async fn handle_subscription_reset(
    controls: &BroadcasterSubscriptionControls,
    last_error: Option<String>,
    vm_rebuild: Option<VmSubscriptionRebuildState>,
) -> Option<VmSubscriptionRebuildState> {
    controls
        .broadcaster_subscription
        .mark_disconnected(last_error.clone())
        .await;
    controls.native_state_store.reset().await;
    controls.vm_state_store.reset().await;
    controls.native_stream_health.increment_restart().await;
    controls.native_stream_health.reset_bursts().await;
    controls
        .native_stream_health
        .set_last_error(last_error.clone())
        .await;
    controls.vm_stream_health.increment_restart().await;
    controls.vm_stream_health.reset_bursts().await;
    controls
        .vm_stream_health
        .set_last_error(last_error.clone())
        .await;

    let mut vm_stream = controls.vm_stream.write().await;
    vm_stream.last_error = last_error;
    drop(vm_stream);

    if !controls.enable_vm_pools {
        return None;
    }

    Some(begin_or_continue_vm_rebuild(controls, vm_rebuild).await)
}

struct VmSubscriptionRebuildState {
    guard: OwnedSemaphorePermit,
}

#[expect(
    clippy::panic,
    reason = "the VM rebuild semaphore should remain available for the process lifetime"
)]
async fn begin_or_continue_vm_rebuild(
    controls: &BroadcasterSubscriptionControls,
    vm_rebuild: Option<VmSubscriptionRebuildState>,
) -> VmSubscriptionRebuildState {
    {
        let mut vm_stream = controls.vm_stream.write().await;
        vm_stream.rebuilding = true;
        vm_stream.restart_count = vm_stream.restart_count.saturating_add(1);
        if vm_stream.rebuild_started_at.is_none() {
            vm_stream.rebuild_started_at = Some(Instant::now());
        }
    }

    if let Some(vm_rebuild) = vm_rebuild {
        return vm_rebuild;
    }

    let Ok(guard) = controls
        .vm_sim_semaphore
        .clone()
        .acquire_many_owned(controls.vm_sim_concurrency)
        .await
    else {
        panic!("vm semaphore closed during broadcaster rebuild");
    };

    if let Err(err) = SHARED_TYCHO_DB.clear() {
        warn!(
            error = %err,
            "Failed clearing TychoDB during broadcaster-driven VM rebuild"
        );
    }

    VmSubscriptionRebuildState { guard }
}

fn snapshot_partition_update(partition: BroadcasterSnapshotPartition) -> Update {
    let mut states = HashMap::new();
    let mut new_pairs = HashMap::new();

    for entry in partition.states {
        states.insert(entry.component_id.clone(), entry.state);
        new_pairs.insert(entry.component_id, entry.component);
    }

    Update::new(partition.block_number, states, new_pairs)
}

fn live_partition_update(partition: BroadcasterUpdatePartition) -> Update {
    let block_number = partition.block_number;
    let mut states: HashMap<String, Box<dyn ProtocolSim>> = HashMap::new();
    let mut new_pairs: HashMap<String, ProtocolComponent> = HashMap::new();
    let mut removed_pairs = HashMap::new();

    for entry in partition.new_pairs {
        states.insert(entry.component_id.clone(), entry.state);
        new_pairs.insert(entry.component_id, entry.component);
    }

    for delta in partition.updated_states {
        states.insert(delta.component_id, delta.state);
    }

    for removed in partition.removed_pairs {
        removed_pairs.insert(removed.component_id, removed.component);
    }

    Update::new(block_number, states, new_pairs).set_removed_pairs(removed_pairs)
}

async fn sleep_backoff(backoff_ms: u64, memory: crate::config::MemoryConfig) {
    maybe_purge_allocator("broadcaster_subscription_restart", memory);
    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
}

fn next_backoff(current: Duration, max: Duration) -> Duration {
    current.saturating_mul(2).min(max)
}

fn jittered_backoff_ms(base: Duration, jitter_pct: f64) -> u64 {
    let base_ms = base.as_millis() as f64;
    let mut rng = rand::thread_rng();
    let jitter = rng.gen_range(-jitter_pct..=jitter_pct);
    let jittered = (base_ms * (1.0 + jitter)).max(0.0);
    jittered.round() as u64
}

struct SubscriptionExit {
    reason: &'static str,
    last_error: Option<String>,
}

impl SubscriptionExit {
    fn error(message: impl Into<String>) -> Self {
        Self {
            reason: "error",
            last_error: Some(message.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Result;
    use num_bigint::BigUint;
    use tokio::sync::RwLock;
    use tycho_simulation::tycho_common::dto::ProtocolStateDelta;
    use tycho_simulation::tycho_common::simulation::errors::{SimulationError, TransitionError};
    use tycho_simulation::{
        protocol::models::ProtocolComponent,
        tycho_common::{
            models::{token::Token, Chain},
            simulation::protocol_sim::{Balances, GetAmountOutResult, ProtocolSim},
            Bytes,
        },
    };

    use super::{
        handle_subscription_reset, BroadcasterSubscriptionControls,
        BroadcasterSubscriptionProcessor,
    };
    use crate::models::state::{BroadcasterSubscriptionStatus, StateStore, VmStreamStatus};
    use crate::models::stream_health::StreamHealth;
    use crate::models::tokens::TokenStore;
    use simulator_core::broadcaster::{
        BroadcasterBackend, BroadcasterBackendHead, BroadcasterEnvelope, BroadcasterHeartbeat,
        BroadcasterPayload, BroadcasterSnapshotChunk, BroadcasterSnapshotEnd,
        BroadcasterSnapshotPartition, BroadcasterSnapshotStart, BroadcasterStateDelta,
        BroadcasterStateEntry, BroadcasterUpdateMessage, BroadcasterUpdatePartition,
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

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
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

    fn controls(enable_vm_pools: bool) -> BroadcasterSubscriptionControls {
        let token_store = Arc::new(TokenStore::new(
            HashMap::new(),
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_millis(10),
        ));

        BroadcasterSubscriptionControls {
            broadcaster_subscription: BroadcasterSubscriptionStatus::default(),
            native_state_store: Arc::new(StateStore::new(Arc::clone(&token_store))),
            vm_state_store: Arc::new(StateStore::new(token_store)),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
            vm_sim_semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
            vm_sim_concurrency: 1,
            enable_vm_pools,
        }
    }

    fn native_component() -> ProtocolComponent {
        ProtocolComponent::new(
            Bytes::from([1u8; 20]),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![dummy_token(2, "TKNA"), dummy_token(3, "TKNB")],
            Vec::new(),
            HashMap::new(),
            Bytes::from([9u8; 32]),
            chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
                .unwrap_or_else(|| unreachable!("valid timestamp"))
                .naive_utc(),
        )
    }

    fn vm_component() -> ProtocolComponent {
        ProtocolComponent::new(
            Bytes::from([4u8; 20]),
            "vm:curve".to_string(),
            "curve_pool".to_string(),
            Chain::Ethereum,
            vec![dummy_token(5, "TKNC"), dummy_token(6, "TKND")],
            Vec::new(),
            HashMap::new(),
            Bytes::from([8u8; 32]),
            chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
                .unwrap_or_else(|| unreachable!("valid timestamp"))
                .naive_utc(),
        )
    }

    fn dummy_token(seed: u8, symbol: &str) -> Token {
        let address = Bytes::from([seed; 20]);
        Token::new(&address, symbol, 18, 0, &[], Chain::Ethereum, 100)
    }

    fn snapshot_start_envelope() -> Result<BroadcasterEnvelope> {
        Ok(BroadcasterEnvelope::new(
            "stream-1",
            1,
            BroadcasterPayload::SnapshotStart(BroadcasterSnapshotStart::new(
                "snapshot-1",
                1,
                vec![BroadcasterBackend::Native, BroadcasterBackend::Vm],
                1,
            )?),
        ))
    }

    fn snapshot_chunk_envelope() -> Result<BroadcasterEnvelope> {
        Ok(BroadcasterEnvelope::new(
            "stream-1",
            2,
            BroadcasterPayload::SnapshotChunk(BroadcasterSnapshotChunk::new(
                "snapshot-1",
                0,
                vec![
                    BroadcasterSnapshotPartition::new(
                        BroadcasterBackend::Native,
                        10,
                        vec![BroadcasterStateEntry::new(
                            "pool-native",
                            native_component(),
                            Box::new(DummySim(1)),
                        )],
                        BTreeMap::new(),
                    ),
                    BroadcasterSnapshotPartition::new(
                        BroadcasterBackend::Vm,
                        11,
                        vec![BroadcasterStateEntry::new(
                            "pool-vm",
                            vm_component(),
                            Box::new(DummySim(2)),
                        )],
                        BTreeMap::new(),
                    ),
                ],
            )?),
        ))
    }

    fn snapshot_end_envelope() -> BroadcasterEnvelope {
        BroadcasterEnvelope::new(
            "stream-1",
            3,
            BroadcasterPayload::SnapshotEnd(BroadcasterSnapshotEnd::new("snapshot-1")),
        )
    }

    fn update_envelope() -> Result<BroadcasterEnvelope> {
        Ok(BroadcasterEnvelope::new(
            "stream-1",
            4,
            BroadcasterPayload::Update(BroadcasterUpdateMessage::new(vec![
                BroadcasterUpdatePartition::new(
                    BroadcasterBackend::Native,
                    12,
                    Vec::new(),
                    vec![BroadcasterStateDelta::new(
                        "pool-native",
                        BroadcasterBackend::Native,
                        Box::new(DummySim(3)),
                    )],
                    Vec::new(),
                    BTreeMap::new(),
                ),
            ])?),
        ))
    }

    fn heartbeat_envelope() -> Result<BroadcasterEnvelope> {
        Ok(BroadcasterEnvelope::new(
            "stream-1",
            4,
            BroadcasterPayload::Heartbeat(BroadcasterHeartbeat::new(
                1,
                "snapshot-1",
                vec![
                    BroadcasterBackendHead::new(BroadcasterBackend::Native, 14),
                    BroadcasterBackendHead::new(BroadcasterBackend::Vm, 15),
                ],
            )?),
        ))
    }

    #[tokio::test]
    async fn snapshot_bootstrap_populates_native_and_vm_separately() -> Result<()> {
        let controls = controls(true);
        let mut processor = BroadcasterSubscriptionProcessor::new(controls.clone(), None);

        processor.observe(snapshot_start_envelope()?).await?;
        processor.observe(snapshot_chunk_envelope()?).await?;

        assert!(controls.native_state_store.has_pool("pool-native").await);
        assert!(controls.vm_state_store.has_pool("pool-vm").await);
        assert!(
            !controls
                .broadcaster_subscription
                .snapshot()
                .await
                .bootstrap_complete
        );

        processor.observe(snapshot_end_envelope()).await?;

        let snapshot = controls.broadcaster_subscription.snapshot().await;
        assert!(snapshot.connected);
        assert!(snapshot.bootstrap_complete);
        assert_eq!(controls.native_state_store.current_block().await, 10);
        assert_eq!(controls.vm_state_store.current_block().await, 11);
        assert!(controls
            .native_stream_health
            .last_update_age_ms()
            .await
            .is_some());
        assert!(controls
            .vm_stream_health
            .last_update_age_ms()
            .await
            .is_some());
        Ok(())
    }

    #[tokio::test]
    async fn heartbeat_refreshes_backend_blocks_without_new_state() -> Result<()> {
        let controls = controls(true);
        let mut processor = BroadcasterSubscriptionProcessor::new(controls.clone(), None);

        processor.observe(snapshot_start_envelope()?).await?;
        processor.observe(snapshot_chunk_envelope()?).await?;
        processor.observe(snapshot_end_envelope()).await?;
        processor.observe(heartbeat_envelope()?).await?;

        assert_eq!(controls.native_state_store.current_block().await, 14);
        assert_eq!(controls.vm_state_store.current_block().await, 15);
        assert!(controls.native_state_store.has_pool("pool-native").await);
        assert!(controls.vm_state_store.has_pool("pool-vm").await);
        Ok(())
    }

    #[tokio::test]
    async fn live_update_keeps_native_and_vm_partitioned() -> Result<()> {
        let controls = controls(true);
        let mut processor = BroadcasterSubscriptionProcessor::new(controls.clone(), None);

        processor.observe(snapshot_start_envelope()?).await?;
        processor.observe(snapshot_chunk_envelope()?).await?;
        processor.observe(snapshot_end_envelope()).await?;
        processor.observe(update_envelope()?).await?;

        assert_eq!(controls.native_state_store.current_block().await, 12);
        assert_eq!(controls.vm_state_store.current_block().await, 11);
        assert!(controls.native_state_store.has_pool("pool-native").await);
        assert!(controls.vm_state_store.has_pool("pool-vm").await);
        Ok(())
    }

    #[tokio::test]
    async fn subscription_reset_clears_ingest_and_bootstrap_state() -> Result<()> {
        let controls = controls(true);
        let mut processor = BroadcasterSubscriptionProcessor::new(controls.clone(), None);

        controls.broadcaster_subscription.mark_connected().await;
        controls.native_stream_health.mark_started().await;
        controls.vm_stream_health.mark_started().await;

        processor.observe(snapshot_start_envelope()?).await?;
        processor.observe(snapshot_chunk_envelope()?).await?;
        processor.observe(snapshot_end_envelope()).await?;

        let vm_rebuild =
            handle_subscription_reset(&controls, Some("broadcaster dropped".to_string()), None)
                .await;

        let broadcaster_snapshot = controls.broadcaster_subscription.snapshot().await;
        assert!(!broadcaster_snapshot.connected);
        assert!(!broadcaster_snapshot.bootstrap_complete);
        assert_eq!(broadcaster_snapshot.snapshot_id, None);
        assert_eq!(broadcaster_snapshot.restart_count, 1);
        assert_eq!(
            broadcaster_snapshot.last_error.as_deref(),
            Some("broadcaster dropped")
        );

        assert_eq!(controls.native_state_store.current_block().await, 0);
        assert_eq!(controls.vm_state_store.current_block().await, 0);
        assert!(!controls.native_state_store.has_pool("pool-native").await);
        assert!(!controls.vm_state_store.has_pool("pool-vm").await);
        assert!(!controls.native_state_store.is_ready());
        assert!(!controls.vm_state_store.is_ready());

        assert_eq!(controls.native_stream_health.restart_count().await, 1);
        assert_eq!(controls.vm_stream_health.restart_count().await, 1);
        assert_eq!(
            controls.native_stream_health.last_error().await.as_deref(),
            Some("broadcaster dropped")
        );
        assert_eq!(
            controls.vm_stream_health.last_error().await.as_deref(),
            Some("broadcaster dropped")
        );

        let vm_stream = controls.vm_stream.read().await;
        assert!(vm_stream.rebuilding);
        assert_eq!(vm_stream.restart_count, 1);
        assert_eq!(vm_stream.last_error.as_deref(), Some("broadcaster dropped"));
        assert!(vm_stream.rebuild_started_at.is_some());
        drop(vm_stream);

        let mut processor = BroadcasterSubscriptionProcessor::new(controls.clone(), vm_rebuild);
        processor.observe(snapshot_start_envelope()?).await?;
        processor.observe(snapshot_chunk_envelope()?).await?;
        processor.observe(snapshot_end_envelope()).await?;

        let vm_stream = controls.vm_stream.read().await;
        assert!(!vm_stream.rebuilding);
        assert!(vm_stream.rebuild_started_at.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn vm_partitions_are_ignored_when_vm_pools_are_disabled() -> Result<()> {
        let controls = controls(false);
        let mut processor = BroadcasterSubscriptionProcessor::new(controls.clone(), None);

        controls.broadcaster_subscription.mark_connected().await;
        controls.native_stream_health.mark_started().await;

        processor.observe(snapshot_start_envelope()?).await?;
        processor.observe(snapshot_chunk_envelope()?).await?;

        assert!(controls.native_state_store.has_pool("pool-native").await);
        assert!(!controls.vm_state_store.has_pool("pool-vm").await);
        assert_eq!(controls.native_state_store.current_block().await, 10);
        assert_eq!(controls.vm_state_store.current_block().await, 0);

        processor.observe(snapshot_end_envelope()).await?;

        let broadcaster_snapshot = controls.broadcaster_subscription.snapshot().await;
        assert!(processor.bootstrap_complete());
        assert!(broadcaster_snapshot.bootstrap_complete);
        assert!(broadcaster_snapshot.connected);
        assert_eq!(controls.native_stream_health.last_block().await, 10);
        assert_eq!(controls.vm_stream_health.last_block().await, 0);
        assert!(controls
            .native_stream_health
            .last_update_age_ms()
            .await
            .is_some());
        assert!(controls
            .vm_stream_health
            .last_update_age_ms()
            .await
            .is_none());
        Ok(())
    }
}
