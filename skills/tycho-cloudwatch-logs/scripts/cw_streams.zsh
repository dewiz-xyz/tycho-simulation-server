#!/usr/bin/env zsh
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
source "$script_dir/cw_env.zsh"

log_group="$TYCHO_LOG_GROUP"
limit=25
prefix=""
output="table"

usage() {
  cat <<'USAGE'
Usage: cw_streams.zsh [--limit 25] [--prefix "tycho-simulator/web/"]
                     [--log-group "/ecs/tycho-simulator"] [--output table|json|text]
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --limit)
      limit="$2"
      shift 2
      ;;
    --prefix)
      prefix="$2"
      shift 2
      ;;
    --log-group)
      log_group="$2"
      shift 2
      ;;
    --output)
      output="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

cmd=(aws logs describe-log-streams \
  --log-group-name "$log_group" \
  --order-by LastEventTime \
  --descending \
  --limit "$limit" \
  --no-cli-pager)

[[ -n "$prefix" ]] && cmd+=(--log-stream-name-prefix "$prefix")

"${cmd[@]}" --query 'logStreams[].{name:logStreamName,lastEvent:lastEventTimestamp,storedBytes:storedBytes}' --output "$output"
