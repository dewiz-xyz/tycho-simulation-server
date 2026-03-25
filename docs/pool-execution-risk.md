# Pool Execution Risk Analysis

> **Context:** This analysis applies to solver platforms such as **CoW Protocol**
> and **Near Intents**, where transactions are settled by trusted solvers through
> batch auctions or intent-fulfillment mechanisms — not by broadcasting raw
> transactions to the public mempool.  Sandwich attacks and traditional
> front-running are therefore **not applicable** risks in this context.

This document explains how to assess whether a pool returned by `/simulate` will
actually execute the swap on-chain close to the simulated price.

The simulation runs against a **snapshot of pool state at a specific block**.
Between that snapshot and the moment the solver's settlement transaction lands
on-chain, other transactions may move the pool.  The question is: *how much
can the pool absorb before the settlement reverts or the price falls below the
user's limit order?*

---

## The Execution Risk Problem in a Solver Context

```
Auction start            Block N   ──► pool state snapshot
                                        ↓ get_amount_out()
                                        ↓ amounts_out, slippage_bps, ...
                         (solver competition + batch construction time)
Settlement lands          Block N+k ──► actual pool state (may differ)
```

In a batch-auction or intent-based model, the gap between simulation and
settlement is dominated by:
- **Auction duration** — CoW Protocol runs ~30-second batch auctions; Near
  Intents may have variable fulfillment windows.
- **Competing solver activity** — other solvers may route through the same
  pools during the same auction window, changing on-chain state before the
  winning settlement is mined.
- **Block time variance** — settlement may not land on the next block.

None of these are sandwich attacks.  They are legitimate competing uses of
shared on-chain liquidity.

---

## Risk Signals Already in the `/simulate` Response

### 1. `slippage_bps` — Price Sensitivity

```json
"slippage_bps": [3, 30, 95]
```

Each entry is the slippage at one ladder step.  It measures how far the
effective price has already moved from the marginal spot price for that trade
size.

**What it tells you about execution risk in a solver context:**
- A pool where `slippage_bps[last] > 100` is a shallow pool relative to
  your trade.  Any on-chain activity (from other solvers, arbitrageurs, or
  ordinary users) that touches the same pool before the settlement block will
  push the price further against the user's limit order, potentially causing
  the settlement to revert.
- A pool where `slippage_bps[last] < 5` has deep liquidity.  Even if several
  competing trades consume some of the pool before settlement, your effective
  price changes only marginally.

**Thresholds (indicative):**

| `slippage_bps` (last step) | Risk level |
|---------------------------|------------|
| 0–10 | Low — deep pool, minimal sensitivity to concurrent activity |
| 10–50 | Medium — monitor utilization; suitable for most orders |
| 50–200 | High — pool state sensitive to competing solver routing |
| > 200 | Very high — prefer splitting across multiple pools |

---

### 2. `pool_utilization_bps` — Capacity Pressure

```json
"pool_utilization_bps": 8500
```

This is `(requested_max_in / pool_max_in) × 10_000`.  It expresses how close
you are to the pool's reported maximum absorbable amount.

**What it tells you about execution risk:**
- `> 10_000` means the requested amount already exceeds `get_limits()`.  The
  service ran a probe swap; if it passed, the pool is operating beyond its
  advisory limit.  High revert probability if any other trade consumes the
  pool between simulation and settlement.
- `5_000–10_000` (50%–100%) — you consume a large fraction of the pool.
  A competing solver routing through the same pool in the same auction window
  may exhaust the remaining capacity, causing the settlement to fail.
- `< 2_000` (< 20%) — well within capacity; execution is unlikely to fail
  due to pool exhaustion.

---

### 3. Slippage Acceleration — Convexity of the Bonding Curve

This is **not directly in the response** but can be computed from the ladder:

```
Δslippage[i] = slippage_bps[i] - slippage_bps[i-1]
```

For a ladder `[1e18, 5e18, 10e18]` with `slippage_bps = [3, 15, 90]`:

```
Δslippage[1] = 15 - 3  = 12 bps (for 4× the amount)
Δslippage[2] = 90 - 15 = 75 bps (for 2× the amount)
```

The acceleration `75 / 12 ≈ 6.25` is steep.  This signals a **convex bonding
curve** — the price degrades non-linearly.  In a solver context this matters
because other solvers routing a small amount through the same pool (e.g.
10% of your intended trade) can cause a disproportionately large price impact
on your settlement.

**Rule of thumb:** if `slippage_bps[last] / slippage_bps[first] > 10`, the
pool is highly sensitive to any concurrent on-chain activity before your
settlement block.

---

### 4. Partial Ladder Completion

When `amounts_out` has fewer entries than the number of amounts requested:

```json
"amounts_out": ["999000000"],   // only 1 of 3 requested steps succeeded
```

This means the pool errored or timed out on larger amounts.  The pool cannot
absorb your full intended trade.  Submitting a settlement at the largest
amount will likely revert.

**Rule:** only consider a pool reliable for amounts up to the last successfully
simulated ladder step.

---

### 5. Zero or Near-Zero Outputs

If any `amounts_out[i]` is `"0"`, the pool returned nothing for that amount.
This is a hard signal: the pool cannot fulfill the trade at that size.
Do not route through this pool for that amount.

---

### 6. Block Age — State Staleness

```json
"block_number": 19876543
```

Compare this against the current network head.  Each additional block
represents ~12 seconds of potential state drift on Ethereum mainnet.

| Block lag | Risk implication |
|-----------|-----------------|
| 0–1 blocks | Fresh; simulation is reliable |
| 2–3 blocks | Mild staleness; widen `minAmountOut` tolerance slightly |
| > 3 blocks | Stale; re-simulate before building the settlement |

In a 30-second CoW batch auction, 2–3 block lag is common by the time the
winning solver submits.  Factor this into `minAmountOut` calculations.

The service exposes `block_number` (native pools) and `vm_block_number` (VM
pools) in the response so you can compute this lag client-side.

---

### 7. Gas Consistency Across Ladder Steps

```json
"gas_used": [210000, 210000, 450000]
```

For native AMMs (Uniswap V2/V3/V4, etc.) gas should be **roughly constant**
across steps for the same pool.  A sudden jump at a specific step indicates
a tick boundary crossing (V3), a storage layout change, or an EVM anomaly in
VM pools.

If gas at the last step is significantly higher than earlier steps:
- The settlement will cost more gas than estimated.
- The solver must budget for this in the gas cost calculation feeding
  `gas_in_sell`.
- Tick crossings in V3 can also indicate the pool has limited liquidity at
  the crossed tick range, which is itself a liquidity risk signal.

For VM pools, the service internally tracks `vm_low_first_gas_count` and
`vm_low_first_gas_ratio` — an anomalously low first-step gas on VM pools is
a known signal of a potentially incorrect simulation.

---

### 8. Spot Price Availability

If `slippage_bps` entries are all `null`, the pool's `spot_price()` call
failed.  This pool has no marginal price reference — you cannot assess how
far the effective price diverges from the theoretical optimum.  Treat it as
an unknown-risk pool: route through it only if no better alternative exists,
and set a conservative `minAmountOut`.

---

## Composite Risk Score

The service computes a composite risk score for each pool and returns it in
the `execution_risk` field of every pool entry in the `/simulate` response:

```json
"execution_risk": {
  "risk_score": 312,
  "risk_level": "medium"
}
```

`risk_score` is a dimensionless integer.  `risk_level` is a qualitative label
derived from that score:

| `risk_score` | `risk_level` | Interpretation |
|-------------|--------------|---------------|
| < 200 | `low` | Deep pool; minimal sensitivity to concurrent activity |
| 200–499 | `medium` | Moderate sensitivity; suitable for most orders |
| 500–999 | `high` | Pool state sensitive to competing solver routing |
| ≥ 1 000 | `very_high` | Prefer splitting across multiple pools |
| — | `unknown` | All slippage entries are null and utilization is absent |

### Formula

```
risk_score = w1 × slippage_last_bps
           + w2 × utilization_bps
           + w3 × (slippage_last / slippage_first)    ← convexity factor
           + w5 × (1 if partial_ladder else 0) × 10_000
```

Block-lag staleness (`w4` in earlier drafts of this document) is **not**
included in the server-side score.  Clients must add that penalty separately
using `block_number` vs. the current chain head (see §6 above).

### Pool-Type-Aware Weights (`pool_type`)

The weights `w1`, `w2`, and `w3` are not fixed — they adapt to the economic
character of the pool specified by the caller via the `pool_type` request
field.

**Request field:**

```json
{
  "pool_type": "volatile"   // "volatile" | "stablecoin" | "blue_chip"
}
```

`pool_type` defaults to `"volatile"` when omitted.

**Weight table:**

| `pool_type` | `w1` (slippage) | `w2` (utilization) | `w3` (convexity) | `w5` (partial ladder) |
|-------------|-----------------|--------------------|-----------------|-----------------------|
| `volatile` *(default)* | **0.35** | **0.35** | 0.15 | 0.05 |
| `stablecoin` | 0.25 | **0.55** | 0.15 | 0.05 |
| `blue_chip` | 0.25 | **0.45** | 0.15 | 0.05 |

**Rationale for each type:**

- **`volatile`** — standard AMM (e.g. Uniswap V3 WETH/USDC, Aerodrome
  volatile pairs).  Slippage and utilization are equally important; neither
  signal dominates in isolation.

- **`stablecoin`** — correlated-asset pools (e.g. Curve 3pool, USDC/USDT
  Uniswap V3).  Slippage is expected to be very low even for large amounts
  (flat bonding curve), so slippage alone is a weak signal.  Utilization
  carries more weight because the risk of capacity exhaustion by a competing
  solver is the primary failure mode.

- **`blue_chip`** — deep, battle-tested pools with large TVL (e.g. ETH/USDC
  500 bps, WBTC/ETH on Uniswap V3).  Deep liquidity means slippage is
  structurally low.  Utilization is still more informative than slippage, but
  less so than for stablecoins where any slippage at all is already a
  meaningful anomaly.

**Choosing the right `pool_type`:**

| Pool characteristics | Suggested `pool_type` |
|----------------------|----------------------|
| Stablecoin-to-stablecoin (Curve, Balancer StableSwap) | `stablecoin` |
| Correlated pegged assets (wstETH/ETH, rETH/ETH) | `stablecoin` |
| ETH/BTC blue-chip pairs with > $10 M TVL | `blue_chip` |
| Long-tail ERC-20 pairs, new pools, meme tokens | `volatile` |
| Any pair where TVL or pool type is unknown | `volatile` *(safe default)* |

---

## Signal Summary Table

| Signal | Where it comes from | Risk it captures |
|--------|---------------------|-----------------|
| `execution_risk.risk_score` | server-computed composite | Weighted aggregate of all signals below |
| `execution_risk.risk_level` | derived from `risk_score` | Qualitative label (`low` / `medium` / `high` / `very_high` / `unknown`) |
| `slippage_bps[last]` | `compute_slippage_bps()` | Price sensitivity to pool state changes |
| `pool_utilization_bps` | `compute_pool_utilization_bps()` | Capacity exhaustion under concurrent solver activity |
| `slippage_bps[last] / slippage_bps[0]` | derived from ladder | Bonding curve convexity |
| `amounts_out.len() < requested` | partial ladder | Pool cannot absorb full trade |
| any `amounts_out[i] == "0"` | simulator output | Hard capacity limit at that step |
| `block_number` lag vs chain head | response field | State staleness during auction window |
| `gas_used` variance across steps | per-step gas | V3 tick crossings / VM anomalies |
| `slippage_bps` all null | missing spot price | Degraded or exotic pool state |
| `pool_type` (request) | caller-provided | Selects pool-type-aware weights for `risk_score` |

---

## What Cannot Be Captured by Simulation

The following risks are inherent to on-chain execution and cannot be derived
from simulation data, even in a solver context:

1. **Competing solver activity on the same pool** — if two solvers
   independently simulate the same shallow pool and both include it in their
   solutions, only the winning settlement executes.  If the losing solver's
   solution had already partially consumed the pool in an earlier block, the
   state the winning solver simulated against is stale.  High `slippage_bps`
   and high `utilization_bps` amplify this risk.

2. **Reorg risk** — the block the simulation was based on may be reorged away,
   forcing the settlement to be re-evaluated against a different state.  This
   is rare on Ethereum post-merge but not impossible.

3. **Protocol pauses / guardian interventions** — Balancer, Curve, and some
   other protocols can be paused by their multisig guardians.  No simulation
   can predict this.  Monitor protocol-level risk out-of-band.

4. **ERC-20 transfer hooks / fee-on-transfer tokens** — some tokens transfer
   less than `amount_in` due to embedded fees or hooks.  The simulator assumes
   clean ERC-20 transfers.  VM pools (Curve, Balancer) often handle this via
   internal accounting, but native AMMs (UniswapV2-style) will see a different
   effective amount arrive than simulated, causing the output to be lower than
   expected and potentially breaching `minAmountOut`.

5. **LP liquidity withdrawals between simulation and settlement** — a large LP
   removing a position during the auction window reduces available liquidity
   below `max_in`, increasing effective slippage beyond what was simulated.
   This is particularly relevant for concentrated liquidity pools (V3, V4)
   where a single LP may own a dominant position in a specific tick range.

---

## Practical Decision Flow

```
For each pool in /simulate response:
│
├─ amounts_out empty or all zero?                       → REJECT (pool cannot fill trade)
├─ partial ladder?                                      → REJECT for amounts beyond last good step
├─ execution_risk.risk_level == "very_high"?            → SKIP or split across other pools
├─ execution_risk.risk_level == "high"?                 → HIGH RISK — widen tolerance or split
├─ execution_risk.risk_level == "unknown"?              → TREAT as unknown risk; conservative tolerance
├─ slippage convexity ratio > 10?                       → HIGH SENSITIVITY — prefer deeper pool
├─ block_number lag > 3?                                → RE-SIMULATE before building settlement
├─ gas_used jumps > 2× between steps?                  → V3 tick crossing — widen gas budget
│
└─ Passed all checks → USE pool; set minAmountOut using formula below
```

> **Note:** `execution_risk.risk_score` and `risk_level` are computed
> server-side using weights appropriate for the `pool_type` you supplied in
> the request (defaulting to `volatile`).  Pass the correct `pool_type` to
> get the most accurate risk assessment for your pool class.

---

## Setting `minAmountOut` for Solver Settlements

In a solver settlement, `minAmountOut` is the on-chain guard that protects
the user's limit order price.  Setting it correctly requires accounting for
both the simulated slippage and the uncertainty introduced by the auction window.

```
auction_drift_bps  = block_lag × drift_per_block_bps    ← empirical; typically 5–15 bps/block
tolerance_bps      = slippage_bps[target_step] + auction_drift_bps + safety_margin_bps

minAmountOut = simulated_amount_out × (1 - tolerance_bps / 10_000)
```

Example for a pool with `slippage_bps[last] = 30`, 2 blocks of lag, and a 10
bps safety margin:

```
tolerance_bps = 30 + (2 × 10) + 10 = 60 bps
minAmountOut  = simulated_amount_out × 0.9940
```

**Conservative bands by pool risk level:**

| Risk level | `slippage_bps[last]` | Recommended `tolerance_bps` |
|------------|---------------------|------------------------------|
| Low | 0–10 | 20–30 |
| Medium | 10–50 | 50–80 |
| High | 50–200 | 100–200 |
| Very high | > 200 | avoid or 300+ |

---

## Relationship to `/encode`

The `/encode` endpoint re-simulates each hop internally and enforces
`expectedAmountOut >= minAmountOut` before encoding calldata.  However,
between the `/simulate` call and the `/encode` call, pool state may have
drifted further.

The risk analysis above should be applied at `/simulate` time to:
1. **Select** which pools to include in the solution.
2. **Calculate** appropriate `minAmountOut` values to pass to `/encode`.

If `/encode` rejects a route because re-simulation produces an output below
`minAmountOut`, that is the intended safety net working correctly.  The solver
should fall back to the next-best pool from the `/simulate` response rather
than relaxing the `minAmountOut` threshold.
