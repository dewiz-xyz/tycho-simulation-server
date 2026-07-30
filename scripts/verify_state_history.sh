#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: verify_state_history.sh [--repo <path>]

Start the local Postgres + MinIO stack and verify the state-history storage contract.

Options:
  --repo      Path to repo root (default: current directory)
  -h, --help  Show this help
USAGE
}

repo="."

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="$2"
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

repo="$(cd "$repo" && pwd)"
compose_file="$repo/docker-compose.state-history.yml"
compose_project="dsolver-state-history"

if [[ ! -f "$compose_file" ]]; then
  echo "State history compose file not found at $compose_file." >&2
  exit 1
fi
if ! command -v docker >/dev/null 2>&1; then
  echo "Docker is required for the state history verifier." >&2
  exit 1
fi
if ! docker compose version >/dev/null 2>&1; then
  echo "Docker Compose v2 is required for the state history verifier." >&2
  exit 1
fi

cleanup() {
  local exit_status=$?
  trap - EXIT
  echo "Stopping local state history storage stack..."
  (
    cd "$repo"
    docker compose -p "$compose_project" -f "$compose_file" down -v --remove-orphans
  ) || true
  exit "$exit_status"
}
trap cleanup EXIT

export DATABASE_URL="postgres://postgres:postgres@127.0.0.1:55432/state_history"
export AWS_ACCESS_KEY_ID="minioadmin"
export AWS_SECRET_ACCESS_KEY="minioadmin"
export AWS_REGION="eu-central-1"
export AWS_DEFAULT_REGION="$AWS_REGION"
export AWS_ENDPOINT_URL_S3="http://127.0.0.1:59000"
export AWS_EC2_METADATA_DISABLED="true"
export STATE_HISTORY_S3_BUCKET="state-history-analysis"
export STATE_HISTORY_S3_PREFIX="smoke"
export STATE_HISTORY_S3_REGION="$AWS_REGION"
export STATE_HISTORY_S3_ENDPOINT_URL="$AWS_ENDPOINT_URL_S3"
export STATE_HISTORY_S3_FORCE_PATH_STYLE="true"

echo "Starting local state history storage stack..."
(
  cd "$repo"
  docker compose -p "$compose_project" -f "$compose_file" up -d --wait --wait-timeout 120 postgres minio
  docker compose -p "$compose_project" -f "$compose_file" run --rm minio-init
)

echo "Running ignored state-history integration tests..."
(
  cd "$repo"
  cargo test -p state-history -- --ignored
)

echo "Running state-history analysis harness..."
(
  cd "$repo"
  cargo run -p apps --bin state-history-analysis
)

echo "State history storage contract verification passed."
