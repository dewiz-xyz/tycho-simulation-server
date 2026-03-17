# Tycho Simulation Server (repo context)

## What this service does
- Rust Axum service that ingests Tycho protocol streams, keeps an in-memory pool state, and exposes HTTP endpoints for quote simulation.
- Main binary: `tycho-price-service` (see `src/main.rs`).

## Local run
- Create `.env` from `.env.example` and set `TYCHO_API_KEY` (required).
- Set `CHAIN_ID` (`1` for Ethereum, `8453` for Base) or pass `--chain-id` to scripts.
- Tycho health checks expect `Authorization: <TYCHO_API_KEY>` (no `Bearer` prefix).
- Start the server:
  ```bash
  cargo run --release
  ```
- Default bind: `127.0.0.1:3000` (override with `HOST`/`PORT`).

## Readiness
- `GET /status` returns:
  - `200 OK` with `{ "status": "ready", "chain_id": <u64>, "block": <u64>, "pools": <usize>, ... }`
  - `503 Service Unavailable` with `{ "status": "warming_up", ... }`
- Cold starts can take several minutes (3–5+ mins; longer with VM pools); wait before concluding readiness is stuck.
- `scripts/wait_ready.sh --expect-chain-id <id>` should be used to guard against hitting the wrong deployment.

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
- HTTP status is always `200 OK`. Validate `meta.result_quality`, `meta.partial_kind`, and `meta.failures`; `meta.status` alone is not enough to decide whether a response is quoteable.
- The live `/simulate` contract is summarized in [README.md](/Users/pedrobergamini/Dev/dewiz/tycho-simulation-server/README.md) and detailed for integrations in [docs/simulate_example.md](/Users/pedrobergamini/Dev/dewiz/tycho-simulation-server/docs/simulate_example.md).

## Repo verification suite (recommended)
- One-shot suite (start → wait_ready → smoke → coverage → latency):
  - `scripts/run_suite.sh --repo . --chain-id 1 --suite core --stop`
  - `scripts/run_suite.sh --repo . --chain-id 8453 --suite core --stop`
  - VM pools are enabled by default; add `--disable-vm-pools` to skip VM feeds.
  - On Ethereum, VM-enabled runs wait for `vm_status=ready` and `vm_pools>=1` before the VM protocol probes run.
  - On Base, the runner also executes a strict native Aerodrome presence probe after the broader coverage sweep.
  - The repo suite is intentionally strict: it keeps verification on healthy quoteable paths and fails on request-visible degradation even when the API response is still contract-valid.
  - There is no repo-runner mode that leaves VM pools enabled while skipping that wait; use `--disable-vm-pools` for the native-only fast path.
  - Use `--allow-failures` on the individual Python helpers only when you want to tolerate request-visible failures on otherwise usable `complete`/`partial` results.
  - Use `--allow-no-liquidity` only when you intentionally want to tolerate `no_liquidity + no_results` responses that report `no_pools`.
  - Smoke validation checks non-empty `data` and pool fields including `gas_in_sell` (decimal string, `"0"` is valid when reporting is disabled/unavailable; pricing inputs are request-scoped).
  - On Ethereum, `--suite core` keeps the tuned defaults: `coverage_core_vm` + `latency_core_vm` when VM is enabled, `latency_core` when it is not.
- Individual runners:
  - `python3 scripts/simulate_smoke.py --chain-id 1 --suite smoke`
  - `python3 scripts/encode_smoke.py --chain-id 1 --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .`
  - `python3 scripts/coverage_sweep.py --chain-id 1 --suite core --out logs/coverage_sweep.json`
  - `python3 scripts/coverage_sweep.py --chain-id 1 --suite erc4626_allowlisted --expect-protocols erc4626`
  - `python3 scripts/coverage_sweep.py --chain-id 1 --suite erc4626_negative --allow-no-pools`
  - `python3 scripts/coverage_sweep.py --chain-id 8453 --suite aerodrome_presence --allow-failures --expect-protocols aerodrome_slipstreams`
  - `python3 scripts/latency_percentiles.py --chain-id 1 --suite core --requests 300 --concurrency 50`
- See `STRESS_TEST_README.md` for suites, defaults, and latency knobs.

## Useful commands
- Format: `cargo fmt`
- Lint: `cargo clippy --all-targets --all-features -- -D warnings`
- Test: `cargo nextest run`
- ERC4626 rollout checks:
  - `python3 scripts/coverage_sweep.py --chain-id 1 --suite erc4626_allowlisted --expect-protocols erc4626`
  - `python3 scripts/coverage_sweep.py --chain-id 1 --suite erc4626_negative --allow-no-pools`
- Base Aerodrome rollout checks:
  - `python3 scripts/coverage_sweep.py --chain-id 8453 --suite aerodrome_presence --allow-failures --expect-protocols aerodrome_slipstreams`
