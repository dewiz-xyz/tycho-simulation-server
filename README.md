# Tycho Simulation Server

An HTTP service that ingests Tycho protocol streams, keeps an in-memory view of pool state, and serves swap quote simulation, route encoding, and readiness APIs.

## Service Role

This service owns three things:

- continuous ingestion of native and optional VM-backed Tycho pool state
- on-demand quote simulation through `POST /simulate`
- route calldata generation through `POST /encode`

It runs on Axum and Tokio, uses `tycho-simulation` for pricing and execution, and keeps request handling aligned with the live runtime contract implemented in `src/models/messages.rs`, `src/services/quotes.rs`, and `src/services/encode/`.

### Local run

```bash
cp .env.example .env
cargo run --release
```

Required runtime inputs:

- `TYCHO_API_KEY` for Tycho access
- `CHAIN_ID` for chain selection (`1` Ethereum, `8453` Base)

Common optional inputs:

- `RPC_URL` for cached `eth_gasPrice` refreshes used by `gas_in_sell`
- `ENABLE_VM_POOLS` to enable or disable VM-backed pool feeds
- `HOST` and `PORT` to change the bind address
- timeout, concurrency, and stream-health knobs from `src/config/mod.rs`

`src/config/mod.rs` is the authoritative source for runtime defaults. `.env.example` is an example setup, not the source of truth for every default.

## API Surface

- `POST /simulate` returns laddered pool quotes plus `meta` describing quote completeness, failures, and readiness-adjacent request outcomes.
- `POST /encode` accepts a client-provided route, re-simulates the swaps internally, and returns ordered settlement `interactions[]`.
- `GET /status` reports native readiness, VM readiness, block progress, and VM rebuild/restart context for pollers and deploy scripts.

Detailed integration docs:

- [docs/simulate_example.md](docs/simulate_example.md) for `/simulate` request and response shape
- [docs/encode_example.md](docs/encode_example.md) for `/encode` request and response shape

## Quote Outcome Semantics

`/simulate` uses a structured response contract. Clients should not treat HTTP `200 OK` as quote success on its own.

`QuoteStatus` is the top-level request state:

- `ready`
- `warming_up`
- `token_missing`
- `no_liquidity`
- `invalid_request`
- `internal_error`

`QuoteResultQuality` describes result completeness:

- `complete`
- `partial`
- `no_results`
- `request_level_failure`

`meta.partial_kind` appears only when `result_quality=partial`:

- `amount_ladders`
- `pool_coverage`
- `mixed`

Summary matrix:

| Situation | `meta.status` | `meta.result_quality` | Quoteable |
| --- | --- | --- | --- |
| Usable quotes with full coverage | `ready` | `complete` | yes |
| Usable quotes with incomplete ladders or pool coverage | `ready` | `partial` | yes |
| No usable quote because liquidity is absent or exhausted | `no_liquidity` | `no_results` | no |
| Request completed with a structurally valid degraded response | `ready` or `internal_error` | `request_level_failure` | no |
| Warm-up, token coverage, or request validation problem | `warming_up`, `token_missing`, or `invalid_request` | `request_level_failure` | no |

Quoteable `/simulate` results require:

- `meta.status=ready`
- `meta.result_quality=complete` or `meta.result_quality=partial`

`meta.failures` is the request-level explanation layer. It captures timeouts, cancellations, token coverage problems, no-pools reasons, and request-relevant simulation failures.

`meta.pool_results` is the per-pool anomaly layer. It captures pool-local outcomes such as `partial_output`, `zero_output`, `skipped_concurrency`, `skipped_deadline`, `timed_out`, `simulator_error`, and `internal_error`.

`meta.vm_unavailable=true` means VM pools were skipped because VM state was not ready. When usable native quotes still exist, that typically surfaces as a `ready + partial` response rather than a hard non-ready status.

## Readiness And Timeouts

`GET /status` is the readiness source of truth:

- `200 OK` with `status="ready"` when native state is ready and not stale
- `503 Service Unavailable` with `status="warming_up"` while initial state is still loading or native updates are stale

`vm_status` is one of:

- `disabled`
- `warming_up`
- `rebuilding`
- `ready`

Timeout behavior differs by endpoint:

- `/simulate` request-guard timeouts return `200 OK` with a contract-valid payload whose `meta.status=ready`, `meta.result_quality=request_level_failure`, and `meta.failures` includes a `timeout`
- `/simulate` router-boundary timeouts also return `200 OK` with `result_quality=request_level_failure`
- `/encode` timeouts return `408 Request Timeout` with `{ "error": "...", "requestId": ... }`
- `/status` is not wrapped in the router timeout layer

If you are integrating against `/simulate`, inspect `meta` on every successful HTTP response. If you are integrating against `/encode`, normal HTTP success and error handling is still the right control flow.

## Verification

CI-equivalent commands:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run
cargo build --release
```

Repo harness:

```bash
scripts/run_suite.sh --repo . --chain-id 1 --stop
scripts/run_suite.sh --repo . --chain-id 8453 --stop
```

Useful helpers:

- `scripts/start_server.sh` to start the server with repo-local PID and log files
- `scripts/wait_ready.sh` to poll `/status` and enforce chain and VM readiness expectations
- `scripts/stop_server.sh` to stop a server started by the repo helper
- `scripts/run_suite.sh` to run smoke, encode smoke, coverage, and latency checks

The repo suite is intentionally strict: it keeps verification on quoteable healthy-path responses and fails on request-visible degradation even when `/simulate` still returns a contract-valid `200 OK` payload.

## Docs Map

- [docs/simulate_example.md](docs/simulate_example.md): `/simulate` API examples and integration notes
- [docs/encode_example.md](docs/encode_example.md): `/encode` API examples and route-shape notes
- [docs/quote_service.md](docs/quote_service.md): maintainer deep dive for quote lifecycle, classification, observability, and integrations
- [STRESS_TEST_README.md](STRESS_TEST_README.md): coverage, latency, and stress-test workflows
- `skills/simulation-service-tests/SKILL.md`: repo-local validation workflow
- `skills/tycho-cloudwatch-logs/SKILL.md`: CloudWatch log triage workflow

## License

MIT
