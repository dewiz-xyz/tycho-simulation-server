# Simulation Test Suite for Tycho Simulation Server

This repo ships a lightweight test suite for the `/simulate` endpoint. It covers smoke checks, protocol/pool coverage, and latency percentiles.

## Overview

The suite (`scripts/run_suite.sh`) performs:

- **Server lifecycle**: start (if needed), wait for `/status`, optional stop
- **Smoke checks**: quick sanity pairs for `/simulate`
- **Coverage sweep**: summarizes pools and protocols seen
- **Latency percentiles**: p50/p90/p99 with configurable concurrency

## Prerequisites

1. **Python 3** (stdlib only)
2. **Rust toolchain** for `cargo run --release`
3. **Tycho API key** in `.env` (`TYCHO_API_KEY=...`)

## Quick Start

Run the full suite (start → wait → smoke → coverage → latency):

```bash
scripts/run_suite.sh --repo . --stop
```

Run with VM pools enabled:

```bash
scripts/run_suite.sh --repo . --enable-vm-pools --stop
```

## Suite Configuration

- **Suites** (see `scripts/presets.py`):
  - `smoke`, `core`, `latency_core`, `coverage_core_vm`, `extended`, `stables`, `lst`, `governance`, `v4_candidates`
  - `extended` includes lower-liquidity pairs removed from `core`.
  - `run_suite.sh --suite core` keeps the broad `core` set for smoke and coverage, then uses `latency_core` for the percentile leg so latency stays on consistently ready paths.
  - With VM pools enabled, `run_suite.sh --suite core` uses `coverage_core_vm` for the coverage leg so the gate stays broad without depending on Balancer pools that currently return partial ladders at the default request sizes.
- **Latency defaults** (override via env):
  - `LATENCY_REQUESTS` (default: 200)
  - `LATENCY_CONCURRENCY` (default: 8)
  - `LATENCY_CONCURRENCY_VM` (default: 4)
- **Amounts**: per-token default ladders (e.g., 6-decimal stables, capped WBTC/WETH). Override with `--amounts` on each script.

## Running Individual Steps

Smoke test:
```bash
python3 scripts/simulate_smoke.py --suite smoke
```

Coverage sweep (writes JSON report):
```bash
python3 scripts/coverage_sweep.py --suite core --out logs/coverage_sweep.json
```

Latency percentiles:
```bash
python3 scripts/latency_percentiles.py --suite core --requests 200 --concurrency 8
```

## Output

- Coverage report: `logs/coverage_sweep.json`
- Server log: `logs/tycho-sim-server.log`

## Troubleshooting

- **Server not running**: `scripts/run_suite.sh` starts it for you. For manual control:
  - `scripts/start_server.sh --repo .`
  - `scripts/stop_server.sh --repo .`
- **Readiness timeouts**: check `logs/tycho-sim-server.log` for startup errors.
- **Partial successes**: `/simulate` returns `200 OK` even when `meta.status=partial_success`. The suite requires `ready` by default.

## Customization Notes

- Use `--allow-status ready,partial_success` and `--allow-failures` on the Python scripts if you want to tolerate partial successes.
- Change suites or token lists in `scripts/presets.py` to match your coverage needs.
