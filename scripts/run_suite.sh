#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: run_suite.sh --repo <path> [--base-url <url>] [--suite <name>] [--disable-vm-pools] [--enable-vm-pools] [--wait-vm-ready] [--allow-no-liquidity] [--allow-partial] [--stop]

Run a small end-to-end test suite:
1) start server (if not already running)
2) wait for /status ready
2.5) (optional) wait for VM pool readiness
3) smoke test /simulate
4) coverage sweep (pool/protocol summary)
5) latency percentiles (p50/p90/p99)

Options:
  --repo             Repo root containing Cargo.toml
  --base-url         Base URL (default: http://localhost:3000)
  --suite            Pair suite for coverage/latency (default: core)
  --disable-vm-pools Start server with ENABLE_VM_POOLS=false
  --enable-vm-pools  Start server with ENABLE_VM_POOLS=true (default)
  --wait-vm-ready    Wait for vm_status=ready after /status is ready (only when VM pools are enabled)
  --allow-no-liquidity  Allow no_liquidity responses with only no_pools failures
  --allow-partial    Allow partial_success responses (and their failures)
  --stop             Stop server when done (only if started by this script)
  -h, --help         Show this help

Tip: For mainnet variability, use --allow-partial --allow-no-liquidity for local runs.
USAGE
}

repo=""
base_url="http://localhost:3000"
suite="core"
enable_vm_pools="true"
wait_vm_ready="false"
allow_no_liquidity="false"
allow_partial="false"
stop_after="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="$2"
      shift 2
      ;;
    --base-url)
      base_url="$2"
      shift 2
      ;;
    --suite)
      suite="$2"
      shift 2
      ;;
    --disable-vm-pools)
      enable_vm_pools="false"
      shift 1
      ;;
    --enable-vm-pools)
      enable_vm_pools="true"
      shift 1
      ;;
    --wait-vm-ready)
      wait_vm_ready="true"
      shift 1
      ;;
    --allow-no-liquidity)
      allow_no_liquidity="true"
      shift 1
      ;;
    --allow-partial)
      allow_partial="true"
      shift 1
      ;;
    --stop)
      stop_after="true"
      shift 1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -z "$repo" ]]; then
  echo "Error: --repo is required" >&2
  usage
  exit 2
fi

repo="$(cd "$repo" && pwd)"
status_url="${base_url%/}/status"
simulate_url="${base_url%/}/simulate"
encode_url="${base_url%/}/encode"

script_dir="$(cd "$(dirname "$0")" && pwd)"
started_by_me="false"

echo "Base URL: $base_url"
echo "Suite: $suite"
echo "Enable VM pools: $enable_vm_pools"
echo "Wait VM ready: $wait_vm_ready"
echo "Allow no_liquidity: $allow_no_liquidity"
echo "Allow partial_success: $allow_partial"

simulate_allow_status="ready"
coverage_allow_status="ready"
latency_allow_status="ready"
allow_failures_flag=""
allow_no_pools_flag=""

if [[ "$allow_partial" == "true" ]]; then
  simulate_allow_status="${simulate_allow_status},partial_success"
  coverage_allow_status="${coverage_allow_status},partial_success"
  latency_allow_status="${latency_allow_status},partial_success"
  allow_failures_flag="--allow-failures"
fi

if [[ "$allow_no_liquidity" == "true" ]]; then
  coverage_allow_status="${coverage_allow_status},no_liquidity"
  latency_allow_status="${latency_allow_status},no_liquidity"
  allow_no_pools_flag="--allow-no-pools"
fi

if curl -s "$status_url" >/dev/null 2>&1; then
  echo "Server already responding at $status_url"
else
  echo "Starting server..."
  if [[ "$enable_vm_pools" == "true" ]]; then
    "$script_dir/start_server.sh" --repo "$repo" --enable-vm-pools
  elif [[ "$enable_vm_pools" == "false" ]]; then
    "$script_dir/start_server.sh" --repo "$repo" --env ENABLE_VM_POOLS=false
  else
    "$script_dir/start_server.sh" --repo "$repo"
  fi
  started_by_me="true"
fi

echo "Waiting for readiness..."
wait_timeout=300
wait_args=(--url "$status_url" --timeout "$wait_timeout" --interval 2)
if [[ "$enable_vm_pools" == "true" ]]; then
  # VM protocols can take much longer to ingest than native pools.
  wait_timeout=600
  wait_args=(--url "$status_url" --timeout "$wait_timeout" --interval 2 --require-vm-ready --require-vm-pools-min 1)
fi
"$script_dir/wait_ready.sh" "${wait_args[@]}"

if [[ "$enable_vm_pools" == "true" ]] && [[ "$wait_vm_ready" == "true" ]]; then
  echo "Waiting for VM pool readiness..."
  STATUS_URL="$status_url" python3 -u - <<'PY'
import json
import os
import time
import urllib.request

status_url = os.environ["STATUS_URL"]
deadline = time.time() + 300

while True:
    with urllib.request.urlopen(status_url, timeout=5) as r:
        s = json.loads(r.read().decode())
    vm_enabled = s.get("vm_enabled")
    vm_status = s.get("vm_status")
    vm_pools = s.get("vm_pools")
    if not vm_enabled:
        break
    if vm_status == "ready":
        break
    if time.time() > deadline:
        raise SystemExit(f"timeout waiting for vm ready (vm_status={vm_status} vm_pools={vm_pools})")
    time.sleep(5)
PY
fi

echo "Smoke testing /simulate..."
python3 "$script_dir/simulate_smoke.py" --url "$simulate_url" --suite smoke --allow-status "$simulate_allow_status" $allow_failures_flag

echo "Encode smoke testing..."
python3 "$script_dir/encode_smoke.py" --encode-url "$encode_url" --simulate-url "$simulate_url" --repo "$repo" --allow-status "$simulate_allow_status" $allow_failures_flag

echo "Coverage sweep..."
mkdir -p "$repo/logs"
python3 "$script_dir/coverage_sweep.py" --url "$simulate_url" --suite "$suite" --allow-status "$coverage_allow_status" $allow_failures_flag $allow_no_pools_flag --out "$repo/logs/coverage_sweep.json"

if [[ "$enable_vm_pools" == "true" ]]; then
  echo "Protocol presence checks (Maverick + Rocketpool)..."
  python3 "$script_dir/coverage_sweep.py" \
    --url "$simulate_url" \
    --pair USDC:USDT \
    --pair USDT:USDC \
    --pair ETH:RETH \
    --allow-status "$coverage_allow_status" \
    $allow_failures_flag \
    $allow_no_pools_flag \
    --expect-protocols rocketpool,maverick_v2 \
    --out "$repo/logs/coverage_protocol_presence.json"
fi

echo "Latency percentiles..."
latency_requests="${LATENCY_REQUESTS:-200}"
if [[ "$enable_vm_pools" == "true" ]]; then
  latency_concurrency="${LATENCY_CONCURRENCY_VM:-4}"
else
  latency_concurrency="${LATENCY_CONCURRENCY:-8}"
fi
python3 "$script_dir/latency_percentiles.py" --url "$simulate_url" --suite "$suite" --requests "$latency_requests" --concurrency "$latency_concurrency" --allow-status "$latency_allow_status" $allow_failures_flag $allow_no_pools_flag

if [[ "$stop_after" == "true" ]] && [[ "$started_by_me" == "true" ]]; then
  echo "Stopping server..."
  "$script_dir/stop_server.sh" --repo "$repo"
fi

echo "Done."
