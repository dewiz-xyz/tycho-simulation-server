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
1. Start and wait for readiness:
   - `cd /path/to/tycho-simulation-server`
   - `scripts/start_server.sh --repo .`
   - `scripts/wait_ready.sh --url http://localhost:3000/status`
2. Run smoke tests + pool/protocol sweep:
   - `python3 scripts/simulate_smoke.py --suite smoke`
   - `python3 scripts/coverage_sweep.py --suite core --out logs/coverage_sweep.json`
3. Run latency percentiles (keep the config with results):
   - `python3 scripts/latency_percentiles.py --suite core --requests 300 --concurrency 50`

## VM pools (Curve/Balancer)
Run the same suite with VM pools enabled:
- `scripts/run_suite.sh --repo . --suite core --enable-vm-pools --stop`

## Load testing / regressions
- Prefer `scripts/run_suite.sh` and/or `scripts/latency_percentiles.py` with higher `--requests`/`--concurrency`.
- Keep note of the machine profile, `LATENCY_REQUESTS`, and concurrency when comparing results.
