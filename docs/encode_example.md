# /encode example (RouteEncodeRequest)

This document shows the **latest** `/encode` schema. The values are representative examples (shape-focused), not a verbatim capture.

## How this was built

- Call `/simulate` for each hop to pick candidate pools.
- Build a `RouteEncodeRequest` with `segments[] → hops[] → swaps[]`, including `amountIn` and `minAmountOut` per pool swap.
- POST to `/encode`, which re-simulates each pool swap, verifies `expectedAmountOut >= minAmountOut`, and fills expected amounts.

## Request body (shape)

```json
{
  "chainId": 1,
  "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
  "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
  "amountIn": "1000000000000000000",
  "settlementAddress": "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
  "tychoRouterAddress": "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35",
  "swapKind": "MultiSwap",
  "segments": [
    {
      "kind": "MultiSwap",
      "shareBps": 10000,
      "hops": [
        {
          "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
          "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
          "swaps": [
            {
              "pool": {
                "protocol": "uniswap_v3",
                "componentId": "pool-dai-usdc"
              },
              "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
              "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
              "splitBps": 10000,
              "amountIn": "1000000000000000000",
              "minAmountOut": "1000250"
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
                "componentId": "pool-usdc-usdt"
              },
              "tokenIn": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
              "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
              "splitBps": 10000,
              "amountIn": "1000250",
              "minAmountOut": "9974710"
            }
          ]
        }
      ]
    }
  ],
  "requestId": "encode-example-1"
}
```

## Response body (shape)

```json
{
  "schemaVersion": "latest",
  "chainId": 1,
  "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
  "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
  "amountIn": "1000000000000000000",
  "swapKind": "MultiSwap",
  "normalizedRoute": {
    "segments": [
      {
        "kind": "MultiSwap",
        "shareBps": 10000,
        "amountIn": "1000000000000000000",
        "expectedAmountOut": "9999709",
        "minAmountOut": "9974710",
        "hops": [
          {
            "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
            "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            "amountIn": "1000000000000000000",
            "expectedAmountOut": "1000500",
            "minAmountOut": "1000250",
            "swaps": [
              {
                "pool": {
                  "protocol": "uniswap_v3",
                  "componentId": "pool-dai-usdc"
                },
                "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
                "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                "splitBps": 10000,
                "amountIn": "1000000000000000000",
                "expectedAmountOut": "1000500",
                "minAmountOut": "1000250"
              }
            ]
          },
          {
            "tokenIn": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
            "amountIn": "1000500",
            "expectedAmountOut": "9999709",
            "minAmountOut": "9974710",
            "swaps": [
              {
                "pool": {
                  "protocol": "uniswap_v3",
                  "componentId": "pool-usdc-usdt"
                },
                "tokenIn": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
                "splitBps": 10000,
                "amountIn": "1000500",
                "expectedAmountOut": "9999709",
                "minAmountOut": "9974710"
              }
            ]
          }
        ]
      }
    ]
  },
  "calls": [
    {
      "index": 0,
      "target": "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35",
      "value": "0",
      "calldata": "0x...",
      "approvals": [
        {
          "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
          "spender": "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35",
          "amount": "1000000000000000000"
        }
      ],
      "kind": "TYCHO_SINGLE_SWAP",
      "hopPath": "segment[0].hop[0]",
      "pool": {
        "protocol": "uniswap_v3",
        "componentId": "pool-dai-usdc"
      },
      "tokenIn": "0x6b175474e89094c44da98b954eedeac495271d0f",
      "tokenOut": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
      "amountIn": "1000000000000000000",
      "minAmountOut": "1000250",
      "expectedAmountOut": "1000500"
    },
    {
      "index": 1,
      "target": "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35",
      "value": "0",
      "calldata": "0x...",
      "approvals": [
        {
          "token": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
          "spender": "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35",
          "amount": "1000500"
        }
      ],
      "kind": "TYCHO_SINGLE_SWAP",
      "hopPath": "segment[0].hop[1]",
      "pool": {
        "protocol": "uniswap_v3",
        "componentId": "pool-usdc-usdt"
      },
      "tokenIn": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
      "tokenOut": "0xdac17f958d2ee523a2206206994597c13d831ec7",
      "amountIn": "1000500",
      "minAmountOut": "9974710",
      "expectedAmountOut": "9999709"
    }
  ],
  "calldata": {
    "target": "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35",
    "value": "0",
    "data": "0x..."
  },
  "totals": {
    "expectedAmountOut": "9999709",
    "minAmountOut": "9974710"
  },
  "debug": {
    "requestId": "encode-example-1",
    "resimulation": {
      "blockNumber": 24200000
    }
  }
}
```

## Notes

- `calls[]` are emitted in **segment → hop → swap** order.
- Each call is a Tycho `singleSwap` with per-swap approvals for ERC20 inputs.
- `calldata` is a single Tycho router transaction for the full route (ready to send).
- `minAmountOut` is supplied by the client and enforced to be non-zero for all swaps.
- `/encode` fails if any resimulated `expectedAmountOut < minAmountOut`.
