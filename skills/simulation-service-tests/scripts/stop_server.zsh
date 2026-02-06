#!/bin/zsh
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: stop_server.zsh [--repo <path>] [--force]

Stop the tycho-simulation-server started by start_server.zsh.

Options:
  --repo       Path to repo root (default: current directory)
  --force      Send SIGKILL if the process does not exit
  -h, --help   Show this help
USAGE
}

repo="."
force="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="$2"
      shift 2
      ;;
    --force)
      force="true"
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
repo_script="$repo/scripts/stop_server.sh"

if [[ -f "$repo_script" ]]; then
  args=(--repo "$repo")
  if [[ "$force" == "true" ]]; then
    args+=(--force)
  fi
  exec bash "$repo_script" "${args[@]}"
fi

pid_file="$repo/.tycho-sim-server.pid"
if [[ ! -f "$pid_file" ]]; then
  echo "No pid file found at $pid_file."
  exit 0
fi

pid="$(cat "$pid_file" 2>/dev/null || true)"
if [[ -z "$pid" ]]; then
  echo "Pid file is empty; removing." >&2
  rm -f "$pid_file"
  exit 0
fi

if ! kill -0 "$pid" 2>/dev/null; then
  echo "Process $pid is not running; removing pid file." >&2
  rm -f "$pid_file"
  exit 0
fi

echo "Stopping server (pid $pid)..."
kill "$pid"

for _ in {1..20}; do
  if ! kill -0 "$pid" 2>/dev/null; then
    rm -f "$pid_file"
    echo "Stopped."
    exit 0
  fi
  sleep 0.25
done

if [[ "$force" == "true" ]]; then
  echo "Process still running; sending SIGKILL." >&2
  kill -9 "$pid" || true
  rm -f "$pid_file"
  exit 0
fi

echo "Process still running; re-run with --force to SIGKILL." >&2
exit 1
