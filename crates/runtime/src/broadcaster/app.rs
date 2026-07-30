use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};
use tycho_simulation::utils::load_all_tokens;

use crate::broadcaster::redis_publisher::{
    BroadcasterDeploymentAdmissionSnapshot, BroadcasterRedisPublisher,
    BroadcasterRedisPublisherConfig, BroadcasterRedisPublisherMode,
    BroadcasterRedisPublisherStatus, TokioRedisStreamWriter,
};
use crate::broadcaster::service::{
    BroadcasterServiceState, SnapshotBoundaryRetention, SnapshotSessionError,
};
use crate::broadcaster::state::{
    BroadcasterReadiness, BroadcasterSnapshotCache, BroadcasterStatusSnapshot,
    BroadcasterUpstreamState,
};
use crate::broadcaster::state_history::{build_state_history_runtime, StateHistoryRuntime};
use crate::config::{
    init_logging, load_broadcaster_config, load_broadcaster_redis_config,
    load_state_history_config, BroadcasterConfig, MemoryConfig,
};
use crate::memory::maybe_log_memory_snapshot;
use crate::metrics::emit_broadcaster_health_snapshot;
use crate::models::stream_health::StreamHealth;
use crate::models::tokens::{TokenStore, TokenStoreError};
use crate::services::rfq_tokens::{load_rfq_token_stores, RfqTokenStoreConfig};
use crate::services::stream_builder::{
    build_broadcaster_raw_stream, build_rfq_stream, BroadcasterProtocols, RFQConfig, RFQTokenStores,
};
use crate::stream::{
    supervise_broadcaster_raw_stream, supervise_broadcaster_stream, BroadcasterStreamControls,
    StreamSupervisorConfig,
};
use simulator_core::broadcaster::{
    BroadcasterBackend, BroadcasterEnvelope, BroadcasterSnapshotSessionResponse,
    BroadcasterTokenDto, BroadcasterTokenLookupResponse, BroadcasterTokenSnapshotResponse,
};
use state_history::{CheckpointKind, StreamPosition};
use tokio_util::sync::CancellationToken;
use tycho_simulation::tycho_common::Bytes;

#[derive(Clone)]
pub struct BroadcasterAppState {
    raw_service: BroadcasterServiceState,
    rfq_service: Option<BroadcasterServiceState>,
    redis_publisher: Arc<BroadcasterRedisPublisher>,
    tokens: Arc<TokenStore>,
    chain_id: u64,
    snapshot_session_ttl: Duration,
    last_reported_readiness: Arc<tokio::sync::Mutex<Option<BroadcasterReadiness>>>,
    state_history: Option<Arc<StateHistoryRuntime>>,
}

impl BroadcasterAppState {
    pub fn with_snapshot_session_ttl(
        raw_service: BroadcasterServiceState,
        rfq_service: Option<BroadcasterServiceState>,
        tokens: Arc<TokenStore>,
        chain_id: u64,
        snapshot_session_ttl: Duration,
        redis_publisher: Arc<BroadcasterRedisPublisher>,
    ) -> Self {
        Self {
            raw_service,
            rfq_service,
            redis_publisher,
            tokens,
            chain_id,
            snapshot_session_ttl,
            last_reported_readiness: Arc::new(tokio::sync::Mutex::new(None)),
            state_history: None,
        }
    }

    pub fn with_state_history(mut self, state_history: Option<Arc<StateHistoryRuntime>>) -> Self {
        self.state_history = state_history;
        self
    }

    pub async fn create_snapshot_session(
        &self,
    ) -> Result<Option<BroadcasterSnapshotSessionResponse>> {
        let mut services = vec![self.raw_service.clone()];
        if let Some(rfq_service) = &self.rfq_service {
            services.push(rfq_service.clone());
        }
        BroadcasterServiceState::create_snapshot_session_for_services(
            &services,
            self.snapshot_session_ttl,
        )
        .await
    }

    pub async fn snapshot_session_payload(
        &self,
        session_id: u64,
        index: u32,
    ) -> Result<BroadcasterEnvelope, SnapshotSessionError> {
        self.raw_service
            .snapshot_session_payload(session_id, index)
            .await
    }

    pub async fn status_snapshot(&self) -> BroadcasterStatusSnapshot {
        let redis_status = self.redis_publisher.status_snapshot().await;
        self.snapshot_with_redis_status(redis_status).await
    }

    pub async fn readiness_snapshot(&self) -> BroadcasterStatusSnapshot {
        let redis_status = self.redis_publisher.verified_status_snapshot().await;
        let mut snapshot = self.snapshot_with_redis_status(redis_status).await;
        if !snapshot.deployment_admission.admitted
            && snapshot.readiness == BroadcasterReadiness::Ready
        {
            snapshot.readiness = BroadcasterReadiness::SnapshotWarmingUp;
        }
        if snapshot.deployment_admission.admitted
            && snapshot
                .redis_publisher
                .as_ref()
                .is_some_and(|publisher| publisher.mode == "active")
        {
            let retention = self.raw_service.snapshot_boundary_is_retained().await;
            if !retention.is_retained() {
                snapshot.snapshot.exportable = false;
                if snapshot.readiness == BroadcasterReadiness::Ready {
                    snapshot.readiness = BroadcasterReadiness::SnapshotUnexportable;
                }
                if let SnapshotBoundaryRetention::InspectionFailed(error) = retention {
                    snapshot.snapshot.last_export_error = Some(error);
                }
            }
        }
        self.log_readiness_transition(&snapshot).await;
        snapshot
    }

    async fn snapshot_with_redis_status(
        &self,
        redis_status: BroadcasterRedisPublisherStatus,
    ) -> BroadcasterStatusSnapshot {
        let mut snapshot = self.raw_service.status_snapshot().await;
        if let Some(rfq_service) = &self.rfq_service {
            snapshot = combine_status_snapshots(snapshot, rfq_service.status_snapshot().await);
        }
        let redis_readiness = redis_readiness(redis_status.mode);
        match snapshot.readiness {
            BroadcasterReadiness::Ready => snapshot.readiness = redis_readiness,
            BroadcasterReadiness::SnapshotUnexportable
            | BroadcasterReadiness::UpstreamRecovering
                if redis_readiness != BroadcasterReadiness::Ready =>
            {
                snapshot.readiness = redis_readiness;
            }
            _ => {}
        }
        snapshot.redis_publisher = Some(redis_status);
        snapshot.deployment_admission = self.redis_publisher.deployment_admission_snapshot();
        snapshot.state_history = self
            .state_history
            .as_ref()
            .map(|state_history| state_history.handle.status());
        snapshot
    }

    async fn log_readiness_transition(&self, snapshot: &BroadcasterStatusSnapshot) {
        let mut previous = self.last_reported_readiness.lock().await;
        let previous_value = *previous;
        if previous_value != Some(snapshot.readiness) {
            let previous_readiness = previous_value.map(BroadcasterReadiness::as_str);
            if snapshot.readiness == BroadcasterReadiness::Ready {
                tracing::info!(
                    event = "broadcaster_readiness_changed",
                    from = previous_readiness,
                    to = snapshot.readiness.as_str(),
                    "Broadcaster readiness opened"
                );
            } else {
                tracing::warn!(
                    event = "broadcaster_readiness_changed",
                    from = previous_readiness,
                    to = snapshot.readiness.as_str(),
                    error = snapshot
                        .upstream
                        .last_error
                        .as_deref()
                        .or_else(|| snapshot.redis_publisher.as_ref()?.last_error.as_deref())
                        .or(snapshot.snapshot.last_export_error.as_deref()),
                    "Broadcaster readiness closed"
                );
            }
            *previous = Some(snapshot.readiness);
        }
    }

    pub fn deployment_admission_snapshot(&self) -> BroadcasterDeploymentAdmissionSnapshot {
        self.redis_publisher.deployment_admission_snapshot()
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub fn begin_shutdown(&self) {
        self.redis_publisher.begin_shutdown();
    }

    pub async fn shutdown_state_history(&self) {
        if let Some(state_history) = &self.state_history {
            state_history.stop_checkpoint_task().await;
            state_history
                .handle
                .clone()
                .shutdown(Duration::from_secs(10))
                .await;
        }
    }

    pub async fn lookup_tokens(
        &self,
        addresses: Vec<Bytes>,
    ) -> Result<BroadcasterTokenLookupResponse, TokenStoreError> {
        let mut tokens = Vec::new();
        let mut missing = Vec::new();

        for address in addresses {
            match self.tokens.ensure(&address).await? {
                Some(token) => tokens.push(BroadcasterTokenDto::from(token)),
                None => missing.push(address),
            }
        }

        Ok(BroadcasterTokenLookupResponse { tokens, missing })
    }

    pub async fn token_snapshot(&self) -> BroadcasterTokenSnapshotResponse {
        let tokens = self
            .tokens
            .snapshot()
            .await
            .into_values()
            .map(BroadcasterTokenDto::from)
            .collect();

        BroadcasterTokenSnapshotResponse {
            chain_id: self.chain_id,
            tokens,
        }
    }
}

pub struct BroadcasterServiceParts {
    pub config: BroadcasterConfig,
    pub app_state: BroadcasterAppState,
    pub supervisors: Vec<tokio::task::JoinHandle<()>>,
}

#[expect(
    clippy::too_many_lines,
    reason = "shared publisher, cache, and task ownership are assembled in one startup path"
)]
pub async fn build_broadcaster_service() -> Result<BroadcasterServiceParts> {
    init_logging();

    let config = load_broadcaster_config();
    let chain = config.chain_profile.chain;
    info!(chain_id = chain.id(), chain = %chain, "Initializing Tycho broadcaster...");
    log_memory_config(config.memory);

    let tokens = load_token_store(&config).await?;
    let raw_backends = raw_configured_backends(&config);
    let heartbeat_interval = Duration::from_secs(config.tuning.heartbeat_interval_secs);
    let (state_history_config, state_history) = initialize_state_history(&config).await?;
    let redis_publisher =
        build_redis_publisher(&config, heartbeat_interval, state_history.as_ref()).await?;
    let token_min_quality = u32::try_from(config.tuning.token_min_quality)
        .context("BROADCASTER_TOKEN_MIN_QUALITY must fit u32")?;
    let raw_cache = BroadcasterSnapshotCache::with_token_catalog(
        chain.id(),
        raw_backends.clone(),
        Arc::clone(&tokens),
        token_min_quality,
    );
    let raw_upstream_state = BroadcasterUpstreamState::default();
    let rfq_backends = rfq_configured_backends(&config);
    let rfq_cache = rfq_backends
        .as_ref()
        .map(|backends| BroadcasterSnapshotCache::new(chain.id(), backends.clone()));
    let rfq_upstream_state = rfq_cache
        .as_ref()
        .map(|_| BroadcasterUpstreamState::default());
    let publication_gate = Arc::new(tokio::sync::Mutex::new(()));
    let recovery_retry_backoff = Duration::from_millis(config.stream_restart_backoff_min_ms);
    let native_progress_lease =
        Duration::from_secs(config.chain_profile.native_progress_lease_secs);
    let raw_service = BroadcasterServiceState::with_lifecycle_gate_and_recovery_backoff_and_lease(
        config.tuning.snapshot_max_payload_bytes,
        raw_cache,
        raw_upstream_state,
        Arc::clone(&redis_publisher),
        Arc::clone(&publication_gate),
        recovery_retry_backoff,
        native_progress_lease,
    );
    let raw_health = Arc::new(StreamHealth::new());
    let supervisor_cfg = build_supervisor_config(&config);
    let mut supervisors = Vec::new();

    let rfq_service = if let (Some(rfq_cache), Some(rfq_upstream_state)) =
        (rfq_cache, rfq_upstream_state)
    {
        let service = BroadcasterServiceState::with_lifecycle_gate_and_recovery_backoff_and_lease(
            config.tuning.snapshot_max_payload_bytes,
            rfq_cache,
            rfq_upstream_state,
            Arc::clone(&redis_publisher),
            Arc::clone(&publication_gate),
            recovery_retry_backoff,
            native_progress_lease,
        );
        let rfq_token_stores = load_rfq_token_stores(RfqTokenStoreConfig {
            tokens: Arc::clone(&tokens),
            chain,
            token_refresh_timeout: Duration::from_millis(config.token_refresh_timeout_ms),
            protocols: &config.chain_profile.rfq_protocols,
            bebop_url: &config.bebop_url,
            hashflow_filename: &config.hashflow_filename,
            liquorice_url: config.liquorice_url.as_deref(),
            liquorice_user: &config.liquorice_user,
            liquorice_key: &config.liquorice_key,
        })
        .await?;
        supervisors.push(spawn_broadcaster_rfq_stream_task(
            &config,
            supervisor_cfg.clone(),
            service.clone(),
            rfq_token_stores,
        ));
        Some(service)
    } else {
        None
    };
    let generation_services = match &rfq_service {
        Some(rfq_service) => vec![raw_service.clone(), rfq_service.clone()],
        None => vec![raw_service.clone()],
    };
    // New broadcasters warm their caches while passive. Readiness stays closed
    // until this local promotion loop wins the writer fence.
    spawn_promotion_task(
        generation_services.clone(),
        Duration::from_secs(1),
        state_history.clone(),
    );
    spawn_snapshot_artifact_refresh_task(generation_services.clone(), Duration::from_secs(5 * 60));
    start_state_history_checkpoint_task(
        &generation_services,
        state_history_config.as_ref(),
        state_history.as_ref(),
    )?;
    supervisors.push(spawn_broadcaster_stream_task(
        &config,
        supervisor_cfg.clone(),
        Arc::clone(&raw_health),
        raw_service.clone(),
    ));
    spawn_heartbeat_task(generation_services, heartbeat_interval);
    let snapshot_session_ttl = Duration::from_secs(config.tuning.snapshot_session_ttl_secs);
    let app_state = BroadcasterAppState::with_snapshot_session_ttl(
        raw_service,
        rfq_service,
        tokens,
        chain.id(),
        snapshot_session_ttl,
        redis_publisher,
    )
    .with_state_history(state_history);
    spawn_broadcaster_health_snapshot_task(app_state.clone(), heartbeat_interval);

    Ok(BroadcasterServiceParts {
        config,
        app_state,
        supervisors,
    })
}

async fn initialize_state_history(
    config: &BroadcasterConfig,
) -> Result<(
    Option<crate::config::StateHistoryConfig>,
    Option<Arc<StateHistoryRuntime>>,
)> {
    let state_history_config = load_state_history_config();
    let state_history = match state_history_config.as_ref() {
        Some(state_history_config) => {
            build_state_history_runtime(state_history_config, config.rpc_url.clone())
                .await?
                .map(Arc::new)
        }
        None => None,
    };
    Ok((state_history_config, state_history))
}

fn combine_status_snapshots(
    mut raw: BroadcasterStatusSnapshot,
    rfq: BroadcasterStatusSnapshot,
) -> BroadcasterStatusSnapshot {
    raw.snapshot
        .configured_backends
        .extend(rfq.snapshot.configured_backends);
    raw.snapshot.configured_backends.sort();
    raw.snapshot.configured_backends.dedup();
    raw.snapshot.total_states = raw
        .snapshot
        .total_states
        .saturating_add(rfq.snapshot.total_states);
    raw.snapshot_sessions.active = raw
        .snapshot_sessions
        .active
        .saturating_add(rfq.snapshot_sessions.active);
    raw.snapshot_sessions.last_error = raw
        .snapshot_sessions
        .last_error
        .or(rfq.snapshot_sessions.last_error);
    raw.backends.extend(rfq.backends);
    raw
}

fn redis_readiness(mode: &str) -> BroadcasterReadiness {
    match mode {
        "active" => BroadcasterReadiness::Ready,
        "passive" => BroadcasterReadiness::RedisPublisherPassive,
        "retired" => BroadcasterReadiness::RedisPublisherRetired,
        _ => BroadcasterReadiness::RedisPublisherUnhealthy,
    }
}

async fn build_redis_publisher(
    config: &BroadcasterConfig,
    heartbeat_interval: Duration,
    state_history: Option<&Arc<StateHistoryRuntime>>,
) -> Result<Arc<BroadcasterRedisPublisher>> {
    let redis_config = load_broadcaster_redis_config();
    let writer = Arc::new(TokioRedisStreamWriter::connect(&redis_config.redis_url).await?);
    let publisher = BroadcasterRedisPublisher::new(
        BroadcasterRedisPublisherConfig::from_redis_config(
            &redis_config,
            config.chain_profile.chain.id(),
            heartbeat_interval,
            config.chain_profile.recovery_max_buffered_native_blocks,
        ),
        writer,
    );
    let publisher = match state_history {
        Some(state_history) => publisher.with_state_history(Arc::clone(state_history)),
        None => publisher,
    };
    Ok(Arc::new(publisher))
}

fn log_memory_config(memory: MemoryConfig) {
    info!(
        event = "memory_config",
        purge_enabled = memory.purge_enabled,
        snapshots_enabled = memory.snapshots_enabled,
        min_interval_secs = memory.snapshots_min_interval_secs,
        min_new_pairs = memory.snapshots_min_new_pairs,
        emf_enabled = memory.snapshots_emit_emf,
        "Memory config loaded"
    );
    maybe_log_memory_snapshot("broadcaster", "startup", None, memory, true);
}

async fn load_token_store(config: &BroadcasterConfig) -> Result<Arc<TokenStore>> {
    let chain = config.chain_profile.chain;
    let all_tokens = load_all_tokens(
        &config.tycho_url,
        false,
        Some(&config.api_key),
        true,
        chain,
        Some(config.tuning.token_min_quality),
        None,
    )
    .await?;
    info!("Loaded {} broadcaster tokens", all_tokens.len());

    Ok(Arc::new(TokenStore::new(
        all_tokens,
        config.tycho_url.clone(),
        config.api_key.clone(),
        chain,
        Duration::from_millis(config.token_refresh_timeout_ms),
    )))
}

fn raw_configured_backends(config: &BroadcasterConfig) -> Vec<BroadcasterBackend> {
    let mut backends = Vec::new();
    if !config.chain_profile.native_protocols.is_empty() {
        backends.push(BroadcasterBackend::Native);
    }
    if !config.chain_profile.vm_protocols.is_empty() {
        backends.push(BroadcasterBackend::Vm);
    }
    backends
}

fn rfq_configured_backends(config: &BroadcasterConfig) -> Option<Vec<BroadcasterBackend>> {
    effective_rfq_enabled(config).then_some(vec![BroadcasterBackend::Rfq])
}

fn effective_rfq_enabled(config: &BroadcasterConfig) -> bool {
    config.enable_rfq_pools && !config.chain_profile.rfq_protocols.is_empty()
}

fn build_supervisor_config(config: &BroadcasterConfig) -> StreamSupervisorConfig {
    StreamSupervisorConfig {
        readiness_stale: Duration::from_secs(config.chain_profile.native_progress_lease_secs),
        stream_stale: Duration::from_secs(config.stream_stale_secs),
        missing_block_burst: config.stream_missing_block_burst,
        missing_block_window: Duration::from_secs(config.stream_missing_block_window_secs),
        error_burst: config.stream_error_burst,
        error_window: Duration::from_secs(config.stream_error_window_secs),
        resync_grace: Duration::from_secs(config.resync_grace_secs),
        restart_backoff_min: Duration::from_millis(config.stream_restart_backoff_min_ms),
        restart_backoff_max: Duration::from_millis(config.stream_restart_backoff_max_ms),
        restart_backoff_jitter_pct: config.stream_restart_backoff_jitter_pct,
        memory: config.memory,
    }
}

fn spawn_broadcaster_stream_task(
    config: &BroadcasterConfig,
    supervisor_cfg: StreamSupervisorConfig,
    health: Arc<StreamHealth>,
    service: BroadcasterServiceState,
) -> tokio::task::JoinHandle<()> {
    let chain = config.chain_profile.chain;
    let tycho_url = config.tycho_url.clone();
    let api_key = config.api_key.clone();
    let tvl_threshold = config.tvl_threshold;
    let tvl_keep_threshold = config.tvl_keep_threshold;
    let protocols = BroadcasterProtocols {
        native: config.chain_profile.native_protocols.clone(),
        vm: config.chain_profile.vm_protocols.clone(),
    };

    tokio::spawn(async move {
        info!("Starting broadcaster upstream supervisor...");
        supervise_broadcaster_raw_stream(
            move || {
                let tycho_url = tycho_url.clone();
                let api_key = api_key.clone();
                let protocols = protocols.clone();
                async move {
                    build_broadcaster_raw_stream(
                        &tycho_url,
                        &api_key,
                        tvl_threshold,
                        tvl_keep_threshold,
                        chain,
                        &protocols,
                    )
                    .await
                }
            },
            health,
            supervisor_cfg,
            BroadcasterStreamControls { service },
        )
        .await;
    })
}

fn spawn_broadcaster_rfq_stream_task(
    config: &BroadcasterConfig,
    supervisor_cfg: StreamSupervisorConfig,
    service: BroadcasterServiceState,
    token_stores: RFQTokenStores,
) -> tokio::task::JoinHandle<()> {
    let chain = config.chain_profile.chain;
    let tvl_threshold = config.tvl_threshold;
    let protocols = config.chain_profile.rfq_protocols.clone();
    let rfq_config = RFQConfig {
        bebop_key: config.bebop_key.clone(),
        hashflow_user: config.hashflow_user.clone(),
        hashflow_key: config.hashflow_key.clone(),
        liquorice_user: config.liquorice_user.clone(),
        liquorice_key: config.liquorice_key.clone(),
    };
    let health = Arc::new(StreamHealth::new());

    tokio::spawn(async move {
        info!("Starting broadcaster RFQ stream supervisor...");
        supervise_broadcaster_stream(
            move || {
                let token_stores = token_stores.clone();
                let protocols = protocols.clone();
                let rfq_config = rfq_config.clone();
                async move {
                    build_rfq_stream(tvl_threshold, token_stores, chain, &protocols, rfq_config)
                        .await
                }
            },
            health,
            supervisor_cfg,
            BroadcasterStreamControls { service },
        )
        .await;
    })
}

fn spawn_heartbeat_task(services: Vec<BroadcasterServiceState>, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            for service in &services {
                if let Err(error) = service.broadcast_heartbeat().await {
                    info!(
                        error = %error,
                        "Broadcaster heartbeat failed; keeping the current generation unready"
                    );
                    break;
                }
            }
        }
    });
}

fn spawn_broadcaster_health_snapshot_task(app_state: BroadcasterAppState, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            emit_broadcaster_health_snapshot(&app_state.status_snapshot().await);
        }
    });
}

fn spawn_promotion_task(
    services: Vec<BroadcasterServiceState>,
    interval: Duration,
    state_history: Option<Arc<StateHistoryRuntime>>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        let mut refreshing_recovered_snapshot = false;
        loop {
            ticker.tick().await;
            let mode = services[0].publisher_mode().await;
            let result = match mode {
                BroadcasterRedisPublisherMode::Passive => {
                    BroadcasterServiceState::promote_when_ready(&services, "active_writer_promoted")
                        .await
                }
                BroadcasterRedisPublisherMode::Unhealthy => {
                    BroadcasterServiceState::recover_shared_publisher_when_paused(
                        &services,
                        "active_writer_recovered",
                    )
                    .await
                }
                BroadcasterRedisPublisherMode::Active => {
                    if refreshing_recovered_snapshot {
                        if let Err(error) =
                            BroadcasterServiceState::run_snapshot_export_preflight(&services).await
                        {
                            info!(
                                error = %error,
                                "Recovered broadcaster snapshot refresh is not ready"
                            );
                        }
                        if services[0].has_current_snapshot_artifact().await {
                            refreshing_recovered_snapshot = false;
                            info!("Recovered broadcaster snapshot artifact is ready");
                        }
                    }
                    continue;
                }
                BroadcasterRedisPublisherMode::Retired => return,
            };
            match result {
                Ok(Some(boundary)) => {
                    info!(
                        stream_id = boundary.stream_id.as_str(),
                        snapshot_id = boundary.snapshot_id.as_str(),
                        generation = boundary.generation,
                        "Broadcaster active writer promoted"
                    );
                    if let Some(state_history) = &state_history {
                        let requested_block_number =
                            BroadcasterServiceState::state_history_aligned_head(&services).await;
                        let requested_rfq_observed_at_ms = state_history.rfq_high_water_ms();
                        let position = StreamPosition {
                            generation: boundary.generation,
                            message_seq: boundary.exclusive_message_seq,
                        };
                        let checkpoint = BroadcasterServiceState::capture_state_history_checkpoint(
                            &services,
                            CheckpointKind::Boundary,
                            None,
                            Some(position),
                            requested_block_number,
                            requested_rfq_observed_at_ms,
                        )
                        .await;
                        // Mirrors the recovery commit path: a promotion boundary that never
                        // lands must leave a durable gap, not just a log line.
                        if !matches!(checkpoint, Ok(Some(_))) {
                            state_history.handle.record_boundary_failure_gap(
                                services[0].chain_id(),
                                position,
                                requested_block_number,
                                requested_rfq_observed_at_ms,
                            );
                        }
                        match checkpoint {
                            Ok(Some(_)) => {}
                            Ok(None) => warn!(
                                event = "state_history_boundary_checkpoint_failed",
                                generation = boundary.generation,
                                message_seq = boundary.exclusive_message_seq,
                                "Broadcaster promotion state history checkpoint was discarded"
                            ),
                            Err(error) => warn!(
                                event = "state_history_boundary_checkpoint_failed",
                                generation = boundary.generation,
                                message_seq = boundary.exclusive_message_seq,
                                error = %error,
                                "Broadcaster promotion state history checkpoint failed"
                            ),
                        }
                    }
                    refreshing_recovered_snapshot =
                        mode == BroadcasterRedisPublisherMode::Unhealthy;
                }
                Ok(None) => {}
                Err(error) => {
                    info!(error = %error, "Broadcaster active writer promotion skipped");
                }
            }
        }
    });
}

fn spawn_snapshot_artifact_refresh_task(
    services: Vec<BroadcasterServiceState>,
    interval: Duration,
) {
    tokio::spawn(async move {
        let _ = BroadcasterServiceState::run_snapshot_export_preflight(&services).await;
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let _ = BroadcasterServiceState::run_snapshot_export_preflight(&services).await;
        }
    });
}

fn start_state_history_checkpoint_task(
    services: &[BroadcasterServiceState],
    config: Option<&crate::config::StateHistoryConfig>,
    state_history: Option<&Arc<StateHistoryRuntime>>,
) -> Result<()> {
    if let (Some(config), Some(state_history)) = (config, state_history) {
        spawn_state_history_checkpoint_task(
            services.to_vec(),
            Arc::clone(state_history),
            config.checkpoint_poll_interval,
            config.checkpoint_block_interval,
        )?;
    }
    Ok(())
}

fn spawn_state_history_checkpoint_task(
    services: Vec<BroadcasterServiceState>,
    state_history: Arc<StateHistoryRuntime>,
    poll_interval: Duration,
    block_interval: u64,
) -> Result<()> {
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let mut boundary_requests = state_history.take_boundary_request_receiver()?;
    let join = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_completed = None;
        let mut retry_delay = Duration::from_secs(30);
        let mut retry_after = None;
        loop {
            tokio::select! {
                () = task_cancellation.cancelled() => break,
                request = boundary_requests.recv() => {
                    let Some(request) = request else {
                        break;
                    };
                    let result = BroadcasterServiceState::capture_state_history_checkpoint(
                        &services,
                        CheckpointKind::Boundary,
                        Some(request.state_version),
                        Some(request.position),
                        request.complete_native_block,
                        request.rfq_high_water_ms,
                    )
                    .await;
                    StateHistoryRuntime::respond_to_boundary_request(request, result);
                }
                _ = ticker.tick() => {
                    if retry_after.is_some_and(|deadline| tokio::time::Instant::now() < deadline) {
                        continue;
                    }
                    let Some(current) =
                        BroadcasterServiceState::state_history_aligned_head(&services).await
                    else {
                        continue;
                    };
                    if last_completed
                        .is_some_and(|last| current.saturating_sub(last) < block_interval)
                    {
                        continue;
                    }
                    match BroadcasterServiceState::capture_state_history_checkpoint(
                        &services,
                        CheckpointKind::Interval,
                        None,
                        None,
                        None,
                        None,
                    )
                    .await
                    {
                        Ok(Some(completed)) => {
                            last_completed = Some(completed);
                            retry_delay = Duration::from_secs(30);
                            retry_after = None;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            warn!(
                                event = "state_history_interval_checkpoint_failed",
                                %error,
                                retry_delay_secs = retry_delay.as_secs(),
                                "State history interval checkpoint failed"
                            );
                            retry_after = Some(tokio::time::Instant::now() + retry_delay);
                            retry_delay =
                                retry_delay.saturating_mul(2).min(Duration::from_secs(600));
                        }
                    }
                }
            }
        }
    });
    state_history.set_checkpoint_task(cancellation, join);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::net::{IpAddr, Ipv4Addr};

    use simulator_core::broadcaster::BroadcasterBackend;
    use tycho_simulation::tycho_common::{models::Chain, Bytes};

    use super::{raw_configured_backends, rfq_configured_backends};
    use crate::config::{BroadcasterConfig, BroadcasterTuning, ChainProfile, MemoryConfig};

    fn test_config() -> BroadcasterConfig {
        BroadcasterConfig {
            chain_profile: ChainProfile {
                chain: Chain::Ethereum,
                native_protocols: vec!["uniswap_v2".to_string()],
                vm_protocols: Vec::new(),
                rfq_protocols: vec!["rfq:bebop".to_string()],
                native_progress_lease_secs: 25,
                recovery_max_buffered_native_blocks: 8,
                native_token_protocol_allowlist: Vec::new(),
                reset_allowance_tokens: HashMap::<u64, HashSet<Bytes>>::new(),
                erc4626_pair_policies: Vec::new(),
            },
            tycho_url: "http://localhost:4242".to_string(),
            bebop_url: "https://example.com/bebop".to_string(),
            hashflow_filename: "./hashflow.csv".to_string(),
            liquorice_url: None,
            api_key: "test-api-key".to_string(),
            rpc_url: None,
            tvl_threshold: 100.0,
            tvl_keep_threshold: 20.0,
            port: 3001,
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            enable_rfq_pools: true,
            token_refresh_timeout_ms: 1_000,
            stream_stale_secs: 120,
            stream_missing_block_burst: 3,
            stream_missing_block_window_secs: 60,
            stream_error_burst: 3,
            stream_error_window_secs: 60,
            resync_grace_secs: 60,
            stream_restart_backoff_min_ms: 500,
            stream_restart_backoff_max_ms: 30_000,
            stream_restart_backoff_jitter_pct: 0.2,
            memory: MemoryConfig {
                purge_enabled: true,
                snapshots_enabled: false,
                snapshots_min_interval_secs: 60,
                snapshots_min_new_pairs: 1_000,
                snapshots_emit_emf: false,
            },
            tuning: BroadcasterTuning {
                snapshot_max_payload_bytes: 8_388_608,
                heartbeat_interval_secs: 5,
                token_min_quality: 0,
                snapshot_session_ttl_secs: 300,
            },
            bebop_key: "bebop-key".to_string(),
            hashflow_user: String::new(),
            hashflow_key: String::new(),
            liquorice_user: String::new(),
            liquorice_key: String::new(),
        }
    }

    #[test]
    fn raw_configured_backends_omit_rfq_when_effective() {
        let config = test_config();

        assert_eq!(
            raw_configured_backends(&config),
            vec![BroadcasterBackend::Native]
        );
    }

    #[test]
    fn rfq_configured_backends_include_only_rfq_when_effective() {
        let config = test_config();

        assert_eq!(
            rfq_configured_backends(&config),
            Some(vec![BroadcasterBackend::Rfq])
        );
    }

    #[test]
    fn rfq_configured_backends_are_absent_when_disabled() {
        let mut config = test_config();
        config.enable_rfq_pools = false;

        assert_eq!(rfq_configured_backends(&config), None);
    }
}
