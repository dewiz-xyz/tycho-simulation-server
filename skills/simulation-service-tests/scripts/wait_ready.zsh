#!/bin/zsh
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: wait_ready.zsh [--url <status_url>] [--timeout <seconds>] [--interval <seconds>]

Poll the /status endpoint until it returns ready or times out.

Options:
  --url        Status URL (default: http://localhost:3000/status)
  --timeout    Timeout in seconds (default: 180)
  --interval   Poll interval in seconds (default: 2)
  -h, --help   Show this help
USAGE
}

url="http://localhost:3000/status"
timeout=180
interval=2

while [[ $# -gt 0 ]]; do
  case "$1" in
    --url)
      url="$2"
      shift 2
      ;;
    --timeout)
      timeout="$2"
      shift 2
      ;;
    --interval)
      interval="$2"
      shift 2
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

start_time="$(date +%s)"

while true; do
  response="$(curl -s -w "\n%{http_code}" "$url" || true)"
  body="${response%$'\n'*}"
  status_code="${response##*$'\n'}"

  if [[ "$status_code" == "200" ]] && [[ "$body" == *"\"ready\""* ]]; then
    echo "ready"
    exit 0
  fi

  now="$(date +%s)"
  if (( now - start_time >= timeout )); then
    echo "Timed out waiting for readiness." >&2
    echo "Last response ($status_code): $body" >&2
    exit 1
  fi

  sleep "$interval"
done
