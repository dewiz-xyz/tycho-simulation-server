# State History

State history records the broadcaster as ordered segments. A stream position is the pair `(generation, message_seq)`. PostgreSQL stores accepted update payloads as deltas and stores checkpoint metadata, while S3 stores the compressed full-state checkpoint archives.

A reader starts from the newest complete checkpoint at or before the requested start block, then selects deltas using each backend's own block or timestamp cursor. A replay plan can contain multiple legs. Each leg starts from one checkpoint and applies its deltas in `(generation, message_seq)` order.

Boundary checkpoints start new segments after writer promotion and recovery commits. Interval checkpoints reduce replay work but do not split an existing segment. A missing or failed boundary stops replay across that boundary and appears as a gap. Updates drained from the recovery buffer at a recovery commit are appended directly to Redis and do not become delta rows. The boundary checkpoint's exported cache state covers them.

## PostgreSQL schema

The migration in `crates/state-history/migrations/20260728000100_create_state_history.sql` is the source of truth for this schema.

### `state_history.schema_meta`

| Column | Meaning |
| --- | --- |
| `singleton` | Primary key fixed to `TRUE`, which keeps this table to one schema metadata row. |
| `version` | State history schema version. The current migration inserts version `1`. |

### `state_history.deltas`

One row represents one accepted update. `(chain_id, generation, message_seq)` is both the idempotency key and replay order.

| Column | Meaning |
| --- | --- |
| `id` | Generated row identifier. |
| `chain_id` | Chain that produced the update. |
| `generation` | Redis writer generation for the update. |
| `message_seq` | Update sequence within the generation. |
| `block_number` | Highest block cursor carried by the update, when any backend has a block cursor. |
| `observed_at_ms` | Time the update was observed, in Unix milliseconds. |
| `payload` | Zstandard-compressed serialized update payload. |
| `payload_sha256` | SHA-256 digest of the uncompressed serialized payload. |
| `created_at` | Time PostgreSQL inserted the row. |

### `state_history.delta_backends`

One row maps a delta to one affected backend. Range queries use these backend cursors instead of the envelope cursor.

| Column | Meaning |
| --- | --- |
| `delta_id` | Referenced `deltas.id`. Deleting the delta deletes this row. |
| `chain_id` | Chain copied from the parent delta for indexed range queries. |
| `backend` | Affected backend. Allowed values are `native`, `vm`, and `rfq`. |
| `generation` | Writer generation copied from the parent delta. |
| `message_seq` | Message sequence copied from the parent delta. |
| `block_number` | Backend block cursor, when the backend advances by block. |
| `observed_at_ms` | Backend observation cursor in Unix milliseconds, when the backend advances by time. |

At least one of `block_number` and `observed_at_ms` is present. A delta has at most one row for each backend.

### `state_history.checkpoints`

One row is the PostgreSQL manifest for a full-state checkpoint archive in S3.

| Column | Meaning |
| --- | --- |
| `id` | Generated checkpoint identifier. |
| `chain_id` | Chain captured by the checkpoint. |
| `generation` | Writer generation at capture. |
| `message_seq` | Inclusive stream position covered by the checkpoint. |
| `state_version` | Redis committed state version for the capture, when available. |
| `kind` | Checkpoint purpose. Allowed values are `boundary` and `interval`. |
| `block_number` | Aligned native and VM head captured by the checkpoint. |
| `rfq_observed_at_ms` | Highest RFQ observation time covered by the checkpoint, in Unix milliseconds. |
| `backends` | Backend names included in the checkpoint archive. |
| `s3_key` | Object key for the completed archive. |
| `archive_sha256` | SHA-256 digest of the uncompressed checkpoint archive JSON. |
| `archive_bytes` | Uncompressed checkpoint archive size in bytes. |
| `compressed_bytes` | Stored Zstandard archive size in bytes. |
| `status` | Lifecycle state. Allowed values are `writing`, `complete`, and `failed`. |
| `error` | Checkpoint failure detail, when the checkpoint failed. |
| `created_at` | Time PostgreSQL created the manifest. |
| `completed_at` | Time the checkpoint reached `complete` or `failed` status. |

`(chain_id, generation, message_seq, kind)` is unique.

After `STATE_HISTORY_S3_PREFIX`, S3 keys use `chain={chain_id}/gen={generation}/seq={message_seq}/kind={boundary|interval}/checkpoint.zst`.

### `state_history.gaps`

Rows record losses known to the writer.

| Column | Meaning |
| --- | --- |
| `id` | Generated gap identifier. |
| `chain_id` | Chain affected by the gap. |
| `generation` | Writer generation containing the gap. |
| `from_message_seq` | First missing or unusable message sequence, inclusive. |
| `to_message_seq` | Last missing or unusable message sequence, inclusive. |
| `reason` | Known cause. Allowed values are `queue_overflow`, `write_failed`, `writer_stopped`, `checkpoint_failed`, and `boundary_slip`. |
| `from_block_number` | First affected block when a block bound is known. |
| `to_block_number` | Last affected block when a block bound is known. |
| `from_observed_at_ms` | First affected observation time in Unix milliseconds, when known. |
| `to_observed_at_ms` | Last affected observation time in Unix milliseconds, when known. |
| `created_at` | Time PostgreSQL inserted the row. |

`from_message_seq` cannot exceed `to_message_seq`. The full gap identity is unique, including nullable block and time bounds.

Block bounds come only from block backend cursors. Observation time bounds come only from RFQ provider cursors, never from Redis publication time. A bound stays `NULL` when the lost payload has no corresponding cursor. Boundary checkpoint failures use the capture block number and RFQ high water for both sides of their bounds. Rows with all four bounds `NULL` are still selected by stream position.

`boundary_slip` records a same generation recovery boundary checkpoint that pins after its requested commit position. Its stream span starts at the requested message sequence plus one and ends at the pinned capture position, so replay across any slipped position fails closed.

### `state_history.block_times`

This table joins block number, block time, hashes, and base fee for direct analysis and RFQ range resolution.

| Column | Meaning |
| --- | --- |
| `chain_id` | Chain containing the block. |
| `block_number` | Block number. Together with `chain_id`, this is the primary key. |
| `timestamp_ms` | Block timestamp in Unix milliseconds. |
| `block_hash` | Block hash, when supplied by the update. |
| `parent_hash` | Parent block hash, when supplied by the update. |
| `base_fee_wei` | Base fee in wei, when an RPC source resolved it. |
| `source_generation` | Writer generation of the latest accepted observation. |
| `source_message_seq` | Message sequence of the latest accepted observation. |
| `created_at` | Time PostgreSQL created the row. |
| `updated_at` | Time a newer observation changed the block metadata. |

A row is superseded only by a newer `(source_generation, source_message_seq)` observation.

When a newer observation keeps the same block hash and supplies no base fee, the stored fee is preserved. When it changes the hash and supplies no base fee, the stored fee is cleared rather than inherited from the orphaned block.

## Example queries

The examples use Base chain ID `8453`.

### Blocks per hour

```sql
SELECT
    date_trunc('hour', to_timestamp(timestamp_ms / 1000.0)) AS hour,
    count(*) AS blocks
FROM state_history.block_times
WHERE chain_id = 8453
GROUP BY hour
ORDER BY hour;
```

### Gap inspection

```sql
SELECT
    generation,
    from_message_seq,
    to_message_seq,
    reason,
    from_block_number,
    to_block_number,
    from_observed_at_ms,
    to_observed_at_ms,
    created_at
FROM state_history.gaps
WHERE chain_id = 8453
ORDER BY generation, from_message_seq, to_message_seq;
```

### Checkpoint inventory

```sql
SELECT
    kind,
    status,
    count(*) AS checkpoints,
    min(block_number) AS first_block,
    max(block_number) AS last_block,
    sum(compressed_bytes) AS compressed_bytes
FROM state_history.checkpoints
WHERE chain_id = 8453
GROUP BY kind, status
ORDER BY kind, status;
```

### Base-fee join

```sql
SELECT
    d.generation,
    d.message_seq,
    db.backend,
    db.block_number,
    bt.timestamp_ms,
    bt.base_fee_wei
FROM state_history.delta_backends AS db
JOIN state_history.deltas AS d
    ON d.id = db.delta_id
JOIN state_history.block_times AS bt
    ON bt.chain_id = db.chain_id
   AND bt.block_number = db.block_number
WHERE db.chain_id = 8453
  AND bt.base_fee_wei IS NOT NULL
ORDER BY d.generation, d.message_seq, db.backend;
```

### Delta volume by backend

```sql
SELECT
    db.backend,
    count(*) AS backend_entries,
    count(DISTINCT db.delta_id) AS deltas,
    sum(octet_length(d.payload)) AS compressed_payload_bytes
FROM state_history.delta_backends AS db
JOIN state_history.deltas AS d
    ON d.id = db.delta_id
WHERE db.chain_id = 8453
GROUP BY db.backend
ORDER BY db.backend;
```

## Reader API

`StateHistoryReader::resolve_backtest_range(&BacktestRequest)` returns a `BacktestPlan`. Native and VM requests get informational start and end timestamps when rows exist. Requests that include RFQ require continuous `block_times` rows through `end_block + 1`, which the reader uses to derive inclusive RFQ timestamp bounds.

Each `RangeLeg` contains a `CheckpointManifest` and zero or more ordered deltas to apply after that checkpoint. Boundary discovery uses the request end rather than the last stored delta. For every requested backend represented by a boundary, its checkpoint cursor must be at or below that backend's requested end. A trailing boundary can therefore close a range without a later delta, producing a leg with zero deltas.

`StateHistoryReader::fetch_checkpoint(&CheckpointManifest)` accepts only a complete manifest, downloads its S3 object, verifies the archive digest, and returns the decoded `CheckpointArchive`.

```rust
use state_history::{Backend, BacktestRequest, StateHistoryReader};

async fn load_range(reader: &StateHistoryReader) -> anyhow::Result<()> {
    let request = BacktestRequest::new(
        8453,
        25_000_000,
        25_001_000,
        vec![Backend::Native, Backend::Vm, Backend::Rfq],
    )?;
    let plan = reader.resolve_backtest_range(&request).await?;
    plan.range.ensure_gap_free()?;

    for leg in &plan.range.legs {
        let checkpoint = reader.fetch_checkpoint(&leg.checkpoint).await?;
        // Restore checkpoint.payloads_json, then apply leg.deltas in order.
        process_leg(checkpoint, &leg.deltas).await?;
    }

    Ok(())
}
```

Construct the reader with `StateHistoryReader::new(StateHistoryStore, CheckpointObjectStore)`. See `crates/state-history/src/reader.rs` for the complete API and replay plan types.

## Operations

State history is inert when `STATE_HISTORY_ENABLED=false`. When it is disabled, the writer is not created and the `state_history` property is absent from `/status` JSON. Enabling it requires PostgreSQL, S3, and the checkpoint block interval. Deployment configuration remains in solver-iac.

### Environment variables

| Variable | Meaning and default |
| --- | --- |
| `STATE_HISTORY_ENABLED` | Enables the writer. Defaults to `false`. |
| `STATE_HISTORY_DATABASE_URL` | PostgreSQL connection URL. Required when enabled. |
| `STATE_HISTORY_S3_BUCKET` | S3 bucket for checkpoint archives. Required when enabled. |
| `STATE_HISTORY_S3_PREFIX` | Object key prefix. Defaults to `state-history`. One trailing slash is removed. |
| `STATE_HISTORY_S3_REGION` | S3 region. Defaults to `eu-central-1`. |
| `STATE_HISTORY_S3_ENDPOINT_URL` | Optional custom S3 endpoint for compatible local services. |
| `STATE_HISTORY_S3_FORCE_PATH_STYLE` | Forces path-style S3 requests. Defaults to `false`. |
| `STATE_HISTORY_CHECKPOINT_BLOCK_INTERVAL` | Required positive interval between checkpoint targets. Use `300` in production. |
| `STATE_HISTORY_CHECKPOINT_POLL_INTERVAL_SECS` | Seconds between checkpoint eligibility checks. Defaults to `30`. |
| `STATE_HISTORY_QUEUE_CAPACITY` | In-memory writer command queue capacity. Defaults to `8192`. |
| `STATE_HISTORY_WRITE_RETRY_WINDOW_MS` | Maximum delta write retry window in milliseconds. Defaults to `30000`. |

`.env.example` contains the complete local example block. The recommended production value is `STATE_HISTORY_CHECKPOINT_BLOCK_INTERVAL=300`.

### `/status.state_history`

The object mirrors the writer's 13 status fields. Its fields use camelCase. Optional fields are omitted when they have no value.

| Field | Meaning |
| --- | --- |
| `healthy` | Whether the writer's latest persistence operation is healthy. It becomes false after a failed delta, checkpoint, shutdown, or permanent gap write. |
| `queueLen` | Current number of queued writer commands. |
| `queueCapacity` | Configured writer command queue capacity. |
| `enqueuedDeltas` | Accepted deltas added to the writer queue. |
| `persistedDeltas` | Deltas successfully persisted. |
| `droppedDeltas` | Deltas rejected because the queue was unavailable or full. |
| `failedDeltas` | Deltas that exhausted persistence attempts or failed permanently. |
| `recordedGaps` | Gap rows successfully persisted. |
| `lastPersisted` | Last persisted stream position as `{ "generation", "messageSeq" }`. Omitted before the first persisted delta. |
| `lastError` | Latest writer error text. Omitted when there is no current error. |
| `checkpointsCompleted` | Checkpoints whose archive and manifest completed successfully. |
| `checkpointsFailed` | Checkpoints whose archive or manifest lifecycle failed. |
| `lastCheckpointStatus` | Latest checkpoint result, currently `complete` or `failed`. Omitted before the first result. |

Gaps are honest reporting. They record losses the writer knows about, but their absence is not proof that a range is complete. Consumers should call `RangePlan::ensure_gap_free()` before using a replay plan.

Retention is keep everything. There is no state history TTL or cleanup policy for PostgreSQL rows or S3 checkpoint archives.
