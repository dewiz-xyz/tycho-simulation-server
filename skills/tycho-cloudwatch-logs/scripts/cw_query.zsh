#!/usr/bin/env zsh
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
source "$script_dir/cw_env.zsh"

log_group="$TYCHO_LOG_GROUP"
since="30m"
until="now"
limit=100
query=""
query_file=""
preset=""

usage() {
  cat <<'USAGE'
Usage: cw_query.zsh [--since 30m] [--until now] [--limit 100]
                   [--query "..."] [--file query.cwl] [--preset name]
                   [--log-group "/ecs/tycho-simulator"]

Presets: block-updates, block-updates-window, block-updates-count,
         readiness, resync, stream-health, stream-supervision,
         vm-rebuild, startup, server, timeouts, router-timeouts,
         simulate-requests, simulate-completions, simulate-successes, token-metadata,
         token-rpc-fetch, state-anomalies, vm-pools, tvl-thresholds,
         simulate-rpm, simulate-rpm-by-auction,
         simulate-requests-per-auction, simulate-runs, simulate-runs-per-minute,
         simulate-runs-per-auction,
         uniswap-v4-filter, warn-error, storage-errors, delta-transition,
         stream-update-stats
USAGE
}

preset_query() {
  case "$1" in
    block-updates)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"level":"(?<level>[^"]+)"/
| filter msg like /Block update:/
| sort @timestamp desc
| display @timestamp, level, msg, @logStream
| limit ${limit}
QUERY
      ;;
    block-updates-window)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"level":"(?<level>[^"]+)"/
| filter msg like /Block update:/
| sort @timestamp asc
| display @timestamp, level, msg, @logStream
| limit ${limit}
QUERY
      ;;
    block-updates-count)
      cat <<QUERY
fields @timestamp
| parse @message '"message":"*"' as msg
| filter msg like /Block update:/
| stats count() as block_updates,
        min(@timestamp) as first_update,
        max(@timestamp) as last_update
QUERY
      ;;
    readiness)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Service ready:/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit ${limit}
QUERY
      ;;
    resync)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"event":"(?<event>[^"]+)"/
| parse @message /"resync_id":(?<resync_id>[0-9]+)/
| parse @message /"duration_ms":(?<duration_ms>[0-9]+)/
| filter msg like /Resync/
| sort @timestamp desc
| display @timestamp, event, resync_id, duration_ms, msg, @logStream
| limit ${limit}
QUERY
      ;;
    stream-health)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Starting stream processing/ or msg like /Stream update processed/ or msg like /Merged protocol streams/ or msg like /Stream error/ or msg like /Stream ended unexpectedly/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit ${limit}
QUERY
      ;;
    stream-supervision)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"event":"(?<event>[^"]+)"/
| parse @message /"stream":"(?<stream>[^"]+)"/
| parse @message /"reason":"(?<reason>[^"]+)"/
| parse @message /"restart_count":(?<restart_count>[0-9]+)/
| parse @message /"backoff_ms":(?<backoff_ms>[0-9]+)/
| parse @message /"last_block":(?<last_block>[0-9]+)/
| parse @message /"last_update_age_ms":(?<last_update_age_ms>[0-9]+)/
| parse @message /"error":"(?<error>[^"]+)"/
| filter msg like /Stream stale; triggering restart/
  or msg like /Stream ended unexpectedly/
  or msg like /Stream error/
  or msg like /Missing block detected/
  or msg like /Advanced synchronizer state detected/
  or msg like /Failed to build native stream/
  or msg like /Failed to build VM stream/
  or msg like /Restarting native stream/
  or msg like /Restarting VM stream/
| sort @timestamp desc
| display @timestamp, event, stream, reason, restart_count, backoff_ms, last_block, last_update_age_ms, error, msg, @logStream
| limit ${limit}
QUERY
      ;;
    startup)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Initializing price service/ or msg like /Loaded .* tokens/ or msg like /Initialized simulation concurrency limits/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit ${limit}
QUERY
      ;;
    server)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Starting HTTP server/ or msg like /Server listening/ or msg like /Failed to bind to address/ or msg like /Server error:/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit ${limit}
QUERY
      ;;
    timeouts)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"request_id":"(?<request_id>[^"]+)"/
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| parse @message /"latency_ms":(?<latency_ms>[0-9]+)/
| parse @message /"timeout_ms":(?<timeout_ms>[0-9]+)/
| filter msg like /Simulate request timed out/ or msg like /Simulate computation completed with timeout/
| sort @timestamp desc
| display @timestamp, request_id, auction_id, latency_ms, timeout_ms, msg, @logStream
| limit ${limit}
QUERY
      ;;
    router-timeouts)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Request-level timeout triggered at router boundary/ or msg like /Unhandled service error/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit ${limit}
QUERY
      ;;
    simulate-requests)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"request_id":"(?<request_id>[^"]+)"/
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| parse @message /"token_in":"(?<token_in>[^"]+)"/
| parse @message /"token_out":"(?<token_out>[^"]+)"/
| parse @message /"amounts":(?<amounts>[0-9]+)/
| filter msg like /Received simulate request/
| sort @timestamp desc
| display @timestamp, request_id, auction_id, token_in, token_out, amounts, msg, @logStream
| limit ${limit}
QUERY
      ;;
    simulate-completions)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"request_id":"(?<request_id>[^"]+)"/
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| parse @message /"token_in":"(?<token_in>[^"]+)"/
| parse @message /"token_out":"(?<token_out>[^"]+)"/
| parse @message /"amounts":(?<amounts>[0-9]+)/
| parse @message /"status":"(?<status>[^"]+)"/
| parse @message /"responses":(?<responses>[0-9]+)/
| parse @message /"failures":(?<failures>[0-9]+)/
| parse @message /"latency_ms":(?<latency_ms>[0-9]+)/
| parse @message /"scheduled_native_pools":(?<scheduled_native_pools>[0-9]+)/
| parse @message /"scheduled_vm_pools":(?<scheduled_vm_pools>[0-9]+)/
| parse @message /"skipped_vm_unavailable":(?<skipped_vm_unavailable>[0-9]+)/
| parse @message /"skipped_native_concurrency":(?<skipped_native_concurrency>[0-9]+)/
| parse @message /"skipped_vm_concurrency":(?<skipped_vm_concurrency>[0-9]+)/
| parse @message /"skipped_native_deadline":(?<skipped_native_deadline>[0-9]+)/
| parse @message /"skipped_vm_deadline":(?<skipped_vm_deadline>[0-9]+)/
| parse @message /"top_pool":"(?<top_pool>[^"]+)"/
| parse @message /"top_pool_name":"(?<top_pool_name>[^"]+)"/
| parse @message /"top_pool_address":"(?<top_pool_address>[^"]+)"/
| parse @message /"top_amount_out":"(?<top_amount_out>[^"]+)"/
| parse @message /"top_gas_used":(?<top_gas_used>[0-9]+)/
| filter msg like /Simulate computation completed/
| sort @timestamp desc
| display @timestamp, request_id, auction_id, status, responses, failures, latency_ms, token_in, token_out, amounts, top_pool_name, top_pool_address, top_amount_out, top_gas_used, scheduled_native_pools, scheduled_vm_pools, skipped_vm_unavailable, skipped_native_concurrency, skipped_vm_concurrency, skipped_native_deadline, skipped_vm_deadline, msg, @logStream
| limit ${limit}
QUERY
      ;;
    simulate-successes)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"request_id":"(?<request_id>[^"]+)"/
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| parse @message /"token_in":"(?<token_in>[^"]+)"/
| parse @message /"token_out":"(?<token_out>[^"]+)"/
| parse @message /"amounts":(?<amounts>[0-9]+)/
| parse @message /"status":"(?<status>[^"]+)"/
| parse @message /"responses":(?<responses>[0-9]+)/
| parse @message /"failures":(?<failures>[0-9]+)/
| parse @message /"latency_ms":(?<latency_ms>[0-9]+)/
| parse @message /"top_pool":"(?<top_pool>[^"]+)"/
| parse @message /"top_pool_name":"(?<top_pool_name>[^"]+)"/
| parse @message /"top_pool_address":"(?<top_pool_address>[^"]+)"/
| parse @message /"top_amount_out":"(?<top_amount_out>[^"]+)"/
| parse @message /"top_gas_used":(?<top_gas_used>[0-9]+)/
| filter msg = "Simulate computation completed" and status = "Ready"
| sort @timestamp desc
| display @timestamp, request_id, auction_id, status, responses, latency_ms, token_in, token_out, amounts, top_pool_name, top_pool_address, top_amount_out, top_gas_used, msg, @logStream
| limit ${limit}
QUERY
      ;;
    simulate-rpm)
      cat <<QUERY
fields @timestamp
| parse @message '"message":"*"' as msg
| filter msg like /Simulate computation completed/
| stats count() as requests by bin(1m) as minute
| sort minute asc
| limit ${limit}
QUERY
      ;;
    simulate-rpm-by-auction)
      cat <<QUERY
fields @timestamp
| parse @message '"message":"*"' as msg
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| filter msg like /Simulate computation completed/ and auction_id != ""
| stats count() as requests by minute = bin(1m), auction_id
| sort minute asc, requests desc
| limit ${limit}
QUERY
      ;;
    token-metadata)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"request_id":"(?<request_id>[^"]+)"/
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| parse @message /"token":"(?<token>[^"]+)"/
| parse @message /"token_address":"(?<token_address>[^"]+)"/
| filter msg like /Token metadata fetch timed out/ or msg like /Token metadata fetch failed/
| sort @timestamp desc
| display @timestamp, request_id, auction_id, token, token_address, msg, @logStream
| limit ${limit}
QUERY
      ;;
    token-rpc-fetch)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"token_address":"(?<token_address>[^"]+)"/
| filter msg like /Token not in cache, trying to fetch from Tycho RPC/ or msg like /Token fetch request failed before response/ or msg like /Token fetch returned non-success status/ or msg like /Failed to decode token response/ or msg like /Token response contained no matching token/ or msg like /Token fetch succeeded/
| sort @timestamp desc
| display @timestamp, token_address, msg, @logStream
| limit ${limit}
QUERY
      ;;
    state-anomalies)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /missing state/ or msg like /unknown protocol/ or msg like /unknown pair/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit ${limit}
QUERY
      ;;
    vm-pools)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /VM pool feeds/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit ${limit}
QUERY
      ;;
    tvl-thresholds)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Using TVL thresholds/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit ${limit}
QUERY
      ;;
    uniswap-v4-filter)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"protocol":"(?<protocol>[^"]+)"/
| parse @message /"filter_rule":"(?<filter_rule>[^"]+)"/
| parse @message /"considered_pools":(?<considered_pools>[0-9]+)/
| parse @message /"accepted_pools":(?<accepted_pools>[0-9]+)/
| parse @message /"filtered_pools":(?<filtered_pools>[0-9]+)/
| parse @message /"pools_with_hook_identifier":(?<pools_with_hook_identifier>[0-9]+)/
| parse @message /"pools_missing_hook_identifier":(?<pools_missing_hook_identifier>[0-9]+)/
| filter msg like /RegisteredUniswapV4HookFilter/
| sort @timestamp desc
| display @timestamp, protocol, filter_rule, considered_pools, accepted_pools, filtered_pools, pools_with_hook_identifier, pools_missing_hook_identifier, msg, @logStream
| limit ${limit}
QUERY
      ;;
    vm-rebuild)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"event":"(?<event>[^"]+)"/
| parse @message /"rebuild_id":(?<rebuild_id>[0-9]+)/
| parse @message /"duration_ms":(?<duration_ms>[0-9]+)/
| parse @message /"error":"(?<error>[^"]+)"/
| filter msg like /VM rebuild started/
  or msg like /VM rebuild completed/
  or msg like /Failed clearing TychoDB during VM rebuild/
| sort @timestamp desc
| display @timestamp, event, rebuild_id, duration_ms, error, msg, @logStream
| limit ${limit}
QUERY
      ;;
    warn-error)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message /"level":"(?<level>[^"]+)"/
| parse @message '"message":"*"' as msg
| filter level in ["WARN", "ERROR"]
| sort @timestamp desc
| display @timestamp, level, msg, @logStream
| limit ${limit}
QUERY
      ;;
    storage-errors)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"error":"(?<error>[^"]+)"/
| parse @message /"pool":"(?<pool>[^"]+)"/
| filter @message like /StorageError/
| sort @timestamp desc
| display @timestamp, msg, pool, error, @logStream
| limit ${limit}
QUERY
      ;;
    delta-transition)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"error":"(?<error>[^"]+)"/
| parse @message /"pool":"(?<pool>[^"]+)"/
| filter msg like /DeltaTransitionError/ or msg like /Failed to apply contract\\/balance update/
| sort @timestamp desc
| display @timestamp, msg, pool, error, @logStream
| limit ${limit}
QUERY
      ;;
    stream-update-stats)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"new_pairs":(?<new_pairs>[0-9]+)/
| parse @message /"removed_pairs":(?<removed_pairs>[0-9]+)/
| parse @message /"updated_states":(?<updated_states>[0-9]+)/
| parse @message /"total_pairs":(?<total_pairs>[0-9]+)/
| filter msg = "Stream update processed"
| stats max(new_pairs) as max_new_pairs,
        max(removed_pairs) as max_removed_pairs,
        max(updated_states) as max_updated_states,
        max(total_pairs) as max_total_pairs
  by @logStream
| sort max_new_pairs desc
| limit ${limit}
QUERY
      ;;
    simulate-requests-per-auction)
      cat <<QUERY
fields @timestamp
| parse @message '"message":"*"' as msg
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| filter msg like /Simulate computation completed/ and auction_id != ""
| stats count() as requests by auction_id
| sort requests desc
| limit ${limit}
QUERY
      ;;
    simulate-runs)
      cat <<QUERY
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"request_id":"(?<request_id>[^"]+)"/
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| parse @message /"status":"(?<status>[^"]+)"/
| parse @message /"responses":(?<responses>[0-9]+)/
| parse @message /"failures":(?<failures>[0-9]+)/
| parse @message /"latency_ms":(?<latency_ms>[0-9]+)/
| parse @message /"scheduled_native_pools":(?<scheduled_native_pools>[0-9]+)/
| parse @message /"scheduled_vm_pools":(?<scheduled_vm_pools>[0-9]+)/
| parse @message /"token_in":"(?<token_in>[^"]+)"/
| parse @message /"token_out":"(?<token_out>[^"]+)"/
| filter msg like /Simulate computation completed/
| fields scheduled_native_pools + scheduled_vm_pools as simulation_runs
| sort @timestamp desc
| display @timestamp, request_id, auction_id, status, simulation_runs, scheduled_native_pools, scheduled_vm_pools, responses, failures, latency_ms, token_in, token_out, @logStream
| limit ${limit}
QUERY
      ;;
    simulate-runs-per-minute)
      cat <<QUERY
fields @timestamp
| parse @message '"message":"*"' as msg
| parse @message /"scheduled_native_pools":(?<scheduled_native_pools>[0-9]+)/
| parse @message /"scheduled_vm_pools":(?<scheduled_vm_pools>[0-9]+)/
| filter msg like /Simulate computation completed/
| fields scheduled_native_pools + scheduled_vm_pools as simulation_runs
| stats sum(simulation_runs) as total_runs, count() as requests by bin(1m) as minute
| sort minute asc
| limit ${limit}
QUERY
      ;;
    simulate-runs-per-auction)
      cat <<QUERY
fields @timestamp
| parse @message '"message":"*"' as msg
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| parse @message /"scheduled_native_pools":(?<scheduled_native_pools>[0-9]+)/
| parse @message /"scheduled_vm_pools":(?<scheduled_vm_pools>[0-9]+)/
| filter msg like /Simulate computation completed/ and auction_id != ""
| fields scheduled_native_pools + scheduled_vm_pools as simulation_runs
| stats sum(simulation_runs) as total_runs, count() as requests by auction_id
| sort total_runs desc
| limit ${limit}
QUERY
      ;;
    *)
      echo "Unknown preset: $1" >&2
      exit 2
      ;;
  esac
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
    --query)
      query="$2"
      shift 2
      ;;
    --file)
      query_file="$2"
      shift 2
      ;;
    --preset)
      preset="$2"
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

if [[ -n "$preset" ]]; then
  query="$(preset_query "$preset")"
elif [[ -n "$query_file" ]]; then
  query="$(cat "$query_file")"
fi

if [[ -z "$query" ]]; then
  echo "Provide --query, --file, or --preset" >&2
  usage
  exit 2
fi

start_s=$(python3 "$script_dir/cw_time.py" "$since")
end_s=$(python3 "$script_dir/cw_time.py" "$until")

query_id=$(aws logs start-query \
  --log-group-name "$log_group" \
  --start-time "$start_s" \
  --end-time "$end_s" \
  --query-string "$query" \
  --limit "$limit" \
  --query queryId \
  --output text \
  --no-cli-pager)

sleep 1

while true; do
  query_status=$(aws logs get-query-results --query-id "$query_id" --query status --output text --no-cli-pager)
  case "$query_status" in
    Complete|Failed|Timeout|Cancelled|Unknown)
      break
      ;;
  esac
  sleep 1
done

aws logs get-query-results --query-id "$query_id" --no-cli-pager

if [[ "$query_status" != "Complete" ]]; then
  echo "Query finished with status: $query_status" >&2
  exit 1
fi
