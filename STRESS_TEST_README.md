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
4. **Chain context** via `--chain-id` or `CHAIN_ID` (`1` Ethereum, `8453` Base)

## Quick Start

Run the full suite on Ethereum (start → wait → smoke → coverage → latency):

```bash
scripts/run_suite.sh --repo . --chain-id 1 --stop
```

Run the full suite on Base:

```bash
scripts/run_suite.sh --repo . --chain-id 8453 --stop
```

Run a native-only Ethereum suite without VM pools:

```bash
scripts/run_suite.sh --repo . --chain-id 1 --disable-vm-pools --stop
```

## Suite Configuration

- **Suites** (see `scripts/presets.py`, chain-aware):
  - Ethereum: `smoke`, `core`, `latency_core`, `coverage_core_vm`, `latency_core_vm`, `extended`, `stables`, `lst`, `governance`, `v4_candidates`, `exploratory_protocols`, `erc4626_allowlisted`, `erc4626_negative`
  - Base: `smoke`, `core`, `extended`, `stables`, `lst`, `governance`, `v4_candidates`
  - `scripts/run_suite.sh --suite core` keeps the tuned Ethereum defaults: `coverage_core_vm` + `latency_core_vm` when VM is enabled, `latency_core` when it is not.
- **Latency defaults** (override via env):
  - `LATENCY_REQUESTS` (default: 200)
  - `LATENCY_CONCURRENCY` (default: 8)
  - `LATENCY_CONCURRENCY_VM` (default: 4)
- **Amounts**: pair-specific ladders when configured, otherwise token-based decimal defaults. Override with `--amounts` on each script.

## Running Individual Steps

Smoke test:
```bash
python3 scripts/simulate_smoke.py --chain-id 1 --suite smoke
```

Coverage sweep (writes JSON report):
```bash
python3 scripts/coverage_sweep.py --chain-id 1 --suite core --out logs/coverage_sweep.json
```

Latency percentiles:
```bash
python3 scripts/latency_percentiles.py --chain-id 1 --suite core --requests 200 --concurrency 8
```

## Output

- Coverage report: `logs/coverage_sweep.json`
- Server log: `logs/tycho-sim-server.log`

## Troubleshooting

- **Server not running**: `scripts/run_suite.sh` starts it for you. For manual control:
  - `scripts/start_server.sh --repo . --chain-id 1`
  - `scripts/stop_server.sh --repo .`
- **Readiness timeouts**: on Ethereum with VM pools enabled, `scripts/run_suite.sh` waits for VM readiness automatically. Check `logs/tycho-sim-server.log` for startup errors.
- **Wrong chain target**: use `scripts/wait_ready.sh --expect-chain-id <id>` to assert the running deployment.
- **Partial successes**: `/simulate` returns `200 OK` even when `meta.status=partial_success`. The suite requires `ready` by default.

## Customization Notes

- Use `--allow-status ready,partial_success` and `--allow-failures` on Python scripts if you want to tolerate partial successes.
- Change suites or token lists in `scripts/presets.py` per chain.
