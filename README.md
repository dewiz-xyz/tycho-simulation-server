# Tycho Simulation Server

An HTTP service that brokers Tycho protocol streams into on-demand quote simulations.

## Description

The server ingests Tycho protocol updates, keeps an in-memory view of pool state, and exposes an HTTP API for requesting swap quotes or checking readiness. It runs on Axum atop Tokio and relies on the `tycho-simulation` crate for all pricing logic.

## Features

- Background ingestion of Tycho protocol streams with TVL-based filtering.
- `POST /simulate` endpoint that returns laddered amount-out simulated quotes with rich metadata.
- `GET /status` endpoint for readiness polling (native + VM block height, pool count).
- Structured logging with `tracing`.
- Fully asynchronous execution with Tokio.

## Prerequisites

- Rust (latest stable toolchain)
- Cargo

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
   Populate `.env` with your Tycho credentials and optional tuning knobs.
3. Build the binary:
   ```bash
   cargo build --release
   ```

## Configuration

The following environment variables are read at startup:

- `TYCHO_URL` – Tycho API base URL (default: `tycho-beta.propellerheads.xyz`)
- `TYCHO_API_KEY` – API key for authenticated Tycho access (**required**)
- `TVL_THRESHOLD` – Minimum TVL (in native units) for adding a pool to the stream (default: `100`)
- `TVL_KEEP_RATIO` – Fraction of `TVL_THRESHOLD` used to decide when to keep/remove pools (default: `0.2`)
- `PORT` – HTTP port (default: `3000`)
- `HOST` – Bind address (default: `127.0.0.1`)
- `RUST_LOG` – Logging filter (default: `info`)
- `QUOTE_TIMEOUT_MS` – Wall-clock timeout for an entire quote request (default: `150`)
- `POOL_TIMEOUT_NATIVE_MS` – Per-pool timeout for native integrations (default: `20`)
- `POOL_TIMEOUT_VM_MS` – Per-pool timeout for VM-backed integrations (default: `150`)
- `REQUEST_TIMEOUT_MS` – Request-level guard applied at handler, router adds +250ms headroom (default: `4000`)
- `TOKEN_REFRESH_TIMEOUT_MS` – Timeout for refreshing token metadata from Tycho (default: `1000`)
- `ENABLE_VM_POOLS` – Enable VM pool feeds (default: `false`)
- `GLOBAL_NATIVE_SIM_CONCURRENCY` – Global native simulation concurrency cap (default: `4 * num_cpus`)
- `GLOBAL_VM_SIM_CONCURRENCY` – Global VM simulation concurrency cap (default: `1 * num_cpus`)
- `STREAM_MEM_PURGE` – Purge jemalloc arenas on stream restarts/rebuilds (default: `true`)
- `STREAM_MEM_LOG` – Enable periodic + event-triggered memory snapshots (default: `true`)
- `STREAM_MEM_LOG_MIN_INTERVAL_SECS` – Minimum seconds between snapshots (default: `60`)
- `STREAM_MEM_LOG_MIN_NEW_PAIRS` – Only snapshot on stream updates with at least this many new pairs (default: `1000`)
- `STREAM_MEM_LOG_EMF` – Emit CloudWatch EMF metrics for snapshots (default: `true`)

Note: when concurrency caps are saturated or a pool would exceed the quote deadline, pools are skipped instead of queued. The response `meta.status` may become `partial_failure` with a `concurrency_limit` failure describing skipped counts.

## HTTP API

### `POST /simulate`

JSON body:

```bash
curl -X POST "http://localhost:3000/simulate" \
  -H "Content-Type: application/json" \
  -d '{
    "request_id": "req-123",
    "token_in": "0x6b175474e89094c44da98b954eedeac495271d0f",
    "token_out": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
    "amounts": ["1000000000000000000", "5000000000000000000"]
  }'
```

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
      "block_number": 19876543
    }
  ],
  "meta": {
    "status": "ready",
    "block_number": 19876543,
    "vm_block_number": 19876540,
    "matching_pools": 4,
    "candidate_pools": 4,
    "total_pools": 412,
    "failures": []
  }
}
```

`QuoteMeta.status` communicates warm-up, validation, and partial-failure states—HTTP status codes remain `200 OK`. `block_number` is the native stream block; `vm_block_number` is the last VM stream block when VM pools are enabled (it may be omitted while VM pools are disabled or still warming up).

Timeout behavior:
- Handler-level timeout returns `200 OK` with `PartialFailure`, includes `request_id`, `block_number`, `total_pools`, and a `Timeout` failure.
- Router-level timeout (rare fallback, applied only to `/simulate`) returns `200 OK` with `PartialFailure`, `request_id` may be empty, `block_number` is `0`, and `total_pools` is unset. Logs include `scope="router_timeout"`.
  - `/status` is not subject to router-level timeouts.

### `GET /status`

Returns readiness information for health checks and pollers:

```json
{
  "status": "ready",
  "block": 19876543,
  "vm_block": 19876540,
  "pools": 412
}
```

If the service is still ingesting initial state the endpoint responds with `503 Service Unavailable` and `"status": "warming_up"`.

### `GET /pools?protocol=curve&source=vm`

Returns the requested protocol pools currently present in the VM or native state store, including pool address and token metadata, to help diagnose stream coverage. Use `source=vm` (default), `source=native`, or `source=both`. Omit `protocol` to return all protocols. Limits are computed using the first two tokens in `token_addresses`.

Example:

```json
{
  "source": "vm",
  "vm_enabled": true,
  "vm_status": "ready",
  "vm_block": 19876540,
  "vm_pools": 412,
  "native_status": "ready",
  "native_block": 19876543,
  "native_pools": 310,
  "protocol": "curve",
  "protocol_pools": 115,
  "data": [
    {
      "source": "vm",
      "pair_id": "0x...",
      "pool_address": "0x...",
      "pool_name": "Curve.fi DAI/USDC/USDT",
      "protocol_system": "vm:curve",
      "protocol_type_name": "curve_pool",
      "token_addresses": ["0x...", "0x..."],
      "token_symbols": ["DAI", "USDC"],
      "limit_in": "12345",
      "limit_out": "67890"
    }
  ]
}
```

## Usage

Launch the server after exporting the necessary environment variables:

```bash
cargo run --release
```

The application binds to the configured `HOST:PORT` and begins streaming protocol data before serving HTTP requests.

## Testing

- `cargo test` runs unit and integration tests.
- Integration tests live under `tests/integration/` and are wired by `tests/integration/main.rs`.
- The memory plateau check uses jemalloc (`jemallocator` + `jemalloc-ctl`) to assert that allocator bytes return to baseline after a resync warm-up. Jemalloc is the default allocator for the service.

Run only the integration tests:

```bash
cargo test --test integration
```

Run just the jemalloc plateau test:

```bash
cargo test --test integration jemalloc_memory_plateau_after_reset
```

## Dependencies

- `tycho-simulation` – protocol stream and simulator client
- `tokio` – asynchronous runtime
- `axum` – HTTP router and extractors
- `serde`/`serde_json` – serialization
- `anyhow` – error propagation
- `tracing` & `tracing-subscriber` – structured logging

## Project Structure

- `src/api` – router wiring for public endpoints
- `src/config` – environment loading and logging setup
- `src/handlers` – HTTP handlers (`quote`, `status`) and stream ingester
- `src/models` – request/response models and shared state
- `src/services` – stream builder and quote engine
- `src/main.rs` – application entrypoint

## License

MIT
