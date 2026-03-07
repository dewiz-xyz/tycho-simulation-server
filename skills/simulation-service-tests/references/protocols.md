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
- `ekubo_v3`

### Base (`CHAIN_ID=8453`)
- `uniswap_v2`
- `uniswap_v3`
- `uniswap_v4`
- `pancakeswap_v3`

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
- `scripts/run_suite.sh` and `wait_ready.sh` now key VM assertions off runtime `/status.vm_enabled`.

## Notes that affect test coverage

- Always pass chain context to scripts (`--chain-id` or env `CHAIN_ID`).
- To test native-only behavior on Ethereum:
  - `scripts/run_suite.sh --repo . --chain-id 1 --suite core --disable-vm-pools --stop`
- Or start with VM disabled:
  - `zsh skills/simulation-service-tests/scripts/start_server.zsh --repo /path/to/tycho-simulation-server --chain-id 1 --env ENABLE_VM_POOLS=false`
- TVL filtering matters: pools are included/removed based on `TVL_THRESHOLD` + `TVL_KEEP_RATIO`.
- Protocol labels in `coverage_sweep.py` are derived from `pool_name` prefixes (best effort).
- Chain-aware protocol assertions are recommended:
  - Ethereum VM gate example: `--expect-protocols maverick_v2`
  - Base currently has no default VM protocol gate.
