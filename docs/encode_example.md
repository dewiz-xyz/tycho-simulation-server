# /encode example (RouteEncodeRequest)

This document describes the target `/encode` schema.

## Terminology

- Swap: a single pool operation. It always uses one pool and one token pair.
- Hop: a single token transition. A hop can split across many pools that all do the same tokenIn to tokenOut.
- Segment: a full path from tokenIn to tokenOut. A MegaSwap contains multiple segments in parallel.

SimpleSwap uses one hop with one or more swaps where every swap is tokenA to tokenB. MultiSwap uses multiple hops with different intermediary tokens. MegaSwap runs multiple MultiSwaps in parallel, each with its own segment share.

## Workflow

- Call `/simulate` for candidate pools.
- Build a `RouteEncodeRequest` with `segments[] -> hops[] -> swaps[]`, using segment `shareBps` and swap `splitBps` to define splits.
- Provide only the top-level `amountIn` and `minAmountOut` as the route-level guard.
- POST to `/encode`, which re-simulates swaps internally, derives per-hop and per-swap amounts, and returns settlement `interactions[]`.

## Request body (shape)

```json
{
  "chainId": 1,
  "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
  "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
  "amountIn": "1000000000000000000",
  "minAmountOut": "9980000",
  "settlementAddress": "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
  "tychoRouterAddress": "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35",
  "swapKind": "MegaSwap",
  "segments": [
    {
      "kind": "MultiSwap",
      "shareBps": 6000,
      "hops": [
        {
          "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
          "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
          "swaps": [
            {
              "pool": {
                "protocol": "uniswap_v3",
                "componentId": "pool-dai-usdc-3000",
                "poolAddress": "0x1111111111111111111111111111111111111111"
              },
              "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
              "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
              "splitBps": 6000
            },
            {
              "pool": {
                "protocol": "curve_v2",
                "componentId": "pool-dai-usdc",
                "poolAddress": "0x2222222222222222222222222222222222222222"
              },
              "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
              "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
              "splitBps": 0
            }
          ]
        },
        {
          "tokenIn": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
          "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
          "swaps": [
            {
              "pool": {
                "protocol": "uniswap_v3",
                "componentId": "pool-usdc-usdt-100",
                "poolAddress": "0x3333333333333333333333333333333333333333"
              },
              "tokenIn": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
              "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
              "splitBps": 0
            }
          ]
        }
      ]
    },
    {
      "kind": "SimpleSwap",
      "shareBps": 0,
      "hops": [
        {
          "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
          "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
          "swaps": [
            {
              "pool": {
                "protocol": "uniswap_v3",
                "componentId": "pool-dai-usdt-100",
                "poolAddress": "0x4444444444444444444444444444444444444444"
              },
              "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
              "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
              "splitBps": 5000
            },
            {
              "pool": {
                "protocol": "curve_v2",
                "componentId": "pool-dai-usdt",
                "poolAddress": "0x5555555555555555555555555555555555555555"
              },
              "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
              "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
              "splitBps": 0
            }
          ]
        }
      ]
    }
  ],
  "requestId": "encode-example-split-only-1"
}
```

## Response body (shape)

```json
{
  "chainId": 1,
  "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
  "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
  "amountIn": "1000000000000000000",
  "minAmountOut": "9980000",
  "swapKind": "MegaSwap",
  "normalizedRoute": {
    "segments": [
      {
        "kind": "MultiSwap",
        "shareBps": 6000,
        "hops": [
          {
            "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
            "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            "swaps": [
              {
                "pool": {
                  "protocol": "uniswap_v3",
                  "componentId": "pool-dai-usdc-3000",
                  "poolAddress": "0x1111111111111111111111111111111111111111"
                },
                "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
                "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                "splitBps": 6000
              },
              {
                "pool": {
                  "protocol": "curve_v2",
                  "componentId": "pool-dai-usdc",
                  "poolAddress": "0x2222222222222222222222222222222222222222"
                },
                "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
                "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                "splitBps": 0
              }
            ]
          },
          {
            "tokenIn": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
            "swaps": [
              {
                "pool": {
                  "protocol": "uniswap_v3",
                  "componentId": "pool-usdc-usdt-100",
                  "poolAddress": "0x3333333333333333333333333333333333333333"
                },
                "tokenIn": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
                "splitBps": 0
              }
            ]
          }
        ]
      },
      {
        "kind": "SimpleSwap",
        "shareBps": 0,
        "hops": [
          {
            "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
            "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
            "swaps": [
              {
                "pool": {
                  "protocol": "uniswap_v3",
                  "componentId": "pool-dai-usdt-100",
                  "poolAddress": "0x4444444444444444444444444444444444444444"
                },
                "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
                "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
                "splitBps": 5000
              },
              {
                "pool": {
                  "protocol": "curve_v2",
                  "componentId": "pool-dai-usdt",
                  "poolAddress": "0x5555555555555555555555555555555555555555"
                },
                "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
                "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
                "splitBps": 0
              }
            ]
          }
        ]
      }
    ]
  },
  "interactions": [
    {
      "kind": "ERC20_APPROVE",
      "target": "0x6b175474e89094c44da98b954eedeac495271d0f",
      "value": "0",
      "calldata": "0x095ea7b3..."
    },
    {
      "kind": "CALL",
      "target": "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35",
      "value": "0",
      "calldata": "0x..."
    }
  ],
  "debug": {
    "requestId": "encode-example-split-only-1",
    "resimulation": {
      "blockNumber": 24200000
    }
  }
}
```

## Notes

- Only route level `minAmountOut` is enforced. There are no per-hop or per-swap `minAmountOut` checks.
- `shareBps` (segment-level) and `splitBps` (swap-level) use Tycho split semantics:
  - `0` means remainder, or 100% if there is only one entry.
  - For each split set, the last entry must be `0` so it receives the remainder.
  - Non-last entries must be > 0.
  - The sum of all non-remainder shares must be <= 10000.
- Hops are sequential in the order provided; there is no hop-level share.
- SimpleSwap has a single hop. All swaps in that hop must share the same tokenIn and tokenOut.
- MultiSwap has multiple hops. Each hop transitions between different tokens.
- MegaSwap has multiple segments. Each segment is a MultiSwap or SimpleSwap and gets a segment share.
- The encoder emits `singleSwap`, `sequentialSwap`, or `splitSwap` based on route shape and splits.
- `poolAddress` is optional and may be omitted when unavailable.
- `interactions[]` are emitted in order: approve(amountIn) -> router call. For reset-allowance tokens an approve(0) is prepended.
- The settlement encoding expects ERC20 `tokenIn` and `tokenOut`. Use wrapped native tokens for ETH.
