# Protocol feeds (what this server subscribes to)

The service subscribes to specific Tycho exchanges at startup (see `src/services/stream_builder.rs`).

## Always enabled (native feeds)
- `uniswap_v2`
- `sushiswap_v2`
- `pancakeswap_v2`
- `uniswap_v3`
- `pancakeswap_v3`
- `uniswap_v4`
- `ekubo_v2`

## Optional (VM feeds)
- `vm:curve` (enabled when `ENABLE_VM_POOLS=true`, default)
- `vm:balancer_v2` (enabled when `ENABLE_VM_POOLS=true`, default)

## Notes that affect test coverage

- **VM pools are on by default**. Disable them if you want to validate native-only behavior:
  - `scripts/run_suite.sh --repo . --suite core --disable-vm-pools --stop`
  - Or start the server with `ENABLE_VM_POOLS=false`:
    - `zsh skills/simulation-service-tests/scripts/start_server.zsh --repo /path/to/tycho-simulation-server --env ENABLE_VM_POOLS=false`
- **TVL filtering matters**: pools are included/removed based on `TVL_THRESHOLD` + `TVL_KEEP_RATIO`.
  - If your tests miss protocols/pools, try lowering `TVL_THRESHOLD` (at the cost of ingesting more pools).
- **Uniswap v4 pairs**: some v4 pools may use native ETH (often represented as `0x000...000`).
  - The `v4_candidates` suite includes `ETH:USDC` alongside `WETH:USDC`.
- **Protocol names in reports**: `coverage_sweep.py` derives “protocol” from `pool_name` (the prefix before `::`).
  - Treat it as best-effort labeling; if upstream changes formatting, update `protocol_from_pool_name`.
  - When using `--expect-protocols`, use the lowercased `pool_name` prefixes from the sweep output, not request protocol IDs like `uniswap_v3`.
