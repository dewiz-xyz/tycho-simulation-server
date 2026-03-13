# Tycho Simulation Library — Analysis & Reference

Source: [https://docs.propellerheads.xyz/tycho/for-solvers/simulation](https://docs.propellerheads.xyz/tycho/for-solvers/simulation)

---

## Overview

`tycho-simulation` is a Rust crate by Propeller Heads that enables developers to interact with DeFi protocol states, calculate spot prices, and simulate token swaps. It is the core simulation engine consumed by this service.

### Cargo Setup

```toml
tycho-simulation = {
    git = "https://github.com/propeller-heads/tycho-simulation.git",
    package = "tycho-simulation",
    tag = "x.y.z",        # use latest release tag
    features = ["evm"]    # required for EVM-backed protocols (Curve, Balancer, Maverick)
}
```

---

## Core Trait: `ProtocolSim`

Every supported protocol implements `ProtocolSim`. This is the abstraction this service relies on to treat native and VM pools uniformly at the simulation layer.

### Method Reference

| Method | Signature | Returns | Notes |
|--------|-----------|---------|-------|
| `spot_price` | `spot_price(&self, base: &Token, quote: &Token)` | `Result<f64, SimulationError>` | Marginal price at current pool state |
| `get_amount_out` | `get_amount_out(amount_in, token_in, token_out)` | `GetAmountOutResult` | Core swap simulation |
| `fee` | `fee()` | decimal ratio | Dynamic fees return minimum; e.g. `0.003` for 0.3% |
| `get_limits` | `get_limits()` | `(max_in, max_out)` | Soft limits for unrestricted pools (e.g. Uniswap V2) |
| `swap_to_price` | `swap_to_price(price: Price)` | amount needed | Price expressed as rational fraction |
| `query_supply` | `query_supply()` | max output | Maximum token_out while respecting minimum trade price |

### Key Structs

```rust
/// Result of a swap simulation
struct GetAmountOutResult {
    amount: BigUint,                  // tokens received
    gas: BigUint,                     // estimated gas units
    new_state: Box<dyn ProtocolSim>,  // updated pool state after the swap
}

/// Rational price representation
struct Price {
    numerator: BigUint,
    denominator: BigUint,
}

/// Token trade pair
struct Trade {
    amount_in: BigUint,
    amount_out: BigUint,
}
```

---

## Streaming Protocol States (`ProtocolStreamBuilder`)

The library provides a real-time state streaming mechanism backed by the Tycho Indexer RPC.

### Setup Pattern

```rust
// 1. Load token metadata
let all_tokens = load_all_tokens(
    "tycho-beta.propellerheads.xyz",
    false,                   // use SSL
    Some("your-api-token"),
    Chain::Ethereum,
    None,                    // min quality filter
    None,                    // days since last trade
).await;

// 2. Build the stream
let mut protocol_stream = ProtocolStreamBuilder::new("tycho-beta.propellerheads.xyz", Chain::Ethereum)
    .exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None)
    .exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None)
    // ... add more protocols
    .auth_key(Some("your-api-token"))
    .skip_state_decode_failures(true)
    .set_tokens(all_tokens.clone())
    .await
    .build()
    .await;
```

### `Update` Message Structure

Each message emitted by the stream contains:

| Field | Type | Description |
|-------|------|-------------|
| `block_number_or_timestamp` | block ref | The block this update corresponds to |
| `new_pairs` | components | Pools newly tracked (entered TVL filter) |
| `removed_pairs` | components | Pools no longer meeting filter criteria |
| `states` | `ProtocolSim` instances | Updated states for modified components only |

**Important**: The **first** message is a full snapshot of all pools. Subsequent messages carry **only modified** components.

### TVL Filtering

```rust
ComponentFilter::with_tvl_range(min_eth, max_eth)
// Example: pools with 9–10 ETH TVL buffer
ComponentFilter::with_tvl_range(9, 10)
```

---

## How This Service Uses the Library

### Mapping to Service Architecture

| Library Concept | Service Usage |
|-----------------|---------------|
| `ProtocolSim::get_amount_out` | Core of `get_amounts_out()` in [services/quotes.rs](../src/services/quotes.rs) |
| `GetAmountOutResult::new_state` | Chained for laddered amounts (price impact simulation across ladder) |
| `GetAmountOutResult::gas` | Converted to sell-token units for `gas_in_sell` response field |
| `ProtocolStreamBuilder` | Wrapped in `build_native_stream()` / `build_vm_stream()` in [services/stream_builder.rs](../src/services/stream_builder.rs) |
| `load_all_tokens()` | Called at startup in [main.rs](../src/main.rs) to populate `TokenStore` |
| `skip_state_decode_failures(true)` | Mirrored by `decode_skip_state_failures()` always returning `true` in [handlers/stream.rs](../src/handlers/stream.rs) |
| `Update::new_pairs` / `removed_pairs` | Processed in `process_stream()` to update `StateStore` |

### Simulation Flow (per pool, per amount)

```
amount_in[0] ──► get_amount_out() ──► GetAmountOutResult { amount, gas, new_state }
                                                                        │
amount_in[1] ──► new_state.get_amount_out() ──► ...                    │
                      ▲                                                  │
                      └── uses updated pool state (price impact) ◄──────┘
```

This chaining using `new_state` is what enables accurate per-amount slippage calculation — each successive amount in the ladder simulates against the post-previous-swap pool state, capturing the true price curve.

### Native vs VM Split

| Stream Type | Protocols | State Type | Timeout |
|-------------|-----------|------------|---------|
| Native | uniswap_v2/v3/v4, pancakeswap, sushiswap, ekubo, fluid | Pure Rust `ProtocolSim` impl | 20ms |
| VM | curve, balancer_v2, maverick_v2 | `EVMPoolState<PreCachedDB>` | 150ms |

VM pools require the `evm` feature flag and use `SHARED_TYCHO_DB` for EVM state.

---

## Notable Behaviors

- **`get_limits()`** returns "soft" limits for unrestricted pools (UniswapV2-style AMMs) — the service does not currently enforce these as hard caps on input amounts.
- **`fee()`** returns the minimum for dynamic-fee pools — actual fee may differ mid-swap.
- **`query_supply()`** is not currently used by this service but could inform upper-bound ladder clamping.
- **`swap_to_price()`** is not currently used but could enable target-price quote modes.
- The `new_state` in `GetAmountOutResult` is heap-allocated (`Box<dyn ProtocolSim>`) — cloning for multiple ladder branches would require additional allocation.

---

## Partial Blocks (Base Chain)

The library supports sub-second latency on Base via pre-confirmation streaming:

```rust
builder.enable_partial_blocks()
```

This is not currently enabled in this service (Ethereum mainnet focus).
