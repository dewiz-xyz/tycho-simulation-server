use std::{collections::BTreeMap, env, process::ExitCode};

use anyhow::{ensure, Context};
use state_history::{
    encode_archive, ArchiveMetadata, Backend, BacktestRequest, BlockTimeObservation, CapturedState,
    CheckpointArchive, CheckpointKind, CheckpointManifest, CheckpointObjectStore,
    DeltaBackendCursor, DeltaEntry, GapReason, GapRecord, RangeGap, RangeGapKind, RangePlan,
    RangeRequest, StateHistoryReader, StateHistoryStore, StreamPosition,
};

const CHAIN_ID: u64 = 8_453;
const DEFAULT_DATABASE_URL: &str = "postgres://postgres:postgres@127.0.0.1:55432/state_history";
const DEFAULT_S3_BUCKET: &str = "state-history-analysis";
const DEFAULT_S3_PREFIX: &str = "smoke";
const DEFAULT_S3_REGION: &str = "eu-central-1";
const DEFAULT_S3_ENDPOINT_URL: &str = "http://127.0.0.1:59000";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => {
            println!("PASS state-history-analysis complete");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("FAIL state-history-analysis: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let database_url = env_or_default("DATABASE_URL", DEFAULT_DATABASE_URL);
    let bucket = env_or_default("STATE_HISTORY_S3_BUCKET", DEFAULT_S3_BUCKET);
    let prefix = env_or_default("STATE_HISTORY_S3_PREFIX", DEFAULT_S3_PREFIX);
    let region = env_or_default("STATE_HISTORY_S3_REGION", DEFAULT_S3_REGION);
    let endpoint = env_or_default("STATE_HISTORY_S3_ENDPOINT_URL", DEFAULT_S3_ENDPOINT_URL);

    let store = StateHistoryStore::connect(&database_url).await?;
    store.run_migrations().await?;
    store.validate_schema().await?;
    println!("PASS migrations and schema validation");

    let seed_objects = object_store(&bucket, &prefix, &region, &endpoint).await;
    let first_checkpoint = persist_checkpoint(
        &store,
        &seed_objects,
        capture(1, 10, 100, CheckpointKind::Interval, "generation-1")?,
    )
    .await?;
    seed_deltas(&store, 1, 11, 40, 100, 110).await?;

    let second_checkpoint = persist_checkpoint(
        &store,
        &seed_objects,
        capture(2, 1, 110, CheckpointKind::Boundary, "generation-2")?,
    )
    .await?;
    seed_deltas(&store, 2, 2, 20, 110, 118).await?;
    record_gap(&store, 2, 8, 10).await?;
    println!("PASS seeded two generations and queue_overflow gap 2:8..=10");

    let pool = store.pool().clone();
    let reader = StateHistoryReader::new(
        store,
        object_store(&bucket, &prefix, &region, &endpoint).await,
    );

    let short_plan = reader.resolve_range(&range_request(100, 105)?).await?;
    assert_plan(&short_plan, &[(position(1, 10), positions(1, 11, 28))], &[])?;
    short_plan.ensure_gap_free()?;
    println!("PASS resolve_range 100..=105 returned one gap-free leg");

    let explicit_request = range_request(100, 115)?;
    let explicit_plan = reader.resolve_range(&explicit_request).await?;
    let recorded_gap = RangeGap {
        kind: RangeGapKind::Recorded,
        generation: Some(2),
        from_message_seq: Some(8),
        to_message_seq: Some(10),
        reason: "queue_overflow".to_owned(),
    };
    let replay_legs = [
        (position(1, 10), positions(1, 11, 40)),
        (position(2, 1), positions(2, 2, 15)),
    ];
    assert_plan(
        &explicit_plan,
        &replay_legs,
        std::slice::from_ref(&recorded_gap),
    )?;
    println!("PASS resolve_range 100..=115 returned two ordered legs and the recorded gap");

    let backtest = reader
        .resolve_backtest_range(&BacktestRequest::new(CHAIN_ID, 100, 115, all_backends())?)
        .await?;
    ensure!(
        backtest.start_block_timestamp_ms == block_timestamp_ms(100)
            && backtest.end_block_timestamp_ms == block_timestamp_ms(115)
            && backtest.rfq_bounds == Some(rfq_bounds(100, 115)),
        "backtest timestamp bounds did not match the seeded block_times rows"
    );
    assert_plan(
        &backtest.range,
        &replay_legs,
        std::slice::from_ref(&recorded_gap),
    )?;
    println!("PASS resolve_backtest_range matched the explicit RFQ-bounds plan");

    assert_checkpoint_round_trip(
        &reader,
        &explicit_plan.legs[0].checkpoint,
        &first_checkpoint,
    )
    .await?;
    assert_checkpoint_round_trip(
        &reader,
        &explicit_plan.legs[1].checkpoint,
        &second_checkpoint,
    )
    .await?;
    println!("PASS fetch_checkpoint round-tripped both hash-verified archives");

    StateHistoryStore::fail_checkpoint(&pool, second_checkpoint.id, "injected boundary failure")
        .await?;
    let failed_plan = reader.resolve_range(&explicit_request).await?;
    assert_failed_boundary(&failed_plan, recorded_gap)?;
    println!("PASS failed gen-2 boundary replaced replay leg 2 with MissingCheckpoint");

    Ok(())
}

fn assert_failed_boundary(failed_plan: &RangePlan, recorded_gap: RangeGap) -> anyhow::Result<()> {
    let missing_checkpoint = RangeGap {
        kind: RangeGapKind::MissingCheckpoint,
        generation: Some(2),
        from_message_seq: Some(1),
        to_message_seq: Some(15),
        reason: "no complete boundary checkpoint at generation 2 message sequence 1".to_owned(),
    };
    assert_plan(
        failed_plan,
        &[(position(1, 10), positions(1, 11, 40))],
        &[missing_checkpoint, recorded_gap],
    )
}

struct SeededCheckpoint {
    id: i64,
    archive: CheckpointArchive,
    sha256: String,
}

async fn persist_checkpoint(
    store: &StateHistoryStore,
    objects: &CheckpointObjectStore,
    capture: CapturedState,
) -> anyhow::Result<SeededCheckpoint> {
    let mut connection = store.pool().acquire().await?;
    let id = StateHistoryStore::create_checkpoint(&mut connection, &capture).await?;
    let archive = CheckpointArchive {
        metadata: ArchiveMetadata {
            schema_version: 1,
            chain_id: capture.chain_id(),
            position: capture.position(),
            state_version: capture.state_version(),
            kind: capture.kind(),
            block_number: capture.block_number(),
            rfq_observed_at_ms: capture.rfq_observed_at_ms(),
            backends: capture.backends().to_vec(),
        },
        payloads_json: capture.payloads_json().to_vec(),
    };
    let encoded = encode_archive(archive.clone())?;
    let key = objects.key_for(capture.chain_id(), capture.position(), capture.kind());
    objects.put(&key, encoded.bytes).await?;
    StateHistoryStore::complete_checkpoint(
        &mut connection,
        id,
        &key,
        &encoded.info,
        None,
        capture.block_times(),
        capture.position(),
        capture.chain_id(),
    )
    .await?;

    Ok(SeededCheckpoint {
        id,
        archive,
        sha256: encoded.info.sha256,
    })
}

async fn seed_deltas(
    store: &StateHistoryStore,
    generation: u64,
    first_seq: u64,
    last_seq: u64,
    first_block: u64,
    last_block: u64,
) -> anyhow::Result<()> {
    let mut connection = store.pool().acquire().await?;
    for message_seq in first_seq..=last_seq {
        let block_number = first_block
            + (message_seq - first_seq) * (last_block - first_block) / (last_seq - first_seq);
        let rfq_observed_at_ms = block_timestamp_ms(block_number + 1) - 1;
        let delta = DeltaEntry::new(
            CHAIN_ID,
            position(generation, message_seq),
            rfq_observed_at_ms,
            format!(
                r#"{{"generation":{generation},"message_seq":{message_seq},"block":{block_number}}}"#
            ),
            vec![
                DeltaBackendCursor {
                    backend: Backend::Native,
                    block_number: Some(block_number),
                    observed_at_ms: None,
                },
                DeltaBackendCursor {
                    backend: Backend::Vm,
                    block_number: Some(block_number),
                    observed_at_ms: None,
                },
                DeltaBackendCursor {
                    backend: Backend::Rfq,
                    block_number: None,
                    observed_at_ms: Some(rfq_observed_at_ms),
                },
            ],
            vec![block_time(block_number)],
        )?;
        StateHistoryStore::insert_delta(&mut connection, &delta, &BTreeMap::new())
            .await
            .with_context(|| {
                format!("failed to seed delta at generation {generation} sequence {message_seq}")
            })?;
    }
    Ok(())
}

async fn record_gap(
    store: &StateHistoryStore,
    generation: u64,
    from_message_seq: u64,
    to_message_seq: u64,
) -> anyhow::Result<()> {
    let mut connection = store.pool().acquire().await?;
    StateHistoryStore::record_gap(
        &mut connection,
        &GapRecord {
            chain_id: CHAIN_ID,
            generation,
            from_message_seq,
            to_message_seq,
            reason: GapReason::QueueOverflow,
            from_block_number: Some(112),
            to_block_number: Some(113),
            from_observed_at_ms: Some(block_timestamp_ms(113) - 1),
            to_observed_at_ms: Some(block_timestamp_ms(114) - 1),
        },
    )
    .await
}

fn assert_plan(
    plan: &RangePlan,
    expected_legs: &[(StreamPosition, Vec<StreamPosition>)],
    expected_gaps: &[RangeGap],
) -> anyhow::Result<()> {
    ensure!(
        plan.legs.len() == expected_legs.len(),
        "expected {} legs, got {}",
        expected_legs.len(),
        plan.legs.len()
    );
    for (leg, (checkpoint, deltas)) in plan.legs.iter().zip(expected_legs) {
        ensure!(
            leg.checkpoint.position == *checkpoint,
            "expected checkpoint {checkpoint:?}, got {:?}",
            leg.checkpoint.position
        );
        let actual = leg
            .deltas
            .iter()
            .map(|delta| delta.position)
            .collect::<Vec<_>>();
        ensure!(
            actual == *deltas,
            "unexpected replay positions after checkpoint {checkpoint:?}: {actual:?}"
        );
    }
    ensure!(
        plan.gaps == expected_gaps,
        "expected gaps {expected_gaps:?}, got {:?}",
        plan.gaps
    );
    Ok(())
}

async fn assert_checkpoint_round_trip(
    reader: &StateHistoryReader,
    manifest: &CheckpointManifest,
    seeded: &SeededCheckpoint,
) -> anyhow::Result<()> {
    ensure!(
        manifest.id == seeded.id
            && manifest.archive_sha256.as_deref() == Some(seeded.sha256.as_str()),
        "checkpoint manifest hash differs from encoded archive hash"
    );
    let fetched = reader.fetch_checkpoint(manifest).await?;
    ensure!(
        fetched == seeded.archive,
        "fetched checkpoint archive differs from its seeded value"
    );
    Ok(())
}

fn capture(
    generation: u64,
    message_seq: u64,
    block_number: u64,
    kind: CheckpointKind,
    label: &str,
) -> anyhow::Result<CapturedState> {
    CapturedState::new(
        CHAIN_ID,
        position(generation, message_seq),
        Some(generation),
        kind,
        block_number,
        Some(block_timestamp_ms(block_number)),
        all_backends(),
        vec![format!(r#"{{"state":"{label}"}}"#)],
        vec![],
    )
    .map_err(Into::into)
}

fn range_request(start_block: u64, end_block: u64) -> anyhow::Result<RangeRequest> {
    let (start_ms, end_ms) = rfq_bounds(start_block, end_block);
    Ok(
        RangeRequest::new(CHAIN_ID, start_block, end_block, all_backends())?
            .with_rfq_bounds(start_ms, end_ms)?,
    )
}

fn all_backends() -> Vec<Backend> {
    vec![Backend::Native, Backend::Vm, Backend::Rfq]
}

fn positions(generation: u64, first_seq: u64, last_seq: u64) -> Vec<StreamPosition> {
    (first_seq..=last_seq)
        .map(|message_seq| position(generation, message_seq))
        .collect()
}

const fn position(generation: u64, message_seq: u64) -> StreamPosition {
    StreamPosition {
        generation,
        message_seq,
    }
}

fn block_time(block_number: u64) -> BlockTimeObservation {
    BlockTimeObservation {
        block_number,
        timestamp_ms: block_timestamp_ms(block_number),
        block_hash: Some(format!("block-{block_number}")),
        parent_hash: Some(format!("block-{}", block_number - 1)),
    }
}

const fn rfq_bounds(start_block: u64, end_block: u64) -> (u64, u64) {
    (
        block_timestamp_ms(start_block),
        block_timestamp_ms(end_block + 1) - 1,
    )
}

const fn block_timestamp_ms(block_number: u64) -> u64 {
    block_number * 1_000
}

fn env_or_default(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}

async fn object_store(
    bucket: &str,
    prefix: &str,
    region: &str,
    endpoint: &str,
) -> CheckpointObjectStore {
    CheckpointObjectStore::from_env_config(
        bucket.to_owned(),
        prefix.to_owned(),
        region.to_owned(),
        Some(endpoint.to_owned()),
        true,
    )
    .await
}
