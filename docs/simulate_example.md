# /simulate example (AmountOutRequest)

This document describes the `/simulate` request and response shape.

## Terminology

- Quote request: one `POST /simulate` call with one sell token, one buy token, and a ladder of input amounts.
- Laddered amounts: the ordered `amounts[]` in the request and the matching `amounts_out[]` in each pool response.
- Usable quote: a pool entry in `data[]` that returned at least one usable ladder output.
- Partial result: a quote response that returned usable data, but with incomplete ladders or incomplete pool coverage.
- Quoteable result: a response with `meta.status = "ready"` and `meta.result_quality = "complete"` or `"partial"`.

## Workflow

- POST a quote request to `/simulate`.
- Inspect `meta.status`, `meta.result_quality`, `meta.partial_kind`, `meta.failures`, and `meta.pool_results`.
- Keep only quoteable responses when selecting pools for execution or route building.
- If you need calldata, feed selected pools into `/encode`, which re-simulates the route internally.

## Request body (shape)

```json
{
  "request_id": "req-123",
  "auction_id": "auction-42",
  "token_in": "0x6b175474e89094c44da98b954eedeac495271d0f",
  "token_out": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
  "amounts": ["1000000000000000000", "5000000000000000000"]
}
```

## Response body (shape)

```json
{
  "request_id": "req-123",
  "data": [
    {
      "pool": "uniswapv3-1",
      "pool_name": "UniswapV3::DAI/USDC",
      "pool_address": "0x1111111111111111111111111111111111111111",
      "amounts_out": ["999000000", "4985000000"],
      "gas_used": [210000, 210000],
      "gas_in_sell": "630000000000000",
      "block_number": 19876543
    }
  ],
  "meta": {
    "status": "ready",
    "result_quality": "complete",
    "block_number": 19876543,
    "vm_block_number": 19876540,
    "matching_pools": 4,
    "candidate_pools": 4,
    "total_pools": 412,
    "auction_id": "auction-42"
  }
}
```

## Notes

- `auction_id` is optional in the request. When present, it is echoed back as `meta.auction_id`.
- `/simulate` uses `200 OK` for contract-valid complete, partial, no-result, and selected degraded outcomes. Clients must inspect `meta`, not just the HTTP status.
- `gas_in_sell` is a sell-token decimal string derived from the latest cached gas price, request-scoped ETH-to-sell pricing, and the pool's last `gas_used` ladder entry. It can legitimately be `"0"` when gas reporting or pricing inputs are unavailable.
- `block_number` is the native stream block. `vm_block_number` is present when VM pool support is enabled and VM state has a current block.

### Outcome interpretation

- `status = "ready"` with `result_quality = "complete"` means usable quotes were returned without request-visible degradation.
- `status = "ready"` with `result_quality = "partial"` means usable quotes were returned, but the result is incomplete.
- `status = "no_liquidity"` with `result_quality = "no_results"` means no usable quote survived because liquidity was absent or exhausted.
- `result_quality = "request_level_failure"` means the request ended in a degraded state that should not be used as a quote source, even if the response shape is valid.
- `partial_kind` appears only when `result_quality = "partial"`. It is `amount_ladders`, `pool_coverage`, or `mixed`.
- `vm_unavailable = true` means VM pools were skipped because VM state was not ready. If native pools still produced usable quotes, the response can still be quoteable.

### Interpreting `meta.failures` and `meta.pool_results`

- `meta.failures` is the request-level explanation layer. It includes timeouts, token coverage failures, no-pools reasons, and other failures that matter to the overall request outcome.
- `meta.pool_results` is the per-pool anomaly layer. It includes degraded pool-local outcomes such as `partial_output`, `zero_output`, `skipped_concurrency`, `skipped_deadline`, `timed_out`, `simulator_error`, and `internal_error`.
- A partial ready response can include both usable data in `data[]` and request-visible issues in `meta.failures` or `meta.pool_results`.

### Timeout behavior

- Request-guard timeouts return `200 OK` with a valid quote payload whose `meta.status = "ready"`, `meta.result_quality = "request_level_failure"`, and `meta.failures` includes a `timeout`.
- Router-boundary timeouts for `/simulate` also return `200 OK` with `result_quality = "request_level_failure"`.
- Timeout responses are structurally valid, but they are not quoteable.
