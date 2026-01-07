# /encode example

This is a real request/response captured from a live local run. It uses two identical multi-hop routes to keep the example small but still show multiple steps.

## How this was built

- Called `/simulate` for DAI -> USDC (3 amounts) and took the first pool’s `pool`, `amounts_out`, `gas_used`, and `block_number`.
- Called `/simulate` for USDC -> USDT using the previous `amounts_out` to get hop 2.
- Built two routes with the same hops and sent them to `/encode` in `tycho_router_transfer_from` mode.

## Request body

```json
{
  "request_id": "encode-example-0c44449a",
  "token_in": "0x6b175474e89094c44da98b954eedeac495271d0f",
  "token_out": "0xdac17f958d2ee523a2206206994597c13d831ec7",
  "calldata": {
    "mode": "tycho_router_transfer_from",
    "sender": "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
    "receiver": "0x9008D19f58AAbD9eD0D60971565AA8510560ab41"
  },
  "routes": [
    {
      "route_id": "route-multi-a",
      "hops": [
        {
          "pool_id": "0x5777d92f208679db4b9778590fa3cab3ac9e2168",
          "token_in": "0x6b175474e89094c44da98b954eedeac495271d0f",
          "token_out": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
        },
        {
          "pool_id": "0x8aa4e11cbdf30eedc92100f4c8a31ff748e201d44712cc8c90d189edaa8e4e47",
          "token_in": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
          "token_out": "0xdac17f958d2ee523a2206206994597c13d831ec7"
        }
      ],
      "amounts": [
        "1000000000000000000",
        "5000000000000000000",
        "10000000000000000000"
      ],
      "amounts_out": ["999970", "4999854", "9999709"],
      "gas_used": [132000, 132000, 132000],
      "block_number": 24176578
    },
    {
      "route_id": "route-multi-b",
      "hops": [
        {
          "pool_id": "0x5777d92f208679db4b9778590fa3cab3ac9e2168",
          "token_in": "0x6b175474e89094c44da98b954eedeac495271d0f",
          "token_out": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
        },
        {
          "pool_id": "0x8aa4e11cbdf30eedc92100f4c8a31ff748e201d44712cc8c90d189edaa8e4e47",
          "token_in": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
          "token_out": "0xdac17f958d2ee523a2206206994597c13d831ec7"
        }
      ],
      "amounts": [
        "1000000000000000000",
        "5000000000000000000",
        "10000000000000000000"
      ],
      "amounts_out": ["999970", "4999854", "9999709"],
      "gas_used": [132000, 132000, 132000],
      "block_number": 24176578
    }
  ]
}
```

## Response body

```json
{
  "request_id": "encode-example-0c44449a",
  "data": [
    {
      "route_id": "route-multi-a",
      "amounts_out": ["999970", "4999854", "9999709"],
      "gas_used": [132000, 132000, 132000],
      "block_number": 24176578,
      "calldata": {
        "mode": "tycho_router_transfer_from",
        "sender": "0x9008d19f58aabd9ed0d60971565aa8510560ab41",
        "receiver": "0x9008d19f58aabd9ed0d60971565aa8510560ab41",
        "steps": [
          {
            "calls": [
              {
                "call_type": "tycho_router",
                "target": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                "value": "0",
                "calldata": "0xe21dd0d30000000000000000000000000000000000000000000000000de0b6b3a76400000000000000000000000000006b175474e89094c44da98b954eedeac495271d0f000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec700000000000000000000000000000000000000000000000000000000000f3e3a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000009008d19f58aabd9ed0d60971565aa8510560ab410000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000012000000000000000000000000000000000000000000000000000000000000000ef0069bab7124c9662b15c6b9af0b1f329907dd55a24fc6b175474e89094c44da98b954eedeac495271d0fa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48000064fd0b31d2e955fa55e3fa641fe90e08b677188d355777d92f208679db4b9778590fa3cab3ac9e216801000082e49b916032c734cd89cdfe80a868805c738a6ceba0b86991c6218b36c1d19d4a2e9eb0ce3606eb48dac17f958d2ee523a2206206994597c13d831ec701019008d19f58aabd9ed0d60971565aa8510560ab41dac17f958d2ee523a2206206994597c13d831ec700000a000001000000000000000000000000000000000000000000000000000000000000000000000000000000",
                "allowances": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "spender": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                    "amount": "1000000000000000000"
                  }
                ],
                "inputs": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "amount": "1000000000000000000"
                  }
                ],
                "outputs": [
                  {
                    "token": "0xdac17f958d2ee523a2206206994597c13d831ec7",
                    "amount": "999970"
                  }
                ]
              }
            ]
          },
          {
            "calls": [
              {
                "call_type": "tycho_router",
                "target": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                "value": "0",
                "calldata": "0xe21dd0d30000000000000000000000000000000000000000000000004563918244f400000000000000000000000000006b175474e89094c44da98b954eedeac495271d0f000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec700000000000000000000000000000000000000000000000000000000004c3726000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000009008d19f58aabd9ed0d60971565aa8510560ab410000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000012000000000000000000000000000000000000000000000000000000000000000ef0069bab7124c9662b15c6b9af0b1f329907dd55a24fc6b175474e89094c44da98b954eedeac495271d0fa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48000064fd0b31d2e955fa55e3fa641fe90e08b677188d355777d92f208679db4b9778590fa3cab3ac9e216801000082e49b916032c734cd89cdfe80a868805c738a6ceba0b86991c6218b36c1d19d4a2e9eb0ce3606eb48dac17f958d2ee523a2206206994597c13d831ec701019008d19f58aabd9ed0d60971565aa8510560ab41dac17f958d2ee523a2206206994597c13d831ec700000a000001000000000000000000000000000000000000000000000000000000000000000000000000000000",
                "allowances": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "spender": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                    "amount": "5000000000000000000"
                  }
                ],
                "inputs": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "amount": "5000000000000000000"
                  }
                ],
                "outputs": [
                  {
                    "token": "0xdac17f958d2ee523a2206206994597c13d831ec7",
                    "amount": "4999854"
                  }
                ]
              }
            ]
          },
          {
            "calls": [
              {
                "call_type": "tycho_router",
                "target": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                "value": "0",
                "calldata": "0xe21dd0d30000000000000000000000000000000000000000000000008ac7230489e800000000000000000000000000006b175474e89094c44da98b954eedeac495271d0f000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec70000000000000000000000000000000000000000000000000000000000986e4d000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000009008d19f58aabd9ed0d60971565aa8510560ab410000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000012000000000000000000000000000000000000000000000000000000000000000ef0069bab7124c9662b15c6b9af0b1f329907dd55a24fc6b175474e89094c44da98b954eedeac495271d0fa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48000064fd0b31d2e955fa55e3fa641fe90e08b677188d355777d92f208679db4b9778590fa3cab3ac9e216801000082e49b916032c734cd89cdfe80a868805c738a6ceba0b86991c6218b36c1d19d4a2e9eb0ce3606eb48dac17f958d2ee523a2206206994597c13d831ec701019008d19f58aabd9ed0d60971565aa8510560ab41dac17f958d2ee523a2206206994597c13d831ec700000a000001000000000000000000000000000000000000000000000000000000000000000000000000000000",
                "allowances": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "spender": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                    "amount": "10000000000000000000"
                  }
                ],
                "inputs": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "amount": "10000000000000000000"
                  }
                ],
                "outputs": [
                  {
                    "token": "0xdac17f958d2ee523a2206206994597c13d831ec7",
                    "amount": "9999709"
                  }
                ]
              }
            ]
          }
        ]
      }
    },
    {
      "route_id": "route-multi-b",
      "amounts_out": ["999970", "4999854", "9999709"],
      "gas_used": [132000, 132000, 132000],
      "block_number": 24176578,
      "calldata": {
        "mode": "tycho_router_transfer_from",
        "sender": "0x9008d19f58aabd9ed0d60971565aa8510560ab41",
        "receiver": "0x9008d19f58aabd9ed0d60971565aa8510560ab41",
        "steps": [
          {
            "calls": [
              {
                "call_type": "tycho_router",
                "target": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                "value": "0",
                "calldata": "0xe21dd0d30000000000000000000000000000000000000000000000000de0b6b3a76400000000000000000000000000006b175474e89094c44da98b954eedeac495271d0f000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec700000000000000000000000000000000000000000000000000000000000f3e3a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000009008d19f58aabd9ed0d60971565aa8510560ab410000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000012000000000000000000000000000000000000000000000000000000000000000ef0069bab7124c9662b15c6b9af0b1f329907dd55a24fc6b175474e89094c44da98b954eedeac495271d0fa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48000064fd0b31d2e955fa55e3fa641fe90e08b677188d355777d92f208679db4b9778590fa3cab3ac9e216801000082e49b916032c734cd89cdfe80a868805c738a6ceba0b86991c6218b36c1d19d4a2e9eb0ce3606eb48dac17f958d2ee523a2206206994597c13d831ec701019008d19f58aabd9ed0d60971565aa8510560ab41dac17f958d2ee523a2206206994597c13d831ec700000a000001000000000000000000000000000000000000000000000000000000000000000000000000000000",
                "allowances": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "spender": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                    "amount": "1000000000000000000"
                  }
                ],
                "inputs": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "amount": "1000000000000000000"
                  }
                ],
                "outputs": [
                  {
                    "token": "0xdac17f958d2ee523a2206206994597c13d831ec7",
                    "amount": "999970"
                  }
                ]
              }
            ]
          },
          {
            "calls": [
              {
                "call_type": "tycho_router",
                "target": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                "value": "0",
                "calldata": "0xe21dd0d30000000000000000000000000000000000000000000000004563918244f400000000000000000000000000006b175474e89094c44da98b954eedeac495271d0f000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec700000000000000000000000000000000000000000000000000000000004c3726000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000009008d19f58aabd9ed0d60971565aa8510560ab410000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000012000000000000000000000000000000000000000000000000000000000000000ef0069bab7124c9662b15c6b9af0b1f329907dd55a24fc6b175474e89094c44da98b954eedeac495271d0fa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48000064fd0b31d2e955fa55e3fa641fe90e08b677188d355777d92f208679db4b9778590fa3cab3ac9e216801000082e49b916032c734cd89cdfe80a868805c738a6ceba0b86991c6218b36c1d19d4a2e9eb0ce3606eb48dac17f958d2ee523a2206206994597c13d831ec701019008d19f58aabd9ed0d60971565aa8510560ab41dac17f958d2ee523a2206206994597c13d831ec700000a000001000000000000000000000000000000000000000000000000000000000000000000000000000000",
                "allowances": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "spender": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                    "amount": "5000000000000000000"
                  }
                ],
                "inputs": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "amount": "5000000000000000000"
                  }
                ],
                "outputs": [
                  {
                    "token": "0xdac17f958d2ee523a2206206994597c13d831ec7",
                    "amount": "4999854"
                  }
                ]
              }
            ]
          },
          {
            "calls": [
              {
                "call_type": "tycho_router",
                "target": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                "value": "0",
                "calldata": "0xe21dd0d30000000000000000000000000000000000000000000000008ac7230489e800000000000000000000000000006b175474e89094c44da98b954eedeac495271d0f000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec70000000000000000000000000000000000000000000000000000000000986e4d000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000009008d19f58aabd9ed0d60971565aa8510560ab410000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000012000000000000000000000000000000000000000000000000000000000000000ef0069bab7124c9662b15c6b9af0b1f329907dd55a24fc6b175474e89094c44da98b954eedeac495271d0fa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48000064fd0b31d2e955fa55e3fa641fe90e08b677188d355777d92f208679db4b9778590fa3cab3ac9e216801000082e49b916032c734cd89cdfe80a868805c738a6ceba0b86991c6218b36c1d19d4a2e9eb0ce3606eb48dac17f958d2ee523a2206206994597c13d831ec701019008d19f58aabd9ed0d60971565aa8510560ab41dac17f958d2ee523a2206206994597c13d831ec700000a000001000000000000000000000000000000000000000000000000000000000000000000000000000000",
                "allowances": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "spender": "0xfd0b31d2e955fa55e3fa641fe90e08b677188d35",
                    "amount": "10000000000000000000"
                  }
                ],
                "inputs": [
                  {
                    "token": "0x6b175474e89094c44da98b954eedeac495271d0f",
                    "amount": "10000000000000000000"
                  }
                ],
                "outputs": [
                  {
                    "token": "0xdac17f958d2ee523a2206206994597c13d831ec7",
                    "amount": "9999709"
                  }
                ]
              }
            ]
          }
        ]
      }
    }
  ],
  "meta": {
    "status": "ready"
  }
}
```

## Notes for solver clients

- **Pool IDs**: use `simulate.data[].pool` verbatim as `routes[].hops[].pool_id` (don’t normalize or re-encode).
- **Big numbers**: amounts are decimal strings; parse as bigints (not `u64`).
- **Hex formatting**: `target`, `calldata`, `sender`, `receiver` are `0x`-prefixed hex strings; treat addresses case-insensitively.
- **Steps vs amounts**: `steps.len()` matches `amounts.len()`; each step corresponds to one ladder amount.
- **Call types**:
  - `tycho_router_transfer_from`: one call per step (`call_type = tycho_router`) with `allowances`, `inputs`, `outputs`.
  - `tycho_router_none`: two calls per step (`erc20_transfer` then `tycho_router`), no allowances on the router call.
- **Failure semantics**: `meta.status` can be `ready`, `partial_failure`, or `invalid_request`.
  - For `partial_failure`, some routes may omit `calldata` while others succeed.
  - Response order always matches request order.
- **Defaults**: if `calldata.sender`/`receiver` are omitted, the server falls back to `COW_SETTLEMENT_CONTRACT` (env).
