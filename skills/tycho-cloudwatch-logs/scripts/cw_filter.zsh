#!/usr/bin/env zsh
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
source "$script_dir/cw_env.zsh"

log_group="$TYCHO_LOG_GROUP"
since="15m"
until="now"
limit=50
filter_pattern=""
stream_prefix=""

usage() {
  cat <<'USAGE'
Usage: cw_filter.zsh [--since 15m] [--until now] [--limit 50]
                    [--filter-pattern "..."] [--stream-prefix "..."]
                    [--log-group "/ecs/tycho-simulator"]

Time specs: 15m, 2h, 1d, 2026-02-02T10:00:00Z, epoch seconds.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --since)
      since="$2"
      shift 2
      ;;
    --until)
      until="$2"
      shift 2
      ;;
    --limit)
      limit="$2"
      shift 2
      ;;
    --filter-pattern|--filter)
      filter_pattern="$2"
      shift 2
      ;;
    --stream-prefix)
      stream_prefix="$2"
      shift 2
      ;;
    --log-group)
      log_group="$2"
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

start_ms=$(python3 "$script_dir/cw_time.py" "$since" --ms)
end_ms=$(python3 "$script_dir/cw_time.py" "$until" --ms)

cmd=(aws logs filter-log-events \
  --log-group-name "$log_group" \
  --start-time "$start_ms" \
  --end-time "$end_ms" \
  --interleaved \
  --limit "$limit" \
  --no-cli-pager)

[[ -n "$filter_pattern" ]] && cmd+=(--filter-pattern "$filter_pattern")
[[ -n "$stream_prefix" ]] && cmd+=(--log-stream-name-prefix "$stream_prefix")

"${cmd[@]}"
