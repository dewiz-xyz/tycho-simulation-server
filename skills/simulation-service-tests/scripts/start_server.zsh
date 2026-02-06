#!/bin/zsh
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: start_server.zsh [--repo <path>] [--log-file <path>] [--env KEY=VALUE] [--enable-vm-pools]

Start the tycho-simulation-server from a repo checkout.

Options:
  --repo       Path to repo root (default: current directory)
  --log-file   Log file path (default: <repo>/logs/tycho-sim-server.log)
  --env        Export KEY=VALUE before starting (repeatable)
  --enable-vm-pools  Shortcut for --env ENABLE_VM_POOLS=true
  -h, --help   Show this help
USAGE
}

repo="."
log_file=""
typeset -a env_overrides=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="$2"
      shift 2
      ;;
    --log-file)
      log_file="$2"
      shift 2
      ;;
    --env)
      env_overrides+=("$2")
      shift 2
      ;;
    --enable-vm-pools)
      env_overrides+=("ENABLE_VM_POOLS=true")
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

repo="$(cd "$repo" && pwd)"
repo_script="$repo/scripts/start_server.sh"

if [[ -f "$repo_script" ]]; then
  args=(--repo "$repo")
  if [[ -n "$log_file" ]]; then
    args+=(--log-file "$log_file")
  fi
  for pair in "${env_overrides[@]}"; do
    args+=(--env "$pair")
  done
  exec bash "$repo_script" "${args[@]}"
fi

if [[ ! -f "$repo/Cargo.toml" ]]; then
  echo "Error: Cargo.toml not found in $repo" >&2
  exit 1
fi

pid_file="$repo/.tycho-sim-server.pid"
if [[ -f "$pid_file" ]]; then
  pid="$(cat "$pid_file" 2>/dev/null || true)"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    echo "Server already running (pid $pid)."
    exit 0
  fi
  rm -f "$pid_file"
fi

if [[ -f "$repo/.env" ]]; then
  # Load .env into the environment without printing secrets.
  set -a
  source "$repo/.env"
  set +a
else
  echo "Warning: .env not found; ensure TYCHO_API_KEY is set." >&2
fi

if [[ -z "${TYCHO_API_KEY:-}" ]]; then
  echo "Warning: TYCHO_API_KEY not set; server may fail to start." >&2
fi

if [[ -z "${RUST_LOG:-}" ]]; then
  export RUST_LOG=info
fi

for pair in "${env_overrides[@]}"; do
  export "${pair}"
done

if [[ -z "$log_file" ]]; then
  mkdir -p "$repo/logs"
  log_file="$repo/logs/tycho-sim-server.log"
fi

(
  cd "$repo"
  nohup cargo run --release > "$log_file" 2>&1 &
  echo $! > "$pid_file"
)

echo "Started tycho-simulation-server."
echo "PID: $(cat "$pid_file")"
echo "Log: $log_file"
echo "Tip: tail -f $log_file"
