---
name: simulation-service-tests
description: Run, test, benchmark, upgrade, and deploy the Tycho simulation server in this repo (tycho-simulation-server). Use when starting/stopping the service, waiting for /status readiness, validating /simulate across many token pairs/pools/protocols (including VM pools like curve/balancer/maverick), computing p50/p90/p99 latencies, running load/stress tests, or verifying upgrades and deployments (cargo fmt/clippy/test, docker build, CDK synth/diff/deploy).
metadata:
  short-description: Tycho simulation service tests
---

# Simulation Service Tests

## Quick start

1. Confirm the repo root (expect `Cargo.toml` and `src/`).
2. Ensure `.env` exists and contains `TYCHO_API_KEY` (avoid logging it). For manual health checks, use `Authorization: <TYCHO_API_KEY>` (no `Bearer` prefix).
3. Start the server (keeps a PID file in the repo root):
   ```bash
   cd /path/to/tycho-simulation-server
   scripts/start_server.sh --repo .
   ```
4. Wait for readiness:
   ```bash
   scripts/wait_ready.sh --url http://localhost:3000/status
   ```
   - If it stays `warming_up`, wait longer (3–5+ minutes on a cold start, longer with VM pools) or re-run with a higher `--timeout`.

## One-shot end-to-end suite

Run start → wait_ready → smoke → coverage → latency:
```bash
cd /path/to/tycho-simulation-server
scripts/run_suite.sh --repo . --suite core --stop
```

`run_suite.sh` smoke checks require non-empty `data` and validate pool entry schema (`amounts_out`, `gas_used`, `gas_in_sell`, monotonicity, and `block_number`). `gas_in_sell` is a decimal-string sell-token amount: ETH/WETH sell tokens use direct `1.0` conversion from gas-in-ETH, non-native sell tokens may return a non-zero value when a sell-token/native or sell-token/wrapped-native spot source is available, and `"0"` remains valid when gas reporting inputs are unavailable or no usable spot source exists.

VM pools (Curve/Balancer/Maverick feeds) are enabled by default. To exclude them:
```bash
scripts/run_suite.sh --repo . --suite core --disable-vm-pools --stop
```

If you need to tolerate partial failures or empty-liquidity responses while still running the suite:
```bash
scripts/run_suite.sh --repo . --suite core --allow-partial --allow-no-liquidity --stop
```
With `--allow-partial`, smoke validation now accepts partial-ladder pool entries as long as `amounts_out` and `gas_used` stay aligned, monotonic, and non-empty.

## Smoke test /simulate

- Use the curated presets (supports many mainnet tokens + suites).
- Remember: `/simulate` returns `200 OK` even on partial failure; use `meta.status` / `meta.failures`.

Examples:
```bash
python3 scripts/simulate_smoke.py --suite smoke
python3 scripts/simulate_smoke.py --pair DAI:USDC --pair WETH:USDC
python3 scripts/simulate_smoke.py --list-tokens
```

For stricter checks (fail on empty data and validate pool entries, including `gas_in_sell`; `"0"` is still valid when reporting inputs or spot conversion data are unavailable):
```bash
python3 scripts/simulate_smoke.py --suite smoke --require-data --validate-data
```

Allow partial responses explicitly:
```bash
python3 scripts/simulate_smoke.py --suite smoke --allow-status ready,partial_success --allow-failures
```
Quote handler logs now keep request-guard and zero-result timeout completions at `WARN` (`scope="handler_timeout"`), while partial-success responses that still return usable quotes log at `INFO` (`scope="handler_complete"`).
When `--allow-failures` is set together with `--validate-data`, partial ladders are accepted.

## Smoke test /encode

- `/encode` follows the latest schema (singleSwap-only execution).
- The smoke test performs two `/simulate` calls to pick pools, then posts a 2-hop route.
- Default addresses can be overridden via `.env` (`COW_SETTLEMENT_CONTRACT`, `TYCHO_ROUTER_ADDRESS`).

Examples:
```bash
python3 scripts/encode_smoke.py --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .
```
```bash
python3 scripts/encode_smoke.py --allow-status ready,partial_success --allow-failures --verbose
```

## Pool/protocol coverage sweeps

`coverage_sweep.py` runs a suite of pairs and reports which protocols/pools appear in responses:
```bash
python3 scripts/coverage_sweep.py --suite core --out logs/coverage_sweep.json
python3 scripts/coverage_sweep.py --suite v4_candidates
```

VM pool feeds (Curve/Balancer/Maverick) are controlled by `ENABLE_VM_POOLS` (default: `true`). Use `ENABLE_VM_POOLS=false` (or `scripts/run_suite.sh --disable-vm-pools ...`) to turn them off. See `references/protocols.md`.

To assert specific protocol presence (derived from `pool_name` prefixes), use:
```bash
python3 scripts/coverage_sweep.py --suite core --expect-protocols uniswap_v3,uniswap_v4,maverick_v2
```

Allow `no_liquidity` responses with only `no_pools` failures:
```bash
python3 scripts/coverage_sweep.py --suite core --allow-no-pools
```

## Latency percentiles (p50/p90/p99)

`latency_percentiles.py` measures only “good” responses by default (`meta.status=ready`, no `meta.failures`):
```bash
python3 scripts/latency_percentiles.py --suite core --requests 300 --concurrency 50
```

## Load / stress testing

- Prefer the repo suite + percentile runner:
  - `scripts/run_suite.sh --repo . --suite core` (smoke + coverage + p50/p90/p99)
  - `python3 scripts/latency_percentiles.py --requests 2000 --concurrency 50 --suite core` (heavier load)
- See `STRESS_TEST_README.md` for defaults and knobs (`LATENCY_REQUESTS`, `LATENCY_CONCURRENCY`, `LATENCY_CONCURRENCY_VM`).

## Maintenance checks

Run a CI-like pass:
```bash
zsh skills/simulation-service-tests/scripts/run_checks.zsh --repo /path/to/tycho-simulation-server
```

## Memory tracking (jemalloc)

1. Run the ignored harnesses and record the output lines in `.codex/memory-task/service-memory-tracking.md`:
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
2. Run: `cargo fmt`, `cargo clippy ...`, `cargo test`, `cargo build --release`.
3. Run the end-to-end suite (`scripts/run_suite.sh`) with and without VM pools.
4. If infra changes are involved: `npm ci`, `npx cdk synth`, `npx cdk diff`.

## Deploy workflow (CDK + Docker)

See `references/deploy.md`.

## Included scripts

- Repo scripts are the canonical entrypoints: `scripts/start_server.sh`, `scripts/stop_server.sh`, `scripts/wait_ready.sh`, `scripts/run_suite.sh`, plus the Python runners in `scripts/`.
- Skill-local wrappers and helper utilities are not kept in lockstep with every repo-script change in this pass. Use repo `scripts/` for start/stop/wait/suite flows.
- Skill utilities: `scripts/run_checks.zsh` (CI-like `cargo fmt/clippy/test/build` + optional `cdk synth`/`docker build`).

## References

- `references/project.md` – repo commands, endpoints, and stress-test details.
- `references/encode.md` – `/encode` schema and smoke-testing notes.
- `references/protocols.md` – which exchanges/protocol feeds this server subscribes to (and how to test them).
- `references/tycho-deps.md` – Tycho/Propeller Heads context and docs.
- `references/upgrade.md` – checklist for dependency upgrades.
- `references/deploy.md` – CDK + Docker deployment notes.
