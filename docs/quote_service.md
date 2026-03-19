# Quote service

This document describes the implemented quote-service workflow behind `/simulate` and how the rest of the repo depends on it.

## Runtime contract

`/simulate` returns `QuoteResult { request_id, data, meta }`.

The request shape is defined by `AmountOutRequest`:

- `request_id`
- optional `auction_id`
- `token_in`
- `token_out`
- `amounts`

The per-pool response shape is `AmountOutResponse`:

- `pool`
- `pool_name`
- `pool_address`
- `amounts_out`
- `gas_used`
- `block_number`

The request-level metadata shape is `QuoteMeta`:

- `status`
- `result_quality`
- optional `partial_kind`
- `block_number`
- optional `vm_block_number`
- `matching_pools`
- `candidate_pools`
- optional `total_pools`
- optional `auction_id`
- optional `pool_results`
- optional `vm_unavailable`
- optional `failures`

Live enum values:

- `QuoteStatus`: `ready`, `warming_up`, `token_missing`, `no_liquidity`, `invalid_request`, `internal_error`
- `QuoteResultQuality`: `complete`, `partial`, `no_results`, `request_level_failure`
- `QuotePartialKind`: `amount_ladders`, `pool_coverage`, `mixed`
- `PoolOutcomeKind`: `partial_output`, `zero_output`, `skipped_concurrency`, `skipped_deadline`, `timed_out`, `simulator_error`, `internal_error`

Contract invariants:

- `partial_success` is not part of the public contract
- `status=ready` can pair with `result_quality=complete`, `partial`, or selected `request_level_failure`
- `status=no_liquidity` pairs with `result_quality=no_results`
- `partial_kind` appears only when `result_quality=partial`
- `no_liquidity` never appears alongside usable quote data
- request-relevant fatal failures remain visible in `meta.failures` even when some usable amount outputs survive
- requested-amount order inside each pool row is part of the quote contract; `amounts_out[i]` matches the requested `amounts[i]`
- emitted pool rows preserve request order and request length for usable partial results; failed or timed-out requested amounts are serialized in place as `"0"` with matching `gas_used=0`
- `"0"` in `amounts_out` means that requested amount did not produce a usable quote for that pool; only positive outputs are usable quotes
- `data[]` contains only pools with at least one positive output across the requested amounts; fully-zero rows stay visible only through `meta.failures` and `meta.pool_results`
- `data[]` is stabilized for reproducibility, but row position is not a ranking signal; clients should not treat `data[0]` as "best pool"

## Request lifecycle

`POST /simulate` flows through these stages:

1. The handler logs the request and wraps quote computation in a request-level timeout guard.
2. The quote runner parses addresses and amounts, rejects invalid native-wrapped direct pairs, and loads request metadata.
3. Native readiness is checked before quoting. If native state is still warming up or stale, the request exits as `warming_up + request_level_failure`.
4. Token metadata is loaded for both sides. Missing or timed-out token coverage exits as `token_missing + request_level_failure`.
5. Candidate pools are loaded from native state and, when available, VM state.
6. Unsupported ERC4626 candidates are filtered before execution.
7. Pool tasks are scheduled under quote deadlines and global concurrency limits.
8. Per-pool execution results are aggregated into `data`, `meta.failures`, and `meta.pool_results`.
9. The runner classifies the final exit as `complete`, `partial`, `no_results`, or `request_level_failure`.

The current classification logic is:

- usable responses with no failures and no pool anomalies => `ready + complete`
- usable responses with failures or anomalies => `ready + partial`
- no usable responses plus liquidity-like failure classification => `no_liquidity + no_results`
- no usable responses plus non-liquidity degradation => `internal_error + request_level_failure`
- hard request gates keep their own top-level status and use `request_level_failure`

## Scenario matrix

| Scenario | `status` | `result_quality` | `partial_kind` | Notes |
| --- | --- | --- | --- | --- |
| Complete quote, all relevant requested amounts returned | `ready` | `complete` | omitted | `meta.failures` and `meta.pool_results` stay empty or omitted |
| Usable quotes returned, but at least one returned pool has partial requested-amount coverage | `ready` | `partial` | `amount_ladders` | emitted pool rows stay full-length and zero-fill failed amount positions; only positive outputs are usable quotes; `meta.pool_results` includes `partial_output` |
| Usable quotes returned, but some matching pools failed, timed out, or were skipped | `ready` | `partial` | `pool_coverage` | scheduling and simulator anomalies remain visible |
| Both partial requested-amount coverage and incomplete pool coverage occurred | `ready` | `partial` | `mixed` | both partiality sources are present |
| Matching pools exist, but none produce a usable quote because liquidity is absent or exhausted | `no_liquidity` | `no_results` | omitted | includes cases where candidate rows would otherwise be fully zero; `meta.failures` explains the no-liquidity reason |
| No matching pools exist | `no_liquidity` | `no_results` | omitted | `meta.failures` includes `no_pools` |
| Request times out or otherwise degrades with no usable quote surviving | `internal_error` or `ready` | `request_level_failure` | omitted | depends on where the timeout or degradation was surfaced |
| Warm-up, token coverage, or invalid request problem | gate-specific status | `request_level_failure` | omitted | request never reaches a normal usable-quote path |

## Readiness and gating

`GET /status` is the readiness contract used by scripts and deploy checks.

Native readiness:

- `status="ready"` with HTTP `200` means native state is ready and recent enough
- `status="warming_up"` with HTTP `503` means initial native state is still loading or native updates are stale

VM readiness:

- `vm_status="disabled"` when VM pools are turned off
- `vm_status="warming_up"` while VM state is still loading
- `vm_status="rebuilding"` during VM rebuilds
- `vm_status="ready"` when VM state is usable

Quote-path implications:

- native readiness gates the whole request
- VM readiness does not gate the whole request when native pools are still available
- when VM pools are enabled but not ready, the runner skips VM candidates and sets `meta.vm_unavailable=true`

## Candidate selection and execution

Candidate discovery:

- native candidates come from the native state store
- VM candidates come from the VM state store only when VM state is ready

Execution rules:

- native and VM tasks share separate global concurrency caps
- per-request quote execution uses a request deadline and per-pool deadlines
- pools that cannot be scheduled before the deadline or under concurrency limits are skipped instead of queued indefinitely
- usable outputs are preserved even when some pools degrade

Partiality sources:

- `amount_ladders`: a returned pool produced at least one usable quote, but one or more requested amounts failed or timed out
- `pool_coverage`: one or more matching pools were skipped, timed out, or failed before returning a usable quote
- `mixed`: both conditions happened in one response

Advisory `get_limits` signals do not define success on their own. `get_amount_out` remains the source of truth for quote success or failure.

Practical client rule:

- trust requested-amount alignment inside each returned row
- choose pools by the amount position you care about, not by row position
- treat `"0"` as "no usable quote for that requested amount," not as a usable quote
- a row can still be usable overall when some amount positions are `"0"`, but a fully-zero row is not usable and should not appear in `data[]`

## Failures and pool outcomes

`meta.failures` is the request-level explanation layer.

Important failure kinds include:

- `warm_up`
- `token_validation`
- `token_coverage`
- `timeout`
- `concurrency_limit`
- `overflow`
- `simulator`
- `no_pools`
- `inconsistent_result`
- `internal`
- `invalid_request`

`meta.pool_results` is the per-pool anomaly layer.

Use it to understand:

- which pools returned partial requested-amount coverage while still yielding at least one usable quote
- which pools returned zero output across all requested amounts and were therefore filtered out of `data[]`
- which pools were skipped because of concurrency or deadlines
- which pools timed out or failed inside the simulator

The two layers are intentionally redundant in some degraded cases. Material request-visible failures should not be visible only through per-pool anomaly rows.

## Timeouts

Handler-level timeout behavior:

- the `/simulate` handler wraps the quote runner in a request-level timeout guard
- when that guard fires, the response is still `200 OK`
- the payload is contract-valid and uses `status=ready`, `result_quality=request_level_failure`, and a timeout failure entry

Router-level timeout behavior:

- `/simulate` also sits behind a router timeout layer with extra headroom
- when the router boundary fires, the endpoint still returns `200 OK` with `result_quality=request_level_failure`
- logs mark those cases with `scope="router_timeout"`

`/encode` is intentionally different:

- router timeouts return `408 Request Timeout`
- the payload shape is `{ error, requestId }`

## Observability contract

The log and metric surfaces use the live quote contract fields, not deprecated status shortcuts.

Important log fields:

- `quote_status`
- `quote_result_quality`
- `partial_kind`
- `failures`
- `pool_results`
- request identifiers, token pair, latency, and best first-amount completion-log fields on completion logs

Observability note:

- API row order and completion-log `top_*` fields are different signals
- `data[]` is deterministic presentation output, not solver ranking
- completion-log `top_*` fields summarize the strongest quote seen for the first requested amount in the response set; they are useful for ops triage, but they are not a general "best pool overall" answer

Operational guidance:

- group dashboards by `quote_status` for hard request-state monitoring
- group by `quote_result_quality` for completeness and degradation monitoring
- use `partial_kind` to split partial requested-amount coverage from incomplete pool coverage
- treat `simulate-successes` style queries as `ready + (complete|partial)` only

## Integrations

`/encode` integration:

- `/encode` does not expose `QuoteMeta`
- `/encode` clients and smoke helpers depend on `/simulate` to find candidate pools
- pool selection should stay strict: `ready + complete|partial` is usable, `request_level_failure` and `no_results` are not
- clients should filter returned rows explicitly for the amount position they need instead of relying on `data[0]`
- repo encode smoke uses dedicated realistic amount presets per default route and requires every tested amount to stay usable across both simulated hops

Repo scripts:

- `scripts/run_suite.sh` is intentionally stricter than the public `/simulate` contract and keeps verification on successful paths with usable quotes
- `simulate_smoke.py`, `coverage_sweep.py`, and `latency_percentiles.py` all evaluate `result_quality`, not just `status`
- helper flags such as `--allow-failures` only relax request-visible failures on otherwise usable `complete` or `partial` results

CloudWatch and query presets:

- completion logs already emit `quote_status`, `quote_result_quality`, and `partial_kind`
- preset filters distinguish usable successes from degraded but contract-valid responses
- query docs and presets should stay aligned with this contract when log fields evolve
