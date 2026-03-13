#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: run_suite.sh --repo <path> [--base-url <url>] [--chain-id <id>] [--suite <name>] [--disable-vm-pools] [--enable-vm-pools] [--allow-no-liquidity] [--allow-partial] [--stop]

Run a small end-to-end test suite:
1) start server (if not already running)
2) wait for /status ready
2.5) wait for VM pool readiness when VM pools are enabled on a supported chain
3) smoke test /simulate
4) coverage sweep (pool/protocol summary)
5) latency percentiles (p50/p90/p99)

Options:
  --repo               Repo root containing Cargo.toml
  --base-url           Base URL (default: http://localhost:3000)
  --chain-id           Runtime chain id (1 or 8453). Overrides CHAIN_ID env/.env.
  --suite              Pair suite for coverage/latency (default: core)
  --disable-vm-pools   Start server with ENABLE_VM_POOLS=false
  --enable-vm-pools    Start server with ENABLE_VM_POOLS=true (default; waits for vm_status=ready on supported chains)
  --allow-no-liquidity Allow no_liquidity responses with only no_pools failures
  --allow-partial      Allow partial_success responses (and their failures)
  --stop               Stop server when done (only if started by this script)
  -h, --help           Show this help

Tip: For mainnet variability, use --allow-partial --allow-no-liquidity for local runs.
USAGE
}

validate_chain_id() {
  case "$1" in
    1|8453) ;;
    *)
      echo "Error: unsupported chain id '$1'. Supported values: 1 (Ethereum), 8453 (Base)." >&2
      return 1
      ;;
  esac
}

load_chain_id_from_env_file() {
  local env_file="$1"
  if [[ ! -f "$env_file" ]]; then
    return 1
  fi
  local raw
  raw="$(grep -E '^[[:space:]]*(export[[:space:]]+)?CHAIN_ID[[:space:]]*=' "$env_file" | tail -n 1 || true)"
  if [[ -z "$raw" ]]; then
    return 1
  fi
  local value="${raw#*=}"
  value="${value%%#*}"
  value="${value#"${value%%[![:space:]]*}"}"
  value="${value%"${value##*[![:space:]]}"}"
  if [[ "$value" == \"*\" ]]; then
    value="${value#\"}"
    value="${value%\"}"
  elif [[ "$value" == \'*\' ]]; then
    value="${value#\'}"
    value="${value%\'}"
  fi
  if [[ -z "$value" ]]; then
    return 1
  fi
  echo "$value"
}

chain_label() {
  case "$1" in
    1) echo "ethereum" ;;
    8453) echo "base" ;;
    *) echo "chain-$1" ;;
  esac
}

repo=""
base_url="http://localhost:3000"
chain_id_arg=""
suite="core"
enable_vm_pools="true"
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
    --chain-id)
      chain_id_arg="$2"
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

resolved_chain_id="${chain_id_arg:-${CHAIN_ID:-}}"
if [[ -z "$resolved_chain_id" ]]; then
  resolved_chain_id="$(load_chain_id_from_env_file "$repo/.env" || true)"
fi
if [[ -z "$resolved_chain_id" ]]; then
  echo "Error: missing chain id. Pass --chain-id or set CHAIN_ID in env/.env." >&2
  exit 2
fi
validate_chain_id "$resolved_chain_id"
chain_id="$resolved_chain_id"
chain_name="$(chain_label "$chain_id")"
chain_supports_vm="false"
if [[ "$chain_id" == "1" ]]; then
  chain_supports_vm="true"
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
started_by_me="false"

echo "Base URL: $base_url"
echo "Chain: $chain_name ($chain_id)"
echo "Suite: $suite"
echo "Enable VM pools (requested): $enable_vm_pools"
echo "Allow no_liquidity: $allow_no_liquidity"
echo "Allow partial_success: $allow_partial"

simulate_allow_status="ready"
coverage_allow_status="ready"
latency_allow_status="ready"
simulate_flags=()
coverage_flags=()
latency_flags=()

if [[ "$allow_partial" == "true" ]]; then
  simulate_allow_status="${simulate_allow_status},partial_success"
  coverage_allow_status="${coverage_allow_status},partial_success"
  latency_allow_status="${latency_allow_status},partial_success"
  simulate_flags+=(--allow-failures)
  coverage_flags+=(--allow-failures)
  latency_flags+=(--allow-failures)
fi

if [[ "$allow_no_liquidity" == "true" ]]; then
  coverage_allow_status="${coverage_allow_status},no_liquidity"
  latency_allow_status="${latency_allow_status},no_liquidity"
  coverage_flags+=(--allow-no-pools)
  latency_flags+=(--allow-no-pools)
fi

if curl -s "$status_url" >/dev/null 2>&1; then
  echo "Server already responding at $status_url"
else
  echo "Starting server..."
  start_server_args=(--repo "$repo" --chain-id "$chain_id")
  if [[ "$enable_vm_pools" == "true" ]]; then
    start_server_args+=(--enable-vm-pools)
  elif [[ "$enable_vm_pools" == "false" ]]; then
    start_server_args+=(--env ENABLE_VM_POOLS=false)
  fi
  "$script_dir/start_server.sh" "${start_server_args[@]}"
  started_by_me="true"
fi

echo "Waiting for readiness..."
wait_timeout=300
wait_args=(
  --url "$status_url"
  --timeout "$wait_timeout"
  --interval 2
  --expect-chain-id "$chain_id"
)
if [[ "$enable_vm_pools" == "true" ]] && [[ "$chain_supports_vm" == "true" ]]; then
  # VM protocols can take much longer to ingest than native pools.
  wait_timeout=600
  wait_args=(
    --url "$status_url"
    --timeout "$wait_timeout"
    --interval 2
    --expect-chain-id "$chain_id"
    --require-vm-ready
    --require-vm-pools-min 1
  )
fi
"$script_dir/wait_ready.sh" "${wait_args[@]}"

# Resolve effective VM availability from runtime status so Base won't run VM-only assertions.
runtime_vm_enabled="$(STATUS_URL="$status_url" python3 - <<'PY'
import json
import os
import urllib.request

with urllib.request.urlopen(os.environ["STATUS_URL"], timeout=5) as response:
    status = json.loads(response.read().decode())

print("true" if bool(status.get("vm_enabled")) else "false")
PY
)"

echo "Runtime VM enabled (effective): $runtime_vm_enabled"

coverage_suite="${COVERAGE_SUITE:-$suite}"
latency_suite="${LATENCY_SUITE:-$suite}"
if [[ "$suite" == "core" ]] && [[ "$chain_id" == "1" ]]; then
  if [[ "$runtime_vm_enabled" == "true" ]]; then
    coverage_suite="${COVERAGE_SUITE:-coverage_core_vm}"
    latency_suite="${LATENCY_SUITE:-latency_core_vm}"
  else
    latency_suite="${LATENCY_SUITE:-latency_core}"
  fi
fi

echo "Coverage suite: $coverage_suite"
echo "Latency suite: $latency_suite"

echo "Smoke testing /simulate..."
# Bash 3.2 + nounset treats empty "${array[@]}" as unbound; guard optional args.
python3 "$script_dir/simulate_smoke.py" \
  --url "$simulate_url" \
  --chain-id "$chain_id" \
  --suite smoke \
  --allow-status "$simulate_allow_status" \
  --require-data \
  --validate-data \
  ${simulate_flags[@]+"${simulate_flags[@]}"}

echo "Encode smoke testing..."
python3 "$script_dir/encode_smoke.py" \
  --encode-url "$encode_url" \
  --simulate-url "$simulate_url" \
  --chain-id "$chain_id" \
  --repo "$repo" \
  --allow-status "$simulate_allow_status" \
  ${simulate_flags[@]+"${simulate_flags[@]}"}

echo "Coverage sweep..."
mkdir -p "$repo/logs"
python3 "$script_dir/coverage_sweep.py" \
  --url "$simulate_url" \
  --chain-id "$chain_id" \
  --suite "$coverage_suite" \
  --allow-status "$coverage_allow_status" \
  ${coverage_flags[@]+"${coverage_flags[@]}"} \
  --out "$repo/logs/coverage_sweep.json"

if [[ "$chain_id" == "8453" ]]; then
  # Keep the Base-native Aerodrome guard separate from the VM presence gates below.
  echo "Protocol presence checks (Aerodrome Slipstreams)..."
  python3 "$script_dir/coverage_sweep.py" \
    --url "$simulate_url" \
    --chain-id "$chain_id" \
    --suite aerodrome_presence \
    --allow-status "$coverage_allow_status" \
    ${coverage_flags[@]+"${coverage_flags[@]}"} \
    --expect-protocols aerodrome_slipstreams \
    --out "$repo/logs/coverage_aerodrome_presence.json"
fi

if [[ "$runtime_vm_enabled" == "true" ]]; then
  expected_vm_protocols=""
  case "$chain_id" in
    1)
      expected_vm_protocols="maverick_v2"
      ;;
    8453)
      expected_vm_protocols=""
      ;;
  esac

  if [[ -n "$expected_vm_protocols" ]]; then
    echo "Ensuring VM readiness for strict VM protocol checks..."
    "$script_dir/wait_ready.sh" \
      --url "$status_url" \
      --timeout 600 \
      --interval 2 \
      --expect-chain-id "$chain_id" \
      --require-vm-ready \
      --require-vm-pools-min 1

    echo "Protocol presence checks (chain-aware VM expectations)..."
    python3 "$script_dir/coverage_sweep.py" \
      --url "$simulate_url" \
      --chain-id "$chain_id" \
      --pair USDC:USDT \
      --pair USDT:USDC \
      --pair ETH:RETH \
      --allow-status "$coverage_allow_status" \
      ${coverage_flags[@]+"${coverage_flags[@]}"} \
      --expect-protocols "$expected_vm_protocols" \
      --out "$repo/logs/coverage_protocol_presence.json"

    echo "Protocol presence checks (Balancer)..."
    python3 "$script_dir/coverage_sweep.py" \
      --url "$simulate_url" \
      --chain-id "$chain_id" \
      --pair WETH:USDC \
      --allow-status "$coverage_allow_status" \
      ${coverage_flags[@]+"${coverage_flags[@]}"} \
      --expect-protocols balancer_v2 \
      --out "$repo/logs/coverage_balancer_presence.json"
  else
    echo "No chain-specific VM protocol expectation configured for chain $chain_id; skipping strict VM protocol gate."
  fi
else
  echo "Skipping VM protocol presence checks (runtime VM is effectively disabled)."
fi

echo "Latency percentiles..."
latency_requests="${LATENCY_REQUESTS:-200}"
if [[ "$runtime_vm_enabled" == "true" ]]; then
  latency_concurrency="${LATENCY_CONCURRENCY_VM:-4}"
else
  latency_concurrency="${LATENCY_CONCURRENCY:-8}"
fi
python3 "$script_dir/latency_percentiles.py" \
  --url "$simulate_url" \
  --chain-id "$chain_id" \
  --suite "$latency_suite" \
  --requests "$latency_requests" \
  --concurrency "$latency_concurrency" \
  --allow-status "$latency_allow_status" \
  ${latency_flags[@]+"${latency_flags[@]}"}

if [[ "$stop_after" == "true" ]] && [[ "$started_by_me" == "true" ]]; then
  echo "Stopping server..."
  "$script_dir/stop_server.sh" --repo "$repo"
fi

echo "Done."
