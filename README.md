# Tycho Simulation Server

An HTTP service that brokers Tycho protocol streams into on-demand quote simulations.

## Description

The server ingests Tycho protocol updates, keeps an in-memory view of pool state, and exposes an HTTP API for requesting swap quotes, route encoding, and readiness checks. It runs on Axum atop Tokio and relies on the `tycho-simulation` crate for pricing and pool-state execution.

## Features

- Background ingestion of Tycho protocol streams with TVL-based filtering.
- `POST /simulate` for laddered amount-out quote simulations with rich metadata.
- `POST /encode` for CoW-ready calldata generation from client-provided routes.
- `GET /status` for readiness polling, chain verification, and VM stream diagnostics.
- Structured logging with `tracing`.
- Fully asynchronous execution with Tokio.

## Prerequisites

- Rust stable and Cargo
- Python 3 for the bundled verification scripts under `scripts/`
- Node.js and npm only if you need the AWS CDK pipeline/service stacks

## Installation

1. Clone the repository:
   ```bash
   git clone https://github.com/dewiz-xyz/tycho-simulation-server.git
   cd tycho-simulation-server
   ```
2. Configure the environment:
   ```bash
   cp .env.example .env
   ```
   `.env.example` contains sample and locally tuned values. Runtime defaults still come from the Rust config code.
3. Build the binary:
   ```bash
   cargo build --release
   ```

## Configuration

The following environment variables are read by the server at startup:

- `TYCHO_API_KEY` – API key for authenticated Tycho access (**required**)
- `CHAIN_ID` – Runtime chain ID (**required**); supported values: `1` (Ethereum), `8453` (Base)
- `RPC_URL` – Optional JSON-RPC endpoint matching `CHAIN_ID`, used for background `eth_gasPrice` refreshes
- `GAS_PRICE_REFRESH_INTERVAL_MS` – Poll interval for `eth_gasPrice` refreshes (default: `5000`)
- `GAS_PRICE_FAILURE_TOLERANCE` – Disable gas reporting when consecutive refresh failures exceed this value (default: `50`)
- `TVL_THRESHOLD` – Minimum TVL, in native-token units, for adding a pool to the stream (default: `100`)
- `TVL_KEEP_RATIO` – Fraction of `TVL_THRESHOLD` used to decide when to keep or remove pools (default: `0.2`)
- `PORT` – HTTP port (default: `3000`)
- `HOST` – Bind address (default: `127.0.0.1`)
- `RUST_LOG` – Logging filter (default: `info`)
- `QUOTE_TIMEOUT_MS` – Wall-clock timeout for an entire quote request (default: `150`)
- `POOL_TIMEOUT_NATIVE_MS` – Per-pool timeout for native integrations (default: `20`)
- `POOL_TIMEOUT_VM_MS` – Per-pool timeout for VM-backed integrations (default: `150`)
- `REQUEST_TIMEOUT_MS` – Request-level guard applied at the handler; the router adds `250ms` headroom (default: `4000`)
- `TOKEN_REFRESH_TIMEOUT_MS` – Timeout for refreshing token metadata from Tycho (default: `1000`)
- `ENABLE_VM_POOLS` – Enable VM pool feeds (default: `true`)
- `GLOBAL_NATIVE_SIM_CONCURRENCY` – Global native simulation concurrency cap (default: `4 * num_cpus`)
- `GLOBAL_VM_SIM_CONCURRENCY` – Global VM simulation concurrency cap (default: `1 * num_cpus`)
- `STREAM_STALE_SECS` – Consider a stream unhealthy if no updates arrive within this many seconds (default: `120`)
- `STREAM_MISSING_BLOCK_BURST` – Missing-block errors allowed within the missing-block window before triggering a restart (default: `3`)
- `STREAM_MISSING_BLOCK_WINDOW_SECS` – Window size for missing-block burst counting (default: `60`)
- `STREAM_ERROR_BURST` – Stream errors allowed within the error window before triggering a restart (default: `3`)
- `STREAM_ERROR_WINDOW_SECS` – Window size for stream error burst counting (default: `60`)
- `RESYNC_GRACE_SECS` – Grace period after a resync starts before treating the stream as stale (default: `60`)
- `STREAM_RESTART_BACKOFF_MIN_MS` – Minimum restart backoff (default: `500`)
- `STREAM_RESTART_BACKOFF_MAX_MS` – Maximum restart backoff (default: `30000`)
- `STREAM_RESTART_BACKOFF_JITTER_PCT` – Jitter fraction applied to restart backoff (default: `0.2`)
- `READINESS_STALE_SECS` – Readiness stale threshold for native and VM readiness checks (default: `120`)
- `STREAM_MEM_PURGE` – Purge jemalloc arenas on stream restarts and rebuilds (default: `true`)
- `STREAM_MEM_LOG` – Enable periodic and event-triggered memory snapshots (default: `true`)
- `STREAM_MEM_LOG_MIN_INTERVAL_SECS` – Minimum seconds between snapshots (default: `60`)
- `STREAM_MEM_LOG_MIN_NEW_PAIRS` – Only snapshot on stream updates with at least this many new pairs (default: `1000`)
- `STREAM_MEM_LOG_EMF` – Emit CloudWatch EMF metrics for snapshots (default: `true`)

Notes:

- The defaults above come from the Rust config in `src/config/mod.rs`. `.env.example` intentionally uses different sample values for some knobs such as TVL, timeouts, concurrency, and memory logging.
- `COW_SETTLEMENT_CONTRACT` and `TYCHO_ROUTER_ADDRESS` are not runtime server config. They are optional helper values used by `scripts/encode_smoke.py`.
- When concurrency caps are saturated or a pool would exceed the quote deadline, pools are skipped instead of queued. `meta.status` remains an operational signal, while `meta.result_quality` and `meta.pool_results` explain quote completeness and per-pool anomalies.

## Usage

Launch the server after exporting the required environment variables:

```bash
cargo run --release
```

The application binds to the configured `HOST:PORT` and begins streaming protocol data before serving HTTP requests.

## Validation

CI currently runs these commands:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run
cargo build --release
```

For an end-to-end local verification pass, use the repo harness:

```bash
scripts/run_suite.sh --repo . --chain-id 1 --stop
scripts/run_suite.sh --repo . --chain-id 8453 --stop
```

Useful `run_suite.sh` flags:

- `--disable-vm-pools` for a native-only Ethereum run
- `--allow-partial` to tolerate `partial_success`
- `--allow-no-liquidity` to tolerate `no_liquidity` responses that only report `no_pools`

Script helpers:

- `scripts/start_server.sh` – start the server with a repo-local PID file and log file
- `scripts/wait_ready.sh` – poll `/status` and optionally enforce `chain_id` and VM readiness
- `scripts/stop_server.sh` – stop a server started by `start_server.sh`
- `scripts/run_suite.sh` – start, wait, smoke test, encode smoke test, coverage sweep, and latency pass

For deeper coverage, latency, and stress-test knobs, see `STRESS_TEST_README.md`.

## Docs And Tooling

- `docs/encode_example.md` – `/encode` schema walkthrough with shape-focused request and response examples
- `STRESS_TEST_README.md` – verification, coverage, latency, and stress-test workflows
- `skills/simulation-service-tests/SKILL.md` – repo-local workflow for local verification, upgrades, and maintenance checks
- `skills/tycho-cloudwatch-logs/SKILL.md` – repo-local workflow for production CloudWatch log inspection

Infrastructure note:

- This repo includes AWS CDK pipeline and service stacks under `bin/` and `lib/`.
- Use `npm ci` to install the CDK dependencies.
- Use `npx cdk synth` to synthesize the stacks when working on infra changes.

## HTTP API

### `POST /simulate`

JSON body:

```bash
curl -X POST "http://localhost:3000/simulate" \
  -H "Content-Type: application/json" \
  -d '{
    "request_id": "req-123",
    "auction_id": "auction-42",
    "token_in": "0x6b175474e89094c44da98b954eedeac495271d0f",
    "token_out": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
    "amounts": ["1000000000000000000", "5000000000000000000"]
  }'
```

`auction_id` is optional. When present, it is echoed back as `meta.auction_id`.

Response body:

```json
{
  "request_id": "req-123",
  "data": [
    {
      "pool": "uniswapv3-1",
      "pool_name": "UniswapV3::DAI/USDC",
      "pool_address": "0x...",
      "amounts_out": ["999000000", "4985000000"],
      "gas_used": [210000, 210000],
      "gas_in_sell": "630000000000000",
      "block_number": 19876543
    }
  ],
  "meta": {
    "status": "ready",
    "result_quality": "complete",
    "block_number": 19876543,
    "vm_block_number": 19876540,
    "matching_pools": 4,
    "candidate_pools": 4,
    "total_pools": 412,
    "auction_id": "auction-42",
    "failures": []
  }
}
```

`QuoteMeta.status` communicates warm-up, validation, and request reliability states. `meta.result_quality` communicates quote completeness (`complete`, `partial`, `no_results`, `request_level_failure`). HTTP status codes remain `200 OK`.

`meta.pool_results` contains anomaly-only per-pool outcomes (`partial_output`, `zero_output`, `skipped_concurrency`, `skipped_deadline`, `skipped_precheck`, `timed_out`, `simulator_error`, `internal_error`). `meta.vm_unavailable=true` indicates VM pools were skipped because VM state was not ready.

`gas_in_sell` is computed per pool from the latest cached `eth_gasPrice` from `RPC_URL`, the request-scoped `spot_price(ETH, sellToken)`, and the pool's last `gas_used` ladder entry. It is returned in sell-token base units. When spot price, cached gas price, gas usage, or gas reporting are unavailable, it is `"0"`.

`block_number` is the native stream block. `vm_block_number` is the last VM stream block when VM pools are enabled and available.

Timeout behavior:

- `/simulate` handler-level timeouts return `200 OK` with `partial_success`, including a `timeout` failure
- `/simulate` router-level timeouts return `200 OK` with `partial_success`; logs include `scope="router_timeout"`
- `/encode` timeouts return `408 Request Timeout` with `{ error, requestId }`
- `/status` is not subject to router-level timeouts

### `POST /encode`

`POST /encode` builds Tycho router calldata (`singleSwap`, `sequentialSwap`, or `splitSwap`) for a client-provided route. It re-simulates each pool swap, derives per-hop and per-swap amounts internally, and enforces only the route-level `minAmountOut`.

Notes:

- The request shape follows `RouteEncodeRequest` with camelCase fields
- `settlementAddress` and `tychoRouterAddress` are required
- `swapKind` describes the route shape: `SimpleSwap`, `MultiSwap`, or `MegaSwap`
- Requests include route-level `amountIn` and `minAmountOut`, plus segment `shareBps` and swap `splitBps` for splits
- Hops are sequential in the order provided; there is no hop-level share
- The encoder chooses `singleSwap` for one swap, `sequentialSwap` for multi-hop routes without splits, and `splitSwap` when any split is present
- Per-hop and per-swap amounts are not accepted and are not returned
- The response is `RouteEncodeResponse` with ordered settlement `interactions` and optional `debug`
- Errors return 4xx or 5xx with `{ error, requestId }`

Example request:

```bash
curl -X POST "http://localhost:3000/encode" \
  -H "Content-Type: application/json" \
  -d '{
    "chainId": 1,
    "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
    "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
    "amountIn": "1000000000000000000",
    "minAmountOut": "1000000",
    "settlementAddress": "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
    "tychoRouterAddress": "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35",
    "swapKind": "SimpleSwap",
    "segments": [
      {
        "kind": "SimpleSwap",
        "shareBps": 0,
        "hops": [
          {
            "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
            "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            "swaps": [
              {
                "pool": { "protocol": "uniswap_v3", "componentId": "pool-id" },
                "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
                "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                "splitBps": 0
              }
            ]
          }
        ]
      }
    ],
    "requestId": "req-encode-1"
  }'
```

The response includes ordered `interactions[]` such as approval and router-call interactions. Computed per-hop and per-swap amounts are logged server-side, not returned.

### `GET /status`

Returns readiness information for health checks and pollers:

```json
{
  "status": "ready",
  "chain_id": 1,
  "block": 19876543,
  "pools": 412,
  "vm_enabled": true,
  "vm_status": "ready",
  "vm_block": 19876540,
  "vm_pools": 36,
  "vm_restarts": 0,
  "vm_last_update_age_ms": 1200
}
```

`vm_status` is one of `disabled`, `warming_up`, `rebuilding`, or `ready`. Optional fields such as `vm_last_error`, `vm_rebuild_duration_ms`, and `vm_last_update_age_ms` are omitted when they are not relevant.

If the service is still ingesting initial state, the endpoint responds with `503 Service Unavailable` and `"status": "warming_up"`.

The readiness scripts in `scripts/wait_ready.sh` and `scripts/run_suite.sh` rely on `chain_id`, `vm_enabled`, `vm_status`, and `vm_pools` to confirm they are talking to the expected runtime.

## Project Structure

- `src/api` – router wiring for public endpoints
- `src/config` – environment loading and logging setup
- `src/handlers` – HTTP handlers and stream ingesters
- `src/models` – request and response models plus shared state
- `src/services` – quote engine, encoding flow, gas-price refresh, and stream builders
- `src/main.rs` – application entrypoint
- `scripts/` – local server lifecycle helpers and verification harnesses
- `tests/integration/` – integration coverage wired by `tests/integration/main.rs`

## License

MIT
