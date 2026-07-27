use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use tracing::warn;

use crate::broadcaster::state::{BroadcasterReadiness, BroadcasterStatusSnapshot};
use crate::models::messages::{QuoteResultQuality, QuoteStatus};
use crate::models::state::{
    SimulatorBackendKind, SimulatorBackendReadiness, SimulatorStatusSnapshot,
};
use simulator_core::broadcaster::BroadcasterBackend;

const METRIC_NAMESPACE: &str = "Tycho/Simulation";
const METRIC_SIMULATE_COMPLETION: &str = "SimulateCompletion";
const METRIC_SIMULATE_RESULT_QUALITY: &str = "SimulateResultQuality";
const METRIC_SIMULATE_TIMEOUT: &str = "SimulateRequestTimeout";
const METRIC_BROADCASTER_REDIS_APPEND_FAILURE: &str = "BroadcasterRedisAppendFailure";
const METRIC_BROADCASTER_WRITER_FENCE_FAILURE: &str = "BroadcasterWriterFenceFailure";
const METRIC_BROADCASTER_RECOVERY_OUTCOME: &str = "BroadcasterRecoveryOutcome";
const METRIC_BROADCASTER_RECOVERY_DURATION: &str = "BroadcasterRecoveryDuration";
const METRIC_BROADCASTER_RECOVERY_ENCODED_BYTES: &str = "BroadcasterRecoveryEncodedBytes";
const METRIC_BROADCASTER_RECOVERY_CHUNK_COUNT: &str = "BroadcasterRecoveryChunkCount";
const METRIC_BROADCASTER_RECOVERY_BUFFER_A_COUNT: &str = "BroadcasterRecoveryBufferACount";
const METRIC_BROADCASTER_RECOVERY_BUFFER_B_COUNT: &str = "BroadcasterRecoveryBufferBCount";
const METRIC_BROADCASTER_SNAPSHOT_EXPORT_FAILURE: &str = "BroadcasterSnapshotExportFailure";
const METRIC_BROADCASTER_REDIS_TRANSPORT_FAILURE: &str = "BroadcasterRedisTransportFailure";
const METRIC_BROADCASTER_REDIS_REBOOTSTRAP: &str = "BroadcasterRedisRebootstrap";
const METRIC_JEMALLOC_ALLOCATED_BYTES: &str = "JemallocAllocatedBytes";
const METRIC_JEMALLOC_RESIDENT_BYTES: &str = "JemallocResidentBytes";
const DIM_STATUS: &str = "Status";
const DIM_TIMED_OUT: &str = "TimedOut";
const DIM_TIMEOUT_KIND: &str = "TimeoutKind";
const DIM_RESULT_QUALITY: &str = "ResultQuality";
const DIM_LABEL: &str = "Label";
const DIM_OPERATION: &str = "Operation";
const DIM_REASON: &str = "Reason";
const DIM_OUTCOME: &str = "Outcome";

const UNIT_COUNT: &str = "Count";
const UNIT_MILLISECONDS: &str = "Milliseconds";

#[derive(Debug, Clone, Copy)]
pub enum TimeoutKind {
    RequestGuard,
    RouterBoundary,
}

impl TimeoutKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RequestGuard => "request_guard",
            Self::RouterBoundary => "router_boundary",
        }
    }
}

pub fn emit_simulate_completion(status: QuoteStatus, timed_out: bool) {
    emit_count_metric(
        METRIC_SIMULATE_COMPLETION,
        &[
            (DIM_STATUS, json!(status)),
            (
                DIM_TIMED_OUT,
                json!(if timed_out { "true" } else { "false" }),
            ),
        ],
    );
}

pub fn emit_simulate_timeout(kind: TimeoutKind) {
    emit_count_metric(
        METRIC_SIMULATE_TIMEOUT,
        &[(DIM_TIMEOUT_KIND, json!(kind.as_str()))],
    );
}

pub fn emit_simulate_result_quality(quality: QuoteResultQuality) {
    emit_count_metric(
        METRIC_SIMULATE_RESULT_QUALITY,
        &[(DIM_RESULT_QUALITY, json!(quality))],
    );
}

pub fn emit_broadcaster_redis_append_failure() {
    emit_count_metric(METRIC_BROADCASTER_REDIS_APPEND_FAILURE, &[]);
}

pub fn emit_broadcaster_writer_fence_failure() {
    emit_count_metric(METRIC_BROADCASTER_WRITER_FENCE_FAILURE, &[]);
}

pub fn emit_broadcaster_snapshot_export_failure() {
    emit_count_metric(METRIC_BROADCASTER_SNAPSHOT_EXPORT_FAILURE, &[]);
}

pub fn emit_broadcaster_recovery_outcome(
    outcome: &'static str,
    duration_ms: u64,
    encoded_bytes: usize,
    chunk_count: usize,
    buffer_a_count: usize,
    buffer_b_count: usize,
) {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let aws = json!({
        "Timestamp": timestamp_ms,
        "CloudWatchMetrics": [{
            "Namespace": METRIC_NAMESPACE,
            "Dimensions": [[DIM_OUTCOME]],
            "Metrics": [
                { "Name": METRIC_BROADCASTER_RECOVERY_OUTCOME, "Unit": "Count" },
                { "Name": METRIC_BROADCASTER_RECOVERY_DURATION, "Unit": "Milliseconds" },
                { "Name": METRIC_BROADCASTER_RECOVERY_ENCODED_BYTES, "Unit": "Bytes" },
                { "Name": METRIC_BROADCASTER_RECOVERY_CHUNK_COUNT, "Unit": "Count" },
                { "Name": METRIC_BROADCASTER_RECOVERY_BUFFER_A_COUNT, "Unit": "Count" },
                { "Name": METRIC_BROADCASTER_RECOVERY_BUFFER_B_COUNT, "Unit": "Count" },
            ],
        }],
    });
    let mut event = Map::new();
    event.insert("_aws".to_string(), aws);
    event.insert(DIM_OUTCOME.to_string(), json!(outcome));
    event.insert(METRIC_BROADCASTER_RECOVERY_OUTCOME.to_string(), json!(1));
    event.insert(
        METRIC_BROADCASTER_RECOVERY_DURATION.to_string(),
        json!(duration_ms),
    );
    event.insert(
        METRIC_BROADCASTER_RECOVERY_ENCODED_BYTES.to_string(),
        json!(encoded_bytes as u64),
    );
    event.insert(
        METRIC_BROADCASTER_RECOVERY_CHUNK_COUNT.to_string(),
        json!(chunk_count as u64),
    );
    event.insert(
        METRIC_BROADCASTER_RECOVERY_BUFFER_A_COUNT.to_string(),
        json!(buffer_a_count as u64),
    );
    event.insert(
        METRIC_BROADCASTER_RECOVERY_BUFFER_B_COUNT.to_string(),
        json!(buffer_b_count as u64),
    );

    match serde_json::to_string(&Value::Object(event)) {
        Ok(line) => println!("{line}"),
        Err(error) => warn!(
            error = %error,
            metric = "broadcaster_recovery_outcome",
            "Failed to serialize EMF metric"
        ),
    }
}

pub fn emit_broadcaster_redis_transport_failure(operation: &'static str) {
    emit_count_metric(
        METRIC_BROADCASTER_REDIS_TRANSPORT_FAILURE,
        &[(DIM_OPERATION, json!(operation))],
    );
}

pub fn emit_broadcaster_redis_rebootstrap(reason: &'static str) {
    emit_count_metric(
        METRIC_BROADCASTER_REDIS_REBOOTSTRAP,
        &[(DIM_REASON, json!(reason))],
    );
}

pub fn emit_broadcaster_health_snapshot(snapshot: &BroadcasterStatusSnapshot) {
    let native = snapshot.backends.get(&BroadcasterBackend::Native);
    let vm = snapshot.backends.get(&BroadcasterBackend::Vm);
    let rfq = snapshot.backends.get(&BroadcasterBackend::Rfq);
    let mut metrics = vec![
        (
            "BroadcasterNativeReady",
            u64::from(snapshot.readiness == BroadcasterReadiness::Ready),
            UNIT_COUNT,
        ),
        (
            "BroadcasterTychoConnected",
            u64::from(snapshot.upstream.connected),
            UNIT_COUNT,
        ),
        (
            "BroadcasterTychoRestartCount",
            snapshot.upstream.restart_count,
            UNIT_COUNT,
        ),
        (
            "BroadcasterRecoveryActive",
            u64::from(snapshot.recovery.active),
            UNIT_COUNT,
        ),
        (
            "BroadcasterRecoveryBufferACount",
            snapshot.recovery.buffer_a_count as u64,
            UNIT_COUNT,
        ),
        (
            "BroadcasterRecoveryBufferBCount",
            snapshot.recovery.buffer_b_count as u64,
            UNIT_COUNT,
        ),
        (
            "BroadcasterNativeAvailable",
            u64::from(native.and_then(|status| status.block_number).is_some()),
            UNIT_COUNT,
        ),
        (
            "BroadcasterVmAvailable",
            u64::from(vm.and_then(|status| status.block_number).is_some()),
            UNIT_COUNT,
        ),
        (
            "BroadcasterRfqAvailable",
            u64::from(rfq.and_then(|status| status.block_number).is_some()),
            UNIT_COUNT,
        ),
    ];
    push_optional_metric(
        &mut metrics,
        "BroadcasterExposedStateAge",
        snapshot.upstream.last_update_age_ms,
        UNIT_MILLISECONDS,
    );
    push_optional_metric(
        &mut metrics,
        "BroadcasterPublishedNativeHead",
        native.and_then(|status| status.block_number),
        UNIT_COUNT,
    );
    push_optional_metric(
        &mut metrics,
        "BroadcasterRecoveryAge",
        snapshot.recovery.age_ms,
        UNIT_MILLISECONDS,
    );
    push_optional_metric(
        &mut metrics,
        "BroadcasterRecoveryOldestBufferedAge",
        snapshot.recovery.oldest_buffered_age_ms,
        UNIT_MILLISECONDS,
    );
    emit_gauge_snapshot(
        "broadcaster_health",
        &[
            ("Readiness", json!(snapshot.readiness.as_str())),
            ("RecoveryPhase", json!(snapshot.recovery.phase)),
        ],
        &metrics,
    );
}

pub fn emit_simulator_health_snapshot(snapshot: &SimulatorStatusSnapshot) {
    let backend = |kind| {
        snapshot
            .backends
            .iter()
            .find(|backend| backend.kind == kind)
    };
    let native = backend(SimulatorBackendKind::Native);
    let vm = backend(SimulatorBackendKind::Vm);
    let rfq = backend(SimulatorBackendKind::Rfq);
    let is_available = |backend: Option<&crate::models::state::SimulatorBackendStatusSnapshot>| {
        backend.is_some_and(|status| status.readiness == SimulatorBackendReadiness::Ready)
    };
    let mut metrics = vec![
        (
            "SimulatorNativeReady",
            u64::from(is_available(native)),
            UNIT_COUNT,
        ),
        (
            "SimulatorNativeAvailable",
            u64::from(is_available(native)),
            UNIT_COUNT,
        ),
        (
            "SimulatorVmAvailable",
            u64::from(is_available(vm)),
            UNIT_COUNT,
        ),
        (
            "SimulatorRfqAvailable",
            u64::from(is_available(rfq)),
            UNIT_COUNT,
        ),
    ];
    push_optional_metric(
        &mut metrics,
        "SimulatorExposedStateAge",
        native.and_then(|status| status.last_update_age_ms),
        UNIT_MILLISECONDS,
    );
    push_optional_metric(
        &mut metrics,
        "SimulatorAppliedNativeHead",
        native.and_then(|status| status.block_number),
        UNIT_COUNT,
    );
    push_optional_metric(
        &mut metrics,
        "BroadcasterPublisherToConsumerDelay",
        native.and_then(|status| status.publisher_to_consumer_delay_ms),
        UNIT_MILLISECONDS,
    );
    emit_gauge_snapshot(
        "simulator_health",
        &[("Status", json!(snapshot.status.label()))],
        &metrics,
    );
}

fn push_optional_metric(
    metrics: &mut Vec<(&'static str, u64, &'static str)>,
    name: &'static str,
    value: Option<u64>,
    unit: &'static str,
) {
    if let Some(value) = value {
        metrics.push((name, value, unit));
    }
}

fn emit_gauge_snapshot(
    event_name: &'static str,
    properties: &[(&'static str, Value)],
    metrics: &[(&'static str, u64, &'static str)],
) {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let metric_definitions = metrics
        .iter()
        .map(|(name, _, unit)| json!({ "Name": name, "Unit": unit }))
        .collect::<Vec<_>>();
    let aws = json!({
        "Timestamp": timestamp_ms,
        "CloudWatchMetrics": [{
            "Namespace": METRIC_NAMESPACE,
            "Dimensions": [[]],
            "Metrics": metric_definitions,
        }],
    });
    let mut event = Map::new();
    event.insert("_aws".to_string(), aws);
    event.insert("event".to_string(), json!(event_name));
    for (name, value) in properties {
        event.insert((*name).to_string(), value.clone());
    }
    for (name, value, _) in metrics {
        event.insert((*name).to_string(), json!(value));
    }

    match serde_json::to_string(&Value::Object(event)) {
        Ok(line) => println!("{line}"),
        Err(error) => warn!(
            error = %error,
            metric = event_name,
            "Failed to serialize EMF metric"
        ),
    }
}

pub fn emit_jemalloc_snapshot(label: &str, allocated_bytes: usize, resident_bytes: usize) {
    // Emit CloudWatch Embedded Metric Format as a raw JSON log line.
    // Tracing's JSON wrapper would prevent EMF extraction, so we write directly to stdout.
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);

    let aws = json!({
        "Timestamp": timestamp_ms,
        "CloudWatchMetrics": [{
            "Namespace": METRIC_NAMESPACE,
            "Dimensions": [[DIM_LABEL]],
            "Metrics": [
                { "Name": METRIC_JEMALLOC_ALLOCATED_BYTES, "Unit": "Bytes" },
                { "Name": METRIC_JEMALLOC_RESIDENT_BYTES, "Unit": "Bytes" },
            ],
        }],
    });

    let mut event = Map::new();
    event.insert("_aws".to_string(), aws);
    event.insert(DIM_LABEL.to_string(), json!(label));
    event.insert(
        METRIC_JEMALLOC_ALLOCATED_BYTES.to_string(),
        json!(allocated_bytes as u64),
    );
    event.insert(
        METRIC_JEMALLOC_RESIDENT_BYTES.to_string(),
        json!(resident_bytes as u64),
    );

    match serde_json::to_string(&Value::Object(event)) {
        Ok(line) => println!("{line}"),
        Err(err) => warn!(
            error = %err,
            metric = "jemalloc_snapshot",
            "Failed to serialize EMF metric"
        ),
    }
}

fn emit_count_metric(metric_name: &str, dimensions: &[(&str, Value)]) {
    // Emit CloudWatch Embedded Metric Format as a raw JSON log line.
    // Tracing's JSON wrapper would prevent EMF extraction, so we write directly to stdout.
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);

    let dimension_names: Vec<&str> = dimensions.iter().map(|(name, _)| *name).collect();
    let aws = json!({
        "Timestamp": timestamp_ms,
        "CloudWatchMetrics": [{
            "Namespace": METRIC_NAMESPACE,
            "Dimensions": [dimension_names],
            "Metrics": [{
                "Name": metric_name,
                "Unit": "Count",
            }],
        }],
    });

    let mut event = Map::new();
    event.insert("_aws".to_string(), aws);
    event.insert(metric_name.to_string(), json!(1));
    for (name, value) in dimensions {
        event.insert((*name).to_string(), value.clone());
    }

    match serde_json::to_string(&Value::Object(event)) {
        Ok(line) => println!("{line}"),
        Err(err) => warn!(error = %err, metric = metric_name, "Failed to serialize EMF metric"),
    }
}
