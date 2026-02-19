---
name: tycho-cloudwatch-logs
description: Search, tail, and query Tycho simulation production logs in AWS CloudWatch using AWS CLI and bundled scripts. Use for /ecs/tycho-simulator investigations such as block updates, resyncs, readiness, timeouts, or general app errors.
---

# Tycho Cloudwatch Logs

## Overview
Use AWS CLI to inspect Tycho simulation logs in CloudWatch. Default log group is `/ecs/tycho-simulator` in `eu-central-1`. Logs are JSON from `tracing_subscriber`, so queries parse `@message` to extract `msg`, `level`, and structured fields.

If you hit `ExpiredTokenException`, refresh the `AWS_SESSION_TOKEN` in `.env` and retry.

Simulate completion logs are now emitted at `info` level for successful requests and include extra context (tokens, latency, top pool details, and failure summaries). For detailed failure summaries, inspect the raw `@message` JSON.
`uniswap-v4-filter` logs include `filter_rule`, `considered_pools`, `accepted_pools`, `filtered_pools`, `pools_with_hook_identifier`, and `pools_missing_hook_identifier` to explain hook-pool coverage.

## Quick start
0. Who am I / which env: `zsh skills/tycho-cloudwatch-logs/scripts/cw_whoami.zsh`
1. List recent streams: `zsh skills/tycho-cloudwatch-logs/scripts/cw_streams.zsh --limit 10`
2. Tail (JSON pretty print): `zsh skills/tycho-cloudwatch-logs/scripts/cw_tail.zsh --since 15m --follow`
3. Filter: `zsh skills/tycho-cloudwatch-logs/scripts/cw_filter.zsh --since 2h --filter-pattern "Block update:"`
4. Query: `zsh skills/tycho-cloudwatch-logs/scripts/cw_query.zsh --preset resync --since 24h`

## Preset catalog
| Preset | Purpose |
| --- | --- |
| block-updates | Track block height progress. |
| block-updates-window | Track block height progress (oldest to newest in the window). |
| block-updates-count | Count block updates in the window (with first/last timestamp). |
| readiness | Confirm readiness events. |
| resync | Inspect resync lifecycle. |
| stream-health | Stream startup and errors. |
| stream-supervision | Stream supervision and restart lifecycle. |
| vm-rebuild | VM rebuild lifecycle. |
| startup | App boot sequence. |
| server | HTTP server lifecycle. |
| timeouts | Handler timeouts. |
| router-timeouts | Router boundary timeouts. |
| simulate-requests | Incoming simulate requests. |
| simulate-completions | Completion logs for simulate calls. |
| simulate-successes | Successful completion logs (`status=Ready`). |
| simulate-rpm | Requests per minute (completion-based). |
| simulate-rpm-by-auction | Requests per minute per auction_id. |
| simulate-requests-per-auction | Total request count per auction_id. |
| simulate-runs | Per-request detail with simulation_runs count. |
| simulate-runs-per-minute | Total pool simulation runs per minute. |
| simulate-runs-per-auction | Total pool simulation runs per auction_id. |
| token-metadata | Token metadata fetch errors. |
| token-rpc-fetch | Single-token RPC fetch path. |
| state-anomalies | Missing/unknown state warnings. |
| vm-pools | VM pool feed config. |
| tvl-thresholds | TVL filter thresholds. |
| uniswap-v4-filter | Uniswap v4 hook filter log. |
| warn-error | WARN and ERROR level logs. |
| storage-errors | StorageError incidents with pool context. |
| delta-transition | DeltaTransitionError and failed update warnings. |
| stream-update-stats | Max new/removed/updated/total pairs by stream. |

## Scripts
- `scripts/cw_streams.zsh`: list recent log streams by last event time.
- `scripts/cw_tail.zsh`: live tail with optional filter and stream prefix (default format: json).
- `scripts/cw_filter.zsh`: time-window search with filter patterns.
- `scripts/cw_query.zsh`: Logs Insights queries with presets and JSON parsing.
- `scripts/cw_metrics.zsh`: memory/cpu metrics summaries and time series.
- `scripts/analyze_snapshot.py`: render a production snapshot markdown report from query + metrics JSON outputs.
- `scripts/cw_time.py`: time parsing helper for `cw_filter`, `cw_query`, and `cw_metrics`.

## Snapshot report workflow
- Prompt source: `~/.codex/prompts/sim-report.md`.
- Default period in the prompt is `12h` when no argument is provided.
- Required generated files for the analyzer:
  `simulate-runs-per-minute.json`, `simulate-runs-per-auction.json`,
  `simulate-runs.json`, `simulate-requests-per-auction.json`,
  `simulate-completions.json`, `cw-metrics.json`.

## Metrics
Quick checks for memory and CPU utilization using ECS ContainerInsights.

- Memory + CPU summary + series:
  `zsh skills/tycho-cloudwatch-logs/scripts/cw_metrics.zsh --since 12h --metrics memory,cpu`
- Pivot spike check:
  `zsh skills/tycho-cloudwatch-logs/scripts/cw_metrics.zsh --since 2h --pivot 2026-02-04T11:38:00Z --window 10m`

## Override defaults
- Set `TYCHO_LOG_GROUP` for another log group.
- Set `AWS_REGION` or `AWS_DEFAULT_REGION` for another region.
- Set `TYCHO_ECS_CLUSTER` or `TYCHO_ECS_SERVICE` to override ECS defaults for metrics.
- Pass `--log-group` on any script to override.

## References
Use `references/queries.md` for the full preset index and filter/query snippets.
