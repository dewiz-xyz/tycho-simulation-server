# Protocol feeds (what this server subscribes to)

The service subscribes to chain-specific Tycho exchanges at startup (see `src/config/mod.rs` and `src/services/stream_builder.rs`).

## Native feeds by chain

### Ethereum (`CHAIN_ID=1`)
- `uniswap_v2`
- `sushiswap_v2`
- `pancakeswap_v2`
- `uniswap_v3`
- `pancakeswap_v3`
- `uniswap_v4`
- `ekubo_v2`
- `fluid_v1`
- `rocketpool`
- `erc4626`
- `ekubo_v3`

### Base (`CHAIN_ID=8453`)
- `uniswap_v2`
- `uniswap_v3`
- `uniswap_v4`
- `pancakeswap_v3`
- `aerodrome_slipstreams`

## VM feeds by chain

### Ethereum (`CHAIN_ID=1`)
- `vm:curve`
- `vm:balancer_v2`
- `vm:maverick_v2`

### Base (`CHAIN_ID=8453`)
- No VM protocols configured in this iteration.

## Effective VM enablement

- Runtime VM state is `effective_vm_enabled = ENABLE_VM_POOLS && vm_protocols_not_empty`.
- This means Base reports `vm_enabled=false` even if `ENABLE_VM_POOLS=true`.
- `scripts/run_suite.sh` waits for VM readiness automatically on Ethereum when VM pools are enabled, and then uses runtime `/status.vm_enabled` for the protocol probes and latency tuning.

## Notes that affect test coverage

- Always pass chain context to scripts (`--chain-id` or env `CHAIN_ID`).
- To test native-only behavior on Ethereum:
  - `scripts/run_suite.sh --repo . --chain-id 1 --suite core --disable-vm-pools --stop`
- Or start with VM disabled:
  - `zsh skills/simulation-service-tests/scripts/start_server.zsh --repo /path/to/tycho-simulation-server --chain-id 1 --env ENABLE_VM_POOLS=false`
- Default repo suite runs are VM-aware when VM pools stay enabled on Ethereum.
  - `scripts/run_suite.sh --repo . --chain-id 1 --suite core --stop` waits for `vm_status=ready` and at least one VM pool before the Maverick/Balancer presence checks run.
  - There is no supported repo-runner mode that keeps VM pools enabled while skipping that wait.
- Core Ethereum suite coverage keeps `ETH:RETH` and `RETH:ETH` for Rocketpool/native paths.
  Maverick coverage is checked separately with the protocol presence probe in `scripts/run_suite.sh`, which is more stable than relying on `GHO:USDC`.
  Balancer coverage is also checked separately with a capped `WETH:USDC` probe.
- ERC4626 support is server-gated to the `erc4626_allowlisted` suite only:
  - `USDS:SUSDS`
  - `SUSDS:USDS`
  - `USDC:SUSDC`
  - `SUSDC:USDC`
  - `PYUSD:SPPYUSD`
  - `SPPYUSD:PYUSD`
  Use `python3 scripts/coverage_sweep.py --chain-id 1 --suite erc4626_allowlisted --expect-protocols erc4626`.
- Negative ERC4626 probe: `SUSDE -> USDE` should remain unsupported for now.
  Use `python3 scripts/coverage_sweep.py --chain-id 1 --suite erc4626_negative --allow-no-pools` when verifying the gate.
- Exploratory coverage lives in the `exploratory_protocols` suite.
  Use it for pairs like `GHO:USDC`, `CBETH:WETH`, and `SUSHI:WETH` that are useful for visibility but too live-state-sensitive for the default green path.
- TVL filtering matters: pools are included/removed based on `TVL_THRESHOLD` + `TVL_KEEP_RATIO`.
  - If your tests miss protocols/pools, try lowering `TVL_THRESHOLD` (at the cost of ingesting more pools).
- Uniswap v4 pairs: some v4 pools may use native ETH (often represented as `0x000...000`).
  - The `v4_candidates` suite includes `ETH:USDC` alongside `WETH:USDC`.
- Protocol labels in `coverage_sweep.py` prefer the explicit `protocol` field and fall back to `pool_name` parsing when needed.
  - Winner-path protocols come from successful `data` entries only.
  - Candidate-path protocols include successful pools plus `meta.pool_results`, so skipped/limited pools still show up in presence checks.
  - `--expect-protocols` validates against candidate-path protocols.
