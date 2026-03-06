# Slippage Calculation Strategy

This document explains how the tycho-simulation-server calculates slippage for each pool
and each trade amount. It is written for developers who are still learning how
liquidity pools and automated market makers (AMMs) work.

---

## What is slippage?

Imagine you walk into a foreign-exchange booth and ask for the rate on a
small amount — say, $10. The cashier quotes you 1 EUR per dollar.
Now you ask for the rate on $10,000. Because the booth only has a limited
supply of euros, buying that many dollars' worth pushes the price against you.
You might get only 0.95 EUR per dollar instead of 1.

That difference — the gap between the "perfect small-trade price" and the
"actual price you get for your real trade size" — is slippage.

In DeFi, **every AMM pool has a finite amount of tokens on each side**.
The bigger your trade relative to the pool's reserves, the worse the rate you receive.

---

## The three pieces of information we have

For every pool that participates in a quote, the server has:

| Data | Where it comes from | What it means |
|---|---|---|
| `amounts_in[i]` | The trade request | Exactly how much token_in you want to sell at ladder step `i` |
| `amounts_out[i]` | `pool_state.get_amount_out()` | How much token_out the pool will actually give back |
| `max_in` | `pool_state.get_limits()` | The maximum token_in this pool can safely absorb |

We also fetch one more piece from each pool:

| Data | Where it comes from | What it means |
|---|---|---|
| `spot_price` | `pool_state.spot_price()` | The marginal rate — the price you would get for an infinitesimally tiny trade |

---

## The spot price is the zero-slippage reference

`spot_price` is what mathematicians call the "instantaneous rate of exchange" at
the current pool state.  Think of it as the price on a price-tag that ignores
trade size entirely.

For a Uniswap V2 pool that holds reserves `R_in` of token_in and `R_out` of
token_out, the spot price is simply:

```
spot_price = R_out / R_in
```

For a Uniswap V3 or Curve pool the formula is more complex, but the concept is
the same: it is the slope of the bonding curve at the current point, before
any trade moves the curve.

Any real trade that moves the curve will get a *worse* effective price than the spot price.
The amount by which it is worse is the slippage.

---

## Step-by-step slippage calculation

### Step 1 — Convert the spot price to base units

Tokens have different decimal precisions (e.g. DAI has 18 decimals, USDC has 6).
`spot_price` is returned by the simulator in "human-readable" units (like
"1.00 USDC per DAI"), but our amounts are in raw base units (wei-equivalent integers).

We correct for this with:

```
spot_price_base = spot_price_human × 10^(decimals_out) / 10^(decimals_in)
```

For the DAI → USDC example:
```
spot_price_base = 1.00 × 10^6 / 10^18 = 1e-12
```

This tells us: for every 1 wei of DAI in, the pool would give 1e-12 USDC-wei at
perfect marginal conditions.

### Step 2 — Compute the effective price for the actual trade

```
effective_price = amounts_out[i] / amounts_in[i]
```

Both numbers are already in base units (they come directly from the simulator),
so no conversion is needed here.

### Step 3 — Calculate slippage as a fraction

```
slippage_fraction = 1 - (effective_price / spot_price_base)
```

- If `effective_price == spot_price_base`: slippage = 0 % (perfect fill)
- If `effective_price < spot_price_base`: slippage > 0 % (you received less than the marginal rate)
- If `effective_price > spot_price_base`: slippage < 0 % (you received *more* than marginal — rare, possible with rebate mechanisms)

### Step 4 — Express it in basis points

Financial systems typically express small percentages in **basis points** (bps),
where 1 bps = 0.01 % and 100 bps = 1 %.

```
slippage_bps = round(slippage_fraction × 10_000)
```

---

## Pool utilization

`pool_utilization_bps` tells you what fraction of the pool's capacity your
trade is consuming:

```
utilization = (requested_max_in / max_in) × 10_000  (in bps)
```

A utilization of 10_000 bps means you are trying to put in exactly as much as
the pool's reported maximum. A utilization of 500 bps means you are only using
5 % of the pool's capacity.

High utilization almost always means high slippage: you are pushing the pool's
reserves significantly, which moves the price curve against you.

---

## Where these values appear in the API response

`POST /simulate` now includes two extra fields in each pool's result:

```json
{
  "pool": "uniswap_v3-1",
  "pool_name": "UniswapV3::DAI/USDC",
  "pool_address": "0x...",
  "amounts_out": ["999000000", "4985000000"],
  "gas_used": [210000, 210000],
  "gas_in_sell": "630000000000000",
  "block_number": 19876543,
  "slippage_bps": [3, 30],
  "pool_utilization_bps": 420
}
```

- `slippage_bps[0]` = 3 bps (0.03 %) for the first (smaller) amount
- `slippage_bps[1]` = 30 bps (0.30 %) for the second (larger) amount
- `pool_utilization_bps` = 420 bps (4.2 % of pool capacity)

Notice how slippage rises as trade size grows — this is the core AMM mechanic in action.

---

## When slippage is unavailable

`slippage_bps` entries are `null` (JSON null) when:

- The pool's `spot_price()` call fails or returns an invalid value (NaN, negative, zero).
  This can happen with some EVM-backed (VM) pools.
- The trade amount at that step is zero.
- Arithmetic overflow occurs in the calculation.

`pool_utilization_bps` is absent when:

- The pool's `get_limits()` call fails or returns zero.

These are treated as "best-effort" fields — their absence does not affect the
correctness of `amounts_out`.

---

## Why the ladder matters

The request sends a *ladder* of amounts (e.g. `[1e18, 5e18, 10e18]`).
Each step is simulated independently against the same pool state.
This is intentional: slippage is not linear.

Doubling the trade size does not double the slippage; the degradation curve is
convex (steeper for large trades). Having multiple data points at once lets the
solver pick the most capital-efficient split across pools.

---

## Summary

```
spot_price_base  = pool.spot_price() × 10^(dec_out - dec_in)   ← zero-slippage reference
effective_price  = amount_out / amount_in                        ← real trade rate
slippage_bps     = (1 - effective_price / spot_price_base) × 10_000
utilization_bps  = (max_requested_amount_in / pool_max_in) × 10_000
```

Both numbers are per-pool and per-step, giving the solver a complete picture of
where liquidity is deep (low slippage, low utilization) and where it is shallow
(high slippage, high utilization).
