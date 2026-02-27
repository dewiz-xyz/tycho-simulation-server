---
name: simulation-service-tests
description: Run, test, benchmark, upgrade, and deploy the Tycho simulation server in this repo (tycho-simulation-server). Use when starting/stopping the service, waiting for /status readiness, validating /simulate across many token pairs/pools/protocols (including VM pools like curve/balancer/maverick), computing p50/p90/p99 latencies, running load/stress tests, or verifying upgrades and deployments (cargo fmt/clippy/nextest/build, docker build, CDK synth/diff/deploy).
metadata:
  short-description: Tycho simulation service tests
---

# Simulation Service Tests

## Quick start

1. Confirm the repo root (expect `Cargo.toml` and `src/`).
2. Ensure `.env` exists and contains `TYCHO_API_KEY` (avoid logging it). For manual health checks, use `Authorization: <TYCHO_API_KEY>` (no `Bearer` prefix).
3. Pick a chain context for this run (`--chain-id 1` for Ethereum, `--chain-id 8453` for Base). Scripts fail fast if neither `--chain-id` nor `CHAIN_ID` is set.
4. Start the server (keeps a PID file in the repo root):
   ```bash
   cd /path/to/tycho-simulation-server
   scripts/start_server.sh --repo . --chain-id 1
   ```
5. Wait for readiness and verify chain:
   ```bash
   scripts/wait_ready.sh --url http://localhost:3000/status --expect-chain-id 1
   ```
   - If it stays `warming_up`, wait longer (3–5+ minutes on a cold start, longer with VM pools) or re-run with a higher `--timeout`.

## One-shot end-to-end suite

Run start → wait_ready → smoke → coverage → latency:
```bash
cd /path/to/tycho-simulation-server
scripts/run_suite.sh --repo . --chain-id 1 --suite core --stop
```

`run_suite.sh` is the stable, VM-aware repo verification path. With VM pools enabled (the default), it waits for `vm_status=ready` and at least one VM pool before running VM protocol presence checks, so cold starts can take longer than native-only runs.

`run_suite.sh` smoke checks require non-empty `data` and validate pool entry schema (`amounts_out`, `gas_used`, `gas_in_sell`, monotonicity, and `block_number`). `gas_in_sell` is a decimal-string sell-token amount computed from request-scoped pricing inputs and can legitimately be `"0"` when gas reporting or pricing inputs are unavailable.

VM pools (Curve/Balancer/Maverick feeds) are enabled by default. The supported fast path is to exclude them entirely:
```bash
scripts/run_suite.sh --repo . --chain-id 1 --suite core --disable-vm-pools --stop
```

There is no repo-runner mode that keeps VM pools enabled while skipping VM readiness waiting.

Tolerate partial failures/no-liquidity while still running the suite:
```bash
scripts/run_suite.sh --repo . --chain-id 8453 --suite core --allow-partial --allow-no-liquidity --stop
```

## Smoke test /simulate

- Use curated chain-aware presets (token/suite sets differ by chain).
- Remember: `/simulate` returns `200 OK` even on partial failure; use `meta.status` / `meta.failures`.

Examples:
```bash
python3 scripts/simulate_smoke.py --chain-id 1 --suite smoke
python3 scripts/simulate_smoke.py --chain-id 8453 --pair USDC:WETH --pair WETH:USDC
python3 scripts/simulate_smoke.py --chain-id 1 --list-tokens
```

For stricter checks (fail on empty data and validate pool entries, including `gas_in_sell`; `"0"` is valid):
```bash
python3 scripts/simulate_smoke.py --chain-id 1 --suite smoke --require-data --validate-data
```

Allow partial responses explicitly:
```bash
python3 scripts/simulate_smoke.py --chain-id 8453 --suite smoke --allow-status ready,partial_success --allow-failures
```

## Smoke test /encode

- `/encode` follows the latest schema (singleSwap-only execution).
- The smoke test performs two `/simulate` calls to pick pools, then posts a 2-hop route.
- Route tokens are chain-aware (`scripts/presets.py` `default_encode_route`).
- Default addresses can be overridden via `.env` (`COW_SETTLEMENT_CONTRACT`, `TYCHO_ROUTER_ADDRESS`).

Examples:
```bash
python3 scripts/encode_smoke.py --chain-id 1 --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .
python3 scripts/encode_smoke.py --chain-id 8453 --allow-status ready,partial_success --allow-failures --verbose
```

## Pool/protocol coverage sweeps

`coverage_sweep.py` runs a suite of pairs and reports which protocols/pools appear in responses:
```bash
python3 scripts/coverage_sweep.py --chain-id 1 --suite core --out logs/coverage_sweep.json
python3 scripts/coverage_sweep.py --chain-id 1 --suite v4_candidates
python3 scripts/coverage_sweep.py --chain-id 1 --suite exploratory_protocols --allow-failures --allow-no-pools
```

VM pool feeds are controlled by `ENABLE_VM_POOLS` (default: `true`). Use `ENABLE_VM_POOLS=false` (or `scripts/run_suite.sh --chain-id 1 --disable-vm-pools ...`) to turn them off. See `references/protocols.md`.

`coverage_sweep.py` now reports both winner-path protocols (from successful `data` entries) and candidate-path protocols (from `data` plus `meta.pool_results`). `--expect-protocols` validates against candidate-path presence so prechecked/skipped pools still count toward coverage expectations.

Protocol assertions should be chain-aware. Example (Ethereum VM probe):
```bash
python3 scripts/coverage_sweep.py --chain-id 1 --pair USDC:USDT --pair USDT:USDC --pair ETH:RETH --expect-protocols maverick_v2
```

Allow `no_liquidity` responses with only `no_pools` failures:
```bash
python3 scripts/coverage_sweep.py --chain-id 8453 --suite core --allow-no-pools
```

## Latency percentiles (p50/p90/p99)

`latency_percentiles.py` measures only "good" responses by default (`meta.status=ready`, no `meta.failures`). `scripts/run_suite.sh --suite core` uses the narrower `latency_core` pair set for this phase so the percentile pass stays on consistently ready paths:
```bash
python3 scripts/latency_percentiles.py --chain-id 1 --suite core --requests 300 --concurrency 50
python3 scripts/latency_percentiles.py --chain-id 8453 --suite core --requests 300 --concurrency 50
```

## Load / stress testing

- Prefer the repo suite + percentile runner:
  - `scripts/run_suite.sh --repo . --chain-id 1 --suite core`
  - `python3 scripts/latency_percentiles.py --chain-id 1 --requests 2000 --concurrency 50 --suite core`
- See `STRESS_TEST_README.md` for defaults and knobs (`LATENCY_REQUESTS`, `LATENCY_CONCURRENCY`, `LATENCY_CONCURRENCY_VM`).

## Maintenance checks

Run a CI-like pass:
```bash
zsh skills/simulation-service-tests/scripts/run_checks.zsh --repo /path/to/tycho-simulation-server
```

## Memory tracking (jemalloc)

1. Run the ignored harnesses and record output lines in `.codex/memory-task/service-memory-tracking.md`:
   ```bash
   cargo test --test integration protocol_reset_memory::memory_spike_breakdown_harness -- --ignored --nocapture
   cargo test --test integration protocol_reset_memory::shared_db_rebuild_stress_harness -- --ignored --nocapture
   ```
2. Append a diff table based on the latest `memory_breakdown` line:
   ```bash
   python3 skills/simulation-service-tests/scripts/memory_diff.py --repo /path/to/tycho-simulation-server
   ```

## Upgrade workflow (typical)

1. Bump `tycho-simulation` tag/version (or other deps).
2. Run: `cargo fmt`, `cargo clippy ...`, `cargo nextest run`, `cargo build --release`.
3. Run the end-to-end suite (`run_suite.zsh`) on both chains (typically one run with `--chain-id 1`, one with `--chain-id 8453`; with/without VM pools as needed).
4. If infra changes are involved: `npm ci`, `npx cdk synth`, `npx cdk diff`.

## Deploy workflow (CDK + Docker)

See `references/deploy.md`.

## Included scripts

- Repo (source of truth): `scripts/start_server.sh`, `scripts/stop_server.sh`, `scripts/wait_ready.sh`, `scripts/run_suite.sh`, plus the Python runners in `scripts/`.
- Repo (source of truth): `scripts/start_server.sh`, `scripts/stop_server.sh`, `scripts/wait_ready.sh`, `scripts/run_suite.sh`, plus Python runners in `scripts/`.
- Skill fallback scripts: chain-aware mirrors under `skills/simulation-service-tests/scripts/` (used when repo scripts are unavailable).
- Skill utilities: `scripts/run_checks.zsh` (CI-like `cargo fmt/clippy/nextest/build` + optional `cdk synth`/`docker build`).

## References

- `references/project.md` – repo commands, endpoints, and stress-test details.
- `references/encode.md` – `/encode` schema and smoke-testing notes.
- `references/protocols.md` – exchange/protocol feeds per chain and test implications.
- `references/tycho-deps.md` – Tycho/Propeller Heads context and docs.
- `references/upgrade.md` – checklist for dependency upgrades.
- `references/deploy.md` – CDK + Docker deployment notes.
