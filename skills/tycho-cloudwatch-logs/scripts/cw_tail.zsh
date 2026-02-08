#!/usr/bin/env zsh
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
source "$script_dir/cw_env.zsh"

log_group="$TYCHO_LOG_GROUP"
since="10m"
format="json"
follow=false
filter_pattern=""
stream_prefix=""

usage() {
  cat <<'USAGE'
Usage: cw_tail.zsh [--since 10m] [--follow] [--format short|detailed|json]
                  [--filter-pattern "..."] [--stream-prefix "..."]
                  [--log-group "/ecs/tycho-simulator"]
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --since)
      since="$2"
      shift 2
      ;;
    --follow)
      follow=true
      shift
      ;;
    --format)
      format="$2"
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

cmd=(aws logs tail "$log_group" --since "$since" --format "$format" --no-cli-pager)
$follow && cmd+=(--follow)
[[ -n "$filter_pattern" ]] && cmd+=(--filter-pattern "$filter_pattern")
[[ -n "$stream_prefix" ]] && cmd+=(--log-stream-name-prefix "$stream_prefix")

"${cmd[@]}"
