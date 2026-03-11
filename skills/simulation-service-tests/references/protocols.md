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
- `ekubo_v3`
- `fluid_v1`
- `rocketpool`

## Optional (VM feeds)
- `vm:curve` (enabled when `ENABLE_VM_POOLS=true`, default)
- `vm:balancer_v2` (enabled when `ENABLE_VM_POOLS=true`, default)
- `vm:maverick_v2` (enabled when `ENABLE_VM_POOLS=true`, default)

## Notes that affect test coverage

- **VM pools are on by default**. Disable them if you want to validate native-only behavior:
  - `scripts/run_suite.sh --repo . --suite core --disable-vm-pools --stop`
  - Or start the server with `ENABLE_VM_POOLS=false`:
    - `zsh skills/simulation-service-tests/scripts/start_server.zsh --repo /path/to/tycho-simulation-server --env ENABLE_VM_POOLS=false`
- **Default repo suite runs are VM-aware** when VM pools stay enabled.
  - `scripts/run_suite.sh --repo . --suite core --stop` waits for `vm_status=ready` and at least one VM pool before the Maverick/Balancer presence checks run.
  - There is no supported repo-runner mode that keeps VM pools enabled while skipping that wait.
- **Core suite coverage** keeps `ETH:RETH` and `RETH:ETH` for Rocketpool/native paths.
  Maverick coverage is checked separately with the protocol presence probe in `scripts/run_suite.sh`, which is more stable than relying on `GHO:USDC`.
  Balancer coverage is also checked separately with a capped `WETH:USDC` probe.
- **Exploratory coverage** lives in the `exploratory_protocols` suite.
  Use it for pairs like `GHO:USDC`, `CBETH:WETH`, and `SUSHI:WETH` that are useful for visibility but too live-state-sensitive for the default green path.
- **TVL filtering matters**: pools are included/removed based on `TVL_THRESHOLD` + `TVL_KEEP_RATIO`.
  - If your tests miss protocols/pools, try lowering `TVL_THRESHOLD` (at the cost of ingesting more pools).
- **Uniswap v4 pairs**: some v4 pools may use native ETH (often represented as `0x000...000`).
  - The `v4_candidates` suite includes `ETH:USDC` alongside `WETH:USDC`.
- **Protocol names in reports**: `coverage_sweep.py` prefers the explicit `protocol` field and falls back to `pool_name` parsing when needed.
  - Winner-path protocols come from successful `data` entries only.
  - Candidate-path protocols include successful pools plus `meta.pool_results`, so skipped/limited pools still show up in presence checks.
  - `--expect-protocols` validates against candidate-path protocols.
