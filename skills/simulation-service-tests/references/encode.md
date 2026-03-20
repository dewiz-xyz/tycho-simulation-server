# /encode testing notes

## Schema (latest)
- `/encode` uses `RouteEncodeRequest` / `RouteEncodeResponse` with camelCase fields.
- The encoder emits `singleSwap`, `sequentialSwap`, or `splitSwap` depending on route shape and splits.
- Requests include only route-level `amountIn` and `minAmountOut` plus `shareBps`/`splitBps` for splits.
- Per-hop and per-swap amounts are not accepted and not returned.
- `/encode` keeps its current success/error response contract. `/simulate` selection semantics are documented in [docs/simulate_example.md](../../../docs/simulate_example.md).

## Smoke test helper

`scripts/encode_smoke.py`:
- Resolves chain from `--chain-id` (or env `CHAIN_ID`).
- Calls `/simulate` for each hop to pick candidate pools.
- Builds a 2-hop `MultiSwap` request and posts to `/encode`.
- Verifies interactions shape, approvals, and router calldata.
- Uses chain-specific route defaults from `scripts/presets.py` (`default_encode_route`).
- Uses dedicated realistic encode amount presets from `scripts/presets.py` for those default routes instead of the generic `/simulate` amount presets.
- Treats any hop output of `"0"` as "no usable quote for that requested amount" and requires usable quotes for every requested amount on both hops.

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
- Timeout behavior differs from `/simulate`: `/encode` returns `408` with `{ error }`, plus `requestId` when it is available.
- Chain mismatch between request `chainId` and server runtime chain fails validation.
- `/simulate` rows with fully-zero `amounts_out` are not usable encode candidates. More generally, `"0"` means that requested amount did not produce a usable quote, not a valid quote.

## Reference docs

- `docs/encode_example.md` for schema shape examples.
- `docs/simulate_example.md` for `/simulate` usability rules used during pool selection.
