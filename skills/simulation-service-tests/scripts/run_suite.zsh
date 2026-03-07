#!/bin/zsh
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: run_suite.zsh --repo <path> [--base-url <url>] [--chain-id <id>] [--suite <name>] [--disable-vm-pools] [--enable-vm-pools] [--wait-vm-ready] [--allow-no-liquidity] [--allow-partial] [--stop]

Run a small end-to-end test suite:
1) start server (if not already running)
2) wait for /status ready
2.5) (optional) wait for VM pool readiness
3) smoke test /simulate
4) coverage sweep (pool/protocol summary)
5) latency percentiles (p50/p90/p99)

Options:
  --repo               Repo root containing Cargo.toml
  --base-url           Base URL (default: http://localhost:3000)
  --chain-id           Runtime chain id (1 or 8453). Overrides CHAIN_ID env/.env.
  --suite              Pair suite for coverage/latency (default: core)
  --disable-vm-pools   Start server with ENABLE_VM_POOLS=false
  --enable-vm-pools    Start server with ENABLE_VM_POOLS=true (default)
  --wait-vm-ready      Wait for vm_status=ready after /status is ready (when VM is effectively enabled)
  --allow-no-liquidity Allow no_liquidity responses with only no_pools failures
  --allow-partial      Allow partial_success responses (and their failures)
  --stop               Stop server when done (only if started by this script)
  -h, --help           Show this help
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

repo=""
base_url="http://localhost:3000"
chain_id_arg=""
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
repo_script="$repo/scripts/run_suite.sh"

if [[ -f "$repo_script" ]]; then
  args=(--repo "$repo" --base-url "$base_url" --suite "$suite")
  if [[ -n "$chain_id_arg" ]]; then
    args+=(--chain-id "$chain_id_arg")
  fi
  if [[ "$enable_vm_pools" == "true" ]]; then
    args+=(--enable-vm-pools)
  elif [[ "$enable_vm_pools" == "false" ]]; then
    args+=(--disable-vm-pools)
  fi
  if [[ "$wait_vm_ready" == "true" ]]; then
    args+=(--wait-vm-ready)
  fi
  if [[ "$allow_no_liquidity" == "true" ]]; then
    args+=(--allow-no-liquidity)
  fi
  if [[ "$allow_partial" == "true" ]]; then
    args+=(--allow-partial)
  fi
  if [[ "$stop_after" == "true" ]]; then
    args+=(--stop)
  fi
  exec bash "$repo_script" "${args[@]}"
fi

status_url="${base_url%/}/status"
simulate_url="${base_url%/}/simulate"

skill_dir="$(cd "$(dirname "$0")" && pwd)"
started_by_me="false"

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

echo "Base URL: $base_url"
echo "Chain ID: $chain_id"
echo "Suite: $suite"
echo "Enable VM pools (requested): $enable_vm_pools"

simulate_allow_status="ready"
coverage_allow_status="ready"
latency_allow_status="ready"
typeset -a simulate_flags=()
typeset -a coverage_flags=()
typeset -a latency_flags=()

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
  "$skill_dir/start_server.zsh" "${start_server_args[@]}"
  started_by_me="true"
fi

echo "Waiting for readiness..."
wait_timeout=300
if [[ "$enable_vm_pools" == "true" ]]; then
  wait_timeout=600
fi
"$skill_dir/wait_ready.zsh" --url "$status_url" --timeout "$wait_timeout" --interval 2 --expect-chain-id "$chain_id"

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

if [[ "$wait_vm_ready" == "true" ]]; then
  if [[ "$runtime_vm_enabled" == "true" ]]; then
    "$skill_dir/wait_ready.zsh" --url "$status_url" --timeout 600 --interval 2 --expect-chain-id "$chain_id" --require-vm-ready --require-vm-pools-min 1
  else
    echo "Skipping --wait-vm-ready because runtime VM is effectively disabled on this chain/config."
  fi
fi

echo "Smoke testing /simulate..."
python3 "$skill_dir/simulate_smoke.py" --url "$simulate_url" --chain-id "$chain_id" --suite smoke --allow-status "$simulate_allow_status" --require-data --validate-data "${simulate_flags[@]}"

echo "Coverage sweep..."
mkdir -p "$repo/logs"
python3 "$skill_dir/coverage_sweep.py" --url "$simulate_url" --chain-id "$chain_id" --suite "$suite" --allow-status "$coverage_allow_status" "${coverage_flags[@]}" --out "$repo/logs/coverage_sweep.json"

echo "Latency percentiles..."
latency_requests="${LATENCY_REQUESTS:-200}"
if [[ "$runtime_vm_enabled" == "true" ]]; then
  latency_concurrency="${LATENCY_CONCURRENCY_VM:-4}"
else
  latency_concurrency="${LATENCY_CONCURRENCY:-8}"
fi
python3 "$skill_dir/latency_percentiles.py" --url "$simulate_url" --chain-id "$chain_id" --suite "$suite" --requests "$latency_requests" --concurrency "$latency_concurrency" --allow-status "$latency_allow_status" "${latency_flags[@]}"

if [[ "$stop_after" == "true" ]] && [[ "$started_by_me" == "true" ]]; then
  echo "Stopping server..."
  "$skill_dir/stop_server.zsh" --repo "$repo"
fi

echo "Done."
