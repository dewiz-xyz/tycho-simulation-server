# Tycho Simulation Server

An HTTP service that brokers Tycho protocol streams into on-demand quote simulations.

## Description

The server ingests Tycho protocol updates, keeps an in-memory view of pool state, and exposes an HTTP API for requesting swap quotes or checking readiness. It runs on Axum atop Tokio and relies on the `tycho-simulation` crate for all pricing logic.

## Features

- Background ingestion of Tycho protocol streams with TVL-based filtering.
- `POST /simulate` endpoint that returns laddered amount-out simulated quotes with rich metadata.
- `POST /encode` endpoint that returns CoW-ready calldata for client-provided routes.
- `GET /status` endpoint for readiness polling (block height, pool count).
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
- `COW_SETTLEMENT_CONTRACT` – Default settlement address for `scripts/encode_smoke.py` (optional)
- `TYCHO_ROUTER_ADDRESS` – Default router address for `scripts/encode_smoke.py` (optional)
- `RUST_LOG` – Logging filter (default: `info`)
- `QUOTE_TIMEOUT_MS` – Wall-clock timeout for an entire quote request (default: `150`)
- `POOL_TIMEOUT_NATIVE_MS` – Per-pool timeout for native integrations (default: `20`)
- `POOL_TIMEOUT_VM_MS` – Per-pool timeout for VM-backed integrations (default: `150`)
- `REQUEST_TIMEOUT_MS` – Request-level guard applied at handler, router adds +250ms headroom (default: `4000`)
- `TOKEN_REFRESH_TIMEOUT_MS` – Timeout for refreshing token metadata from Tycho (default: `1000`)
- `ENABLE_VM_POOLS` – Enable VM pool feeds (default: `false`)
- `GLOBAL_NATIVE_SIM_CONCURRENCY` – Global native simulation concurrency cap (default: `4 * num_cpus`, clamped to `256`)
- `GLOBAL_VM_SIM_CONCURRENCY` – Global VM simulation concurrency cap (default: `2 * num_cpus`, clamped to `256`)

Note: when concurrency caps are saturated or a pool would exceed the quote deadline, pools are skipped instead of queued. The response `meta.status` may become `partial_failure` with a `concurrency_limit` failure describing skipped counts.

## Docs

- `docs/encode_example.md` – `/encode` schema walkthrough with shape-focused request/response examples.

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
    "matching_pools": 4,
    "candidate_pools": 4,
    "total_pools": 412,
    "failures": []
  }
}
```

`QuoteMeta.status` communicates warm-up, validation, and partial-failure states—HTTP status codes remain `200 OK`.

Timeout behavior:
- `/simulate` handler-level timeouts return `200 OK` with `partial_failure`, including a `timeout` failure.
- `/simulate` router-level timeouts return `200 OK` with `partial_failure`. Logs include `scope="router_timeout"`.
- `/encode` timeouts return `408 Request Timeout` with `{ error, requestId }`.
  - `/status` is not subject to router-level timeouts.

### `POST /encode`

`POST /encode` builds Tycho router calldata (`singleSwap`, `sequentialSwap`, or `splitSwap`) for a client-provided route. It **re-simulates** each pool swap, derives per-hop/per-swap amounts internally, and enforces only the route-level `minAmountOut`.

Notes:
- The request shape follows `RouteEncodeRequest` (camelCase fields).
- `settlementAddress` and `tychoRouterAddress` are required.
- `swapKind` describes the route shape (`SimpleSwap`, `MultiSwap`, `MegaSwap`).
- Requests include route-level `amountIn` and `minAmountOut`, plus segment `shareBps` and swap `splitBps` for splits.
- Hops are sequential in the order provided; there is no hop-level share.
- The encoder chooses `singleSwap` for one swap, `sequentialSwap` for multi-hop without splits, and `splitSwap` when any split is present.
- Per-hop and per-swap amounts are not accepted and not returned.
- The response is `RouteEncodeResponse` with `normalizedRoute` and settlement `interactions`.
- Errors return 4xx/5xx with `{ error, requestId }`.

Example request (shape only):

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

The response includes ordered `interactions[]` (approve + router call). Computed per-hop/per-swap amounts are logged server-side, not returned.

### `GET /status`

Returns readiness information for health checks and pollers:

```json
{
  "status": "ready",
  "block": 19876543,
  "pools": 412
}
```

If the service is still ingesting initial state the endpoint responds with `503 Service Unavailable` and `"status": "warming_up"`.

## Usage

Launch the server after exporting the necessary environment variables:

```bash
cargo run --release
```

The application binds to the configured `HOST:PORT` and begins streaming protocol data before serving HTTP requests.

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
