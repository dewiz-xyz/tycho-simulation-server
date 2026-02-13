# Tycho CloudWatch log queries

## Defaults
- Region: eu-central-1
- Log group: /ecs/tycho-simulator
- Log stream prefix (ECS): tycho-simulator/web/

## JSON note
Logs are JSON from `tracing_subscriber`. CloudWatch does not always flatten nested `fields`,
so these queries parse `@message` to extract `msg`, `level`, and structured fields.

## Preset index
| Preset | Focus | Notes |
| --- | --- | --- |
| block-updates | Block height progress | Matches "Block update:" messages. |
| block-updates-window | Block height progress (window) | Same as block-updates, but sorts ascending. |
| block-updates-count | Block update frequency | Counts block update logs; also reports first/last timestamp in window. |
| readiness | Service readiness | Matches "Service ready: first pools ingested". |
| resync | Resync lifecycle | Matches any "Resync" message. |
| stream-health | Stream startup and errors | Stream start, merged stream status, and stream errors. |
| stream-supervision | Stream supervision lifecycle | Restarts, stale streams, missing blocks, advanced state. |
| vm-rebuild | VM rebuild lifecycle | VM rebuild start/success and cleanup failures. |
| startup | App boot sequence | Init, token load, concurrency limits. |
| server | HTTP server lifecycle | Start/listen/bind/server errors. |
| timeouts | Request handler timeouts | Request-level and computation timeouts. |
| router-timeouts | Router boundary timeouts | Timeout layer and router errors. |
| simulate-requests | Incoming simulate calls | "Received simulate request". |
| simulate-completions | Quote completions | Success and timeout completions. |
| simulate-successes | Successful completions | `status=Ready` completions with top pool details. |
| simulate-rpm | Requests per minute | Uses completion logs (more reliable than request-start logs). |
| simulate-rpm-by-auction | Requests per minute per auction_id | Uses completion logs; can be large, bump `--limit`. |
| simulate-requests-per-auction | Total request count per auction_id | Complements simulate-rpm-by-auction (not per-minute). |
| simulate-runs | Per-request simulation runs detail | Shows simulation_runs (scheduled pools) alongside key metrics. |
| simulate-runs-per-minute | Pool simulation runs per minute | Sum of scheduled pools per minute; distinguishes runs from requests. |
| simulate-runs-per-auction | Pool simulation runs per auction_id | Sum of scheduled pools per auction; distinguishes runs from requests. |
| token-metadata | Token metadata fetch errors | Timeout and failure messages. |
| token-rpc-fetch | Single token RPC fetch | Rare fetch path and failures. |
| state-anomalies | State store warnings | Missing state, unknown protocol/pair. |
| vm-pools | VM pool feed config | Enabled/disabled logs. |
| tvl-thresholds | TVL filter config | "Using TVL thresholds". |
| uniswap-v4-filter | Hook filter | "RegisteredUniswapV4HookFilter". |
| warn-error | WARN and ERROR levels | Filters JSON level field. |
| storage-errors | Storage error incidents | Filters StorageError with pool context. |
| delta-transition | Delta transition failures | DeltaTransitionError and failed update warnings. |
| stream-update-stats | Stream update stats | Max new/removed/updated/total pairs by stream. |

## Tail/filter patterns (aws logs tail/filter-log-events)
- Block updates: Block update:
- Readiness: Service ready: first pools ingested
- Resync lifecycle: Resync
- Stream errors: Stream error OR Stream ended unexpectedly
- Stream start: Starting stream processing OR Stream update processed
- Merged stream startup: Starting merged protocol streams OR Merged protocol streams running
- HTTP server: Starting HTTP server OR Server listening OR Failed to bind to address
- Handler timeouts: Simulate request timed out OR Simulate computation completed with timeout
- Router timeouts: Request-level timeout triggered at router boundary
- Simulate request: Received simulate request
- Simulate completions: Simulate computation completed
- Token metadata: Token metadata fetch timed out OR Token metadata fetch failed
- Token RPC fetch: Token not in cache, trying to fetch from Tycho RPC
- State anomalies: missing state OR unknown protocol OR unknown pair
- VM pools: VM pool feeds enabled OR VM pool feeds disabled
- TVL thresholds: Using TVL thresholds
- Uniswap v4: RegisteredUniswapV4HookFilter
- Stream supervision: Stream stale; triggering restart OR Missing block detected OR Advanced synchronizer state detected OR Failed to build native stream OR Failed to build VM stream OR Restarting native stream OR Restarting VM stream
- VM rebuild: VM rebuild started OR VM rebuild completed OR Failed clearing TychoDB during VM rebuild
- Storage errors: StorageError
- Delta transition: DeltaTransitionError OR Failed to apply contract/balance update

## Metrics
Use `cw_metrics.zsh` for ECS ContainerInsights memory/CPU checks.

- Memory + CPU summary + series:
  `zsh skills/tycho-cloudwatch-logs/scripts/cw_metrics.zsh --since 12h --metrics memory,cpu`
- Pivot spike check:
  `zsh skills/tycho-cloudwatch-logs/scripts/cw_metrics.zsh --since 2h --pivot 2026-02-04T11:38:00Z --window 10m`

## Logs Insights queries
### block-updates
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"level":"(?<level>[^"]+)"/
| filter msg like /Block update:/
| sort @timestamp desc
| display @timestamp, level, msg, @logStream
| limit 100
```

### block-updates-window
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"level":"(?<level>[^"]+)"/
| filter msg like /Block update:/
| sort @timestamp asc
| display @timestamp, level, msg, @logStream
| limit 100
```

### block-updates-count
```
fields @timestamp
| parse @message '"message":"*"' as msg
| filter msg like /Block update:/
| stats count() as block_updates,
        min(@timestamp) as first_update,
        max(@timestamp) as last_update
```

### readiness
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Service ready:/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit 100
```

### resync
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"event":"(?<event>[^"]+)"/
| parse @message /"resync_id":(?<resync_id>[0-9]+)/
| parse @message /"duration_ms":(?<duration_ms>[0-9]+)/
| filter msg like /Resync/
| sort @timestamp desc
| display @timestamp, event, resync_id, duration_ms, msg, @logStream
| limit 100
```

### stream-health
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Starting stream processing/ or msg like /Stream update processed/ or msg like /Merged protocol streams/ or msg like /Stream error/ or msg like /Stream ended unexpectedly/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit 100
```

### stream-supervision
```
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
| limit 100
```
Note: `stream-supervision` parses structured `event`/`stream` fields when they are present; if those are missing, the message filter still matches.

### startup
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Initializing price service/ or msg like /Loaded .* tokens/ or msg like /Initialized simulation concurrency limits/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit 100
```

### server
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Starting HTTP server/ or msg like /Server listening/ or msg like /Failed to bind to address/ or msg like /Server error:/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit 100
```

### timeouts
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"request_id":"(?<request_id>[^"]+)"/
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| parse @message /"latency_ms":(?<latency_ms>[0-9]+)/
| parse @message /"timeout_ms":(?<timeout_ms>[0-9]+)/
| filter msg like /Simulate request timed out/ or msg like /Simulate computation completed with timeout/
| sort @timestamp desc
| display @timestamp, request_id, auction_id, latency_ms, timeout_ms, msg, @logStream
| limit 100
```

### router-timeouts
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Request-level timeout triggered at router boundary/ or msg like /Unhandled service error/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit 100
```

### simulate-requests
```
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
| limit 100
```

### simulate-completions
```
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
| limit 100
```

Note: failure summaries (`failure_kinds`, `failure_protocols`, `failure_pool_kinds`, `failure_samples`) are emitted with debug formatting, so they appear as JSON string fields in `@message`. The easiest way to inspect them is to include `@message` in the display or open the log event details in the CloudWatch console.

### simulate-successes
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"request_id":"(?<request_id>[^"]+)"/
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| parse @message /"token_in":"(?<token_in>[^"]+)"/
| parse @message /"token_out":"(?<token_out>[^"]+)"/
| parse @message /"amounts":(?<amounts>[0-9]+)/
| parse @message /"status":"(?<status>[^"]+)"/
| parse @message /"responses":(?<responses>[0-9]+)/
| parse @message /"latency_ms":(?<latency_ms>[0-9]+)/
| parse @message /"top_pool":"(?<top_pool>[^"]+)"/
| parse @message /"top_pool_name":"(?<top_pool_name>[^"]+)"/
| parse @message /"top_pool_address":"(?<top_pool_address>[^"]+)"/
| parse @message /"top_amount_out":"(?<top_amount_out>[^"]+)"/
| parse @message /"top_gas_used":(?<top_gas_used>[0-9]+)/
| filter msg = "Simulate computation completed" and status = "Ready"
| sort @timestamp desc
| display @timestamp, request_id, auction_id, status, responses, latency_ms, token_in, token_out, amounts, top_pool_name, top_pool_address, top_amount_out, top_gas_used, msg, @logStream
| limit 100
```

### simulate-rpm
Note: You can't `sort bin(1m) asc` directly (it parses as a function call). Alias it first and sort the alias.
```
fields @timestamp
| parse @message '"message":"*"' as msg
| filter msg like /Simulate computation completed/
| stats count() as requests by bin(1m) as minute
| sort minute asc
| limit 1000
```

### simulate-rpm-by-auction
```
fields @timestamp
| parse @message '"message":"*"' as msg
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| filter msg like /Simulate computation completed/ and auction_id != ""
| stats count() as requests by minute = bin(1m), auction_id
| sort minute asc, requests desc
| limit 10000
```

### simulate-requests-per-auction
```
fields @timestamp
| parse @message '"message":"*"' as msg
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| filter msg like /Simulate computation completed/ and auction_id != ""
| stats count() as requests by auction_id
| sort requests desc
| limit 100
```

### simulate-runs
```
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
| limit 100
```

### simulate-runs-per-minute
```
fields @timestamp
| parse @message '"message":"*"' as msg
| parse @message /"scheduled_native_pools":(?<scheduled_native_pools>[0-9]+)/
| parse @message /"scheduled_vm_pools":(?<scheduled_vm_pools>[0-9]+)/
| filter msg like /Simulate computation completed/
| fields scheduled_native_pools + scheduled_vm_pools as simulation_runs
| stats sum(simulation_runs) as total_runs, count() as requests by bin(1m) as minute
| sort minute asc
| limit 100
```

### simulate-runs-per-auction
```
fields @timestamp
| parse @message '"message":"*"' as msg
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| parse @message /"scheduled_native_pools":(?<scheduled_native_pools>[0-9]+)/
| parse @message /"scheduled_vm_pools":(?<scheduled_vm_pools>[0-9]+)/
| filter msg like /Simulate computation completed/ and auction_id != ""
| fields scheduled_native_pools + scheduled_vm_pools as simulation_runs
| stats sum(simulation_runs) as total_runs, count() as requests by auction_id
| sort total_runs desc
| limit 100
```

### token-metadata
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"request_id":"(?<request_id>[^"]+)"/
| parse @message /"auction_id":"(?<auction_id>[^"]+)"/
| parse @message /"token":"(?<token>[^"]+)"/
| parse @message /"token_address":"(?<token_address>[^"]+)"/
| filter msg like /Token metadata fetch timed out/ or msg like /Token metadata fetch failed/
| sort @timestamp desc
| display @timestamp, request_id, auction_id, token, token_address, msg, @logStream
| limit 100
```

### token-rpc-fetch
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"token_address":"(?<token_address>[^"]+)"/
| filter msg like /Token not in cache, trying to fetch from Tycho RPC/ or msg like /Token fetch request failed before response/ or msg like /Token fetch returned non-success status/ or msg like /Failed to decode token response/ or msg like /Token response contained no matching token/ or msg like /Token fetch succeeded/
| sort @timestamp desc
| display @timestamp, token_address, msg, @logStream
| limit 100
```

### vm-rebuild
```
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
| limit 100
```

### state-anomalies
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /missing state/ or msg like /unknown protocol/ or msg like /unknown pair/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit 100
```

### vm-pools
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /VM pool feeds/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit 100
```

### tvl-thresholds
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| filter msg like /Using TVL thresholds/
| sort @timestamp desc
| display @timestamp, msg, @logStream
| limit 100
```

### uniswap-v4-filter
```
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
| limit 100
```
`filter_rule=passthrough_no_filter` keeps all hooks pools enabled (`accepted_pools == considered_pools`, `filtered_pools == 0`) while exposing identifier coverage via `pools_with_hook_identifier` and `pools_missing_hook_identifier`.

### warn-error
```
fields @timestamp, @logStream
| parse @message /"level":"(?<level>[^"]+)"/
| parse @message '"message":"*"' as msg
| filter level in ["WARN", "ERROR"]
| sort @timestamp desc
| display @timestamp, level, msg, @logStream
| limit 100
```

### storage-errors
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"error":"(?<error>[^"]+)"/
| parse @message /"pool":"(?<pool>[^"]+)"/
| filter @message like /StorageError/
| sort @timestamp desc
| display @timestamp, msg, pool, error, @logStream
| limit 100
```

### delta-transition
```
fields @timestamp, @logStream
| parse @message '"message":"*"' as msg
| parse @message /"error":"(?<error>[^"]+)"/
| parse @message /"pool":"(?<pool>[^"]+)"/
| filter msg like /DeltaTransitionError/ or msg like /Failed to apply contract\/balance update/
| sort @timestamp desc
| display @timestamp, msg, pool, error, @logStream
| limit 100
```

### stream-update-stats
```
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
| limit 100
```
