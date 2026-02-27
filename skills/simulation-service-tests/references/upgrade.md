# Upgrade checklist (tycho-simulation-server)

Use this when bumping `tycho-simulation` (or changing stream logic, timeouts, or concurrency).

## Dependency bump
1. Update `Cargo.toml` (e.g., `tycho-simulation` git tag).
2. Run `cargo build --release` to refresh `Cargo.lock`.

## Local correctness checks
1. `cargo fmt`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test`

## End-to-end API verification

Run verification per target chain.

### Ethereum (`CHAIN_ID=1`)
1. Start and wait for readiness:
   - `cd /path/to/tycho-simulation-server`
   - `scripts/start_server.sh --repo . --chain-id 1`
   - `scripts/wait_ready.sh --url http://localhost:3000/status --expect-chain-id 1`
2. Run smoke tests + pool/protocol sweep:
   - `python3 scripts/simulate_smoke.py --chain-id 1 --suite smoke`
   - `python3 scripts/coverage_sweep.py --chain-id 1 --suite core --out logs/coverage_sweep.json`
3. Run latency percentiles:
   - `python3 scripts/latency_percentiles.py --chain-id 1 --suite core --requests 300 --concurrency 50`

### Base (`CHAIN_ID=8453`)
1. Start and wait for readiness:
   - `scripts/start_server.sh --repo . --chain-id 8453`
   - `scripts/wait_ready.sh --url http://localhost:3000/status --expect-chain-id 8453`
2. Run smoke tests + sweep:
   - `python3 scripts/simulate_smoke.py --chain-id 8453 --suite smoke --allow-status ready,partial_success --allow-failures`
   - `python3 scripts/coverage_sweep.py --chain-id 8453 --suite core --allow-status ready,partial_success --allow-failures --out logs/coverage_sweep.base.json`
3. Run latency percentiles:
   - `python3 scripts/latency_percentiles.py --chain-id 8453 --suite core --requests 300 --concurrency 50 --allow-status ready,partial_success --allow-failures`

## VM pools (Curve/Balancer/Maverick)
- VM checks are meaningful only when `/status.vm_enabled=true`.
- For Ethereum comparison runs, include one VM-disabled pass:
  - `scripts/run_suite.sh --repo . --chain-id 1 --suite core --disable-vm-pools --stop`

## Load testing / regressions
- Prefer `scripts/run_suite.sh` and/or `scripts/latency_percentiles.py` with higher `--requests`/`--concurrency`.
- Keep note of chain id, machine profile, `LATENCY_REQUESTS`, and concurrency when comparing results.
