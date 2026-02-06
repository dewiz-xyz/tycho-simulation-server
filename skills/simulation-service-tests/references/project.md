# Tycho Simulation Server (repo context)

## What this service does
- Rust Axum service that ingests Tycho protocol streams, keeps an in-memory pool state, and exposes HTTP endpoints for quote simulation.
- Main binary: `tycho-price-service` (see `src/main.rs`).

## Local run
- Create `.env` from `.env.example` and set `TYCHO_API_KEY` (required).
- Tycho health checks expect `Authorization: <TYCHO_API_KEY>` (no `Bearer` prefix).
- Start the server:
  ```bash
  cargo run --release
  ```
- Default bind: `127.0.0.1:3000` (override with `HOST`/`PORT`).

## Readiness
- `GET /status` returns:
  - `200 OK` with `{ "status": "ready", "block": <u64>, "pools": <usize> }`
  - `503 Service Unavailable` with `{ "status": "warming_up", ... }`
- Cold starts can take several minutes (3–5+ mins; longer with VM pools); wait before concluding readiness is stuck.

## Quote simulation
- `POST /simulate` request body:
  ```json
  {
    "request_id": "req-123",
    "token_in": "0x...",
    "token_out": "0x...",
    "amounts": ["1000000000000000000", "5000000000000000000"]
  }
  ```
- HTTP status is always `200 OK`. Validate `meta.status` and `meta.failures` to determine readiness or partial failures.

## Repo verification suite (recommended)
- One-shot suite (start → wait_ready → smoke → coverage → latency):
  - `scripts/run_suite.sh --repo . --suite core --stop`
  - Add `--enable-vm-pools` when validating VM feeds (Curve/Balancer).
  - Use `--allow-partial` or `--allow-no-liquidity` if you expect partial/no-liquidity responses.
- Individual runners:
  - `python3 scripts/simulate_smoke.py --suite smoke`
  - `python3 scripts/encode_smoke.py --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .`
  - `python3 scripts/coverage_sweep.py --suite core --out logs/coverage_sweep.json`
  - `python3 scripts/latency_percentiles.py --suite core --requests 300 --concurrency 50`
- See `STRESS_TEST_README.md` for suites, defaults, and latency knobs.

## Useful commands
- Format: `cargo fmt`
- Lint: `cargo clippy --all-targets --all-features -- -D warnings`
- Test: `cargo test`
