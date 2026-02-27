# /encode testing notes

## Schema (latest)
- `/encode` uses `RouteEncodeRequest` / `RouteEncodeResponse` with camelCase fields.
- Execution is **singleSwap-only** (no splitSwap or sequentialSwap in v1).
- Requests include only route-level `amountIn` and `minAmountOut` plus `shareBps`/`splitBps` for splits.
- Per-hop and per-swap amounts are not accepted and not returned.

## Smoke test helper

`scripts/encode_smoke.py`:
- Resolves chain from `--chain-id` (or env `CHAIN_ID`).
- Calls `/simulate` for each hop to pick candidate pools.
- Builds a 2-hop `MultiSwap` request and posts to `/encode`.
- Verifies interactions shape, approvals, and router calldata.
- Uses chain-specific route defaults from `scripts/presets.py` (`default_encode_route`).

Examples:
```bash
python3 scripts/encode_smoke.py --chain-id 1 --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .
python3 scripts/encode_smoke.py --chain-id 8453 --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .
```

## Common pitfalls

- `settlementAddress` and `tychoRouterAddress` are required. Defaults come from `.env`:
  - `COW_SETTLEMENT_CONTRACT`
  - `TYCHO_ROUTER_ADDRESS`
- `/encode` fails if the resimulated route `expectedAmountOut < minAmountOut`.
- Timeout behavior differs from `/simulate`: `/encode` returns `408` with `{ error, requestId }`.
- Chain mismatch between request `chainId` and server runtime chain fails validation.

## Reference docs

- `docs/encode_example.md` for schema shape examples.
