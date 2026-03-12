#!/usr/bin/env python3
"""Smoke test helper for POST /encode (RouteEncodeRequest spec)."""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import uuid
from pathlib import Path
from urllib import request
from urllib.error import HTTPError, URLError

from presets import (
    amounts_for_pair,
    chain_label,
    default_encode_route,
    resolve_chain_id,
    resolve_token,
)

DEFAULT_SETTLEMENT_BY_CHAIN = {
    1: "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
    8453: "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
}
DEFAULT_TYCHO_ROUTER_BY_CHAIN = {
    1: "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35",
    8453: "0xea3207778e39EB02D72C9D3c4Eac7E224ac5d369",
}
DEFAULT_SLIPPAGE_BPS = 25


def is_int_string(value: object) -> bool:
    return isinstance(value, str) and value.isdigit()


def request_json(url: str, payload: dict, timeout: float) -> tuple[int, dict, float]:
    data = json.dumps(payload).encode("utf-8")
    req = request.Request(url, data=data, headers={"Content-Type": "application/json"})
    start = time.perf_counter()
    with request.urlopen(req, timeout=timeout) as response:
        body = response.read()
        elapsed = time.perf_counter() - start
        return response.status, json.loads(body), elapsed


def load_env_value(repo: Path, key: str) -> str | None:
    env_value = os.environ.get(key)
    if env_value:
        return env_value.strip()
    env_path = repo / ".env"
    if not env_path.exists():
        return None
    try:
        for line in env_path.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            name, value = line.split("=", 1)
            name = name.strip()
            if name.startswith("export "):
                name = name.removeprefix("export ").strip()
            if name == key:
                value = value.strip()
                if value.startswith('"') and value.endswith('"'):
                    value = value[1:-1]
                elif value.startswith("'") and value.endswith("'"):
                    value = value[1:-1]
                return value
    except OSError:
        return None
    return None


def default_contract_address(
    chain_id: int, defaults_by_chain: dict[int, str], label: str
) -> str:
    address = defaults_by_chain.get(chain_id)
    if address is None:
        raise ValueError(f"No default {label} configured for chain {chain_id}")
    return address


def resolve_contract_address(
    repo: Path, env_key: str, chain_id: int, defaults_by_chain: dict[int, str], label: str
) -> str:
    return load_env_value(repo, env_key) or default_contract_address(
        chain_id, defaults_by_chain, label
    )


def assert_hex(value: str, label: str) -> None:
    if not isinstance(value, str) or not value.startswith("0x"):
        raise AssertionError(f"{label} must be 0x-prefixed hex")
    if len(value) % 2 != 0:
        raise AssertionError(f"{label} must be even-length hex")


def apply_slippage(amount: str | int, bps: int) -> str:
    value = int(amount)
    safe_bps = 10_000 - bps
    return str((value * safe_bps) // 10_000)


def select_pool(response: dict, label: str) -> dict:
    data = response.get("data")
    if not isinstance(data, list) or not data:
        raise AssertionError(f"{label}: no pool data")
    pool = data[0]
    required = ["pool", "amounts_out", "gas_used", "gas_in_sell", "block_number", "pool_name", "pool_address"]
    for key in required:
        if key not in pool:
            raise AssertionError(f"{label}: missing {key}")
    if not is_int_string(pool.get("gas_in_sell")):
        raise AssertionError(f'{label}: gas_in_sell must be an integer string ("0" is valid)')
    return pool


def protocol_from_pool_name(pool_name: str) -> str:
    if "::" in pool_name:
        return pool_name.split("::", 1)[0]
    return "unknown"


def validate_encode_response(response: dict, expected_router: str, expected_token_in: str) -> int:
    interactions = response.get("interactions")
    if not isinstance(interactions, list) or len(interactions) not in (2, 3):
        raise AssertionError("interactions length mismatch")

    if len(interactions) == 2:
        approval, router = interactions
        if approval.get("kind") != "ERC20_APPROVE":
            raise AssertionError("approval interaction missing")
        if router.get("kind") != "CALL":
            raise AssertionError("router call interaction missing")
        assert_hex(approval.get("target", ""), "interaction target")
        assert_hex(router.get("target", ""), "interaction target")
        assert_hex(approval.get("calldata", ""), "interaction calldata")
        assert_hex(router.get("calldata", ""), "interaction calldata")
        if approval.get("target", "").lower() != expected_token_in.lower():
            raise AssertionError("approval target mismatch (tokenIn)")
        if router.get("target", "").lower() != expected_router.lower():
            raise AssertionError("router interaction target mismatch")
        return 2

    first, second, third = interactions
    if first.get("kind") != "ERC20_APPROVE" or second.get("kind") != "ERC20_APPROVE":
        raise AssertionError("approval interactions missing or misordered")
    if third.get("kind") != "CALL":
        raise AssertionError("router call interaction missing")

    assert_hex(first.get("target", ""), "interaction target")
    assert_hex(second.get("target", ""), "interaction target")
    assert_hex(third.get("target", ""), "interaction target")
    assert_hex(first.get("calldata", ""), "interaction calldata")
    assert_hex(second.get("calldata", ""), "interaction calldata")
    assert_hex(third.get("calldata", ""), "interaction calldata")

    if first.get("target", "").lower() != expected_token_in.lower():
        raise AssertionError("approval target mismatch (tokenIn)")
    if second.get("target", "").lower() != expected_token_in.lower():
        raise AssertionError("approval target mismatch (tokenIn)")
    if third.get("target", "").lower() != expected_router.lower():
        raise AssertionError("router interaction target mismatch")
    return 3


def main() -> int:
    parser = argparse.ArgumentParser(description="Smoke test POST /encode (spec schema)")
    parser.add_argument("--encode-url", default="http://localhost:3000/encode")
    parser.add_argument("--simulate-url", default="http://localhost:3000/simulate")
    parser.add_argument("--chain-id", help="Runtime chain id (1 or 8453); overrides CHAIN_ID env")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--allow-status", default="ready", help="Comma-separated allowed meta.status values")
    parser.add_argument("--allow-failures", action="store_true", help="Allow meta.failures to be non-empty")
    parser.add_argument("--verbose", action="store_true")

    args = parser.parse_args()

    try:
        chain_id = resolve_chain_id(args.chain_id)
    except ValueError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 2

    repo = Path(args.repo)
    settlement = resolve_contract_address(
        repo,
        "COW_SETTLEMENT_CONTRACT",
        chain_id,
        DEFAULT_SETTLEMENT_BY_CHAIN,
        "settlement address",
    )
    tycho_router = resolve_contract_address(
        repo,
        "TYCHO_ROUTER_ADDRESS",
        chain_id,
        DEFAULT_TYCHO_ROUTER_BY_CHAIN,
        "Tycho router address",
    )
    allowed_statuses = {status.strip() for status in args.allow_status.split(",") if status.strip()}
    if not allowed_statuses:
        print("Error: --allow-status produced no values", file=sys.stderr)
        return 2

    token_in_symbol, mid_symbol, token_out_symbol = default_encode_route(chain_id)
    token_in = resolve_token(token_in_symbol, chain_id)
    mid_token = resolve_token(mid_symbol, chain_id)
    token_out = resolve_token(token_out_symbol, chain_id)

    amounts = amounts_for_pair(token_in, mid_token, chain_id)

    simulate_payload = {
        "request_id": f"encode-smoke-{uuid.uuid4().hex[:8]}",
        "token_in": token_in,
        "token_out": mid_token,
        "amounts": amounts,
    }

    try:
        status, response, elapsed = request_json(args.simulate_url, simulate_payload, args.timeout)
    except (HTTPError, URLError) as exc:
        print(f"[FAIL] simulate request failed: {exc}")
        return 1

    if status != 200:
        print(f"[FAIL] simulate HTTP {status}")
        return 1

    meta = response.get("meta", {})
    status_value = meta.get("status") if isinstance(meta, dict) else None
    failures_list = meta.get("failures", []) if isinstance(meta, dict) else []
    if status_value not in allowed_statuses:
        print(
            f"[FAIL] simulate {token_in_symbol}->{mid_symbol} expected {sorted(allowed_statuses)}, got {status_value}"
        )
        return 1
    if failures_list and not args.allow_failures:
        print(f"[FAIL] simulate {token_in_symbol}->{mid_symbol} had {len(failures_list)} failures")
        return 1

    pool_first = select_pool(response, f"simulate {token_in_symbol}->{mid_symbol}")

    hop_amounts_out = pool_first["amounts_out"]
    hop_amounts_in = [apply_slippage(value, DEFAULT_SLIPPAGE_BPS) for value in hop_amounts_out]
    simulate_payload_second = {
        "request_id": f"encode-smoke-hop-{uuid.uuid4().hex[:8]}",
        "token_in": mid_token,
        "token_out": token_out,
        "amounts": hop_amounts_in,
    }

    try:
        status, response_second, _ = request_json(
            args.simulate_url, simulate_payload_second, args.timeout
        )
    except (HTTPError, URLError) as exc:
        print(f"[FAIL] simulate second hop failed: {exc}")
        return 1

    if status != 200:
        print(f"[FAIL] simulate second hop HTTP {status}")
        return 1

    meta = response_second.get("meta", {})
    status_value = meta.get("status") if isinstance(meta, dict) else None
    failures_list = meta.get("failures", []) if isinstance(meta, dict) else []
    if status_value not in allowed_statuses:
        print(
            f"[FAIL] simulate {mid_symbol}->{token_out_symbol} expected {sorted(allowed_statuses)}, got {status_value}"
        )
        return 1
    if failures_list and not args.allow_failures:
        print(f"[FAIL] simulate {mid_symbol}->{token_out_symbol} had {len(failures_list)} failures")
        return 1

    pool_second = select_pool(response_second, f"simulate {mid_symbol}->{token_out_symbol}")

    protocol_first = protocol_from_pool_name(pool_first.get("pool_name", ""))
    protocol_second = protocol_from_pool_name(pool_second.get("pool_name", ""))

    min_out_first = hop_amounts_in[0]
    if int(min_out_first) <= 0:
        print("[FAIL] computed hop output for first hop is zero", file=sys.stderr)
        return 1
    min_out_second = apply_slippage(pool_second["amounts_out"][0], DEFAULT_SLIPPAGE_BPS)
    if int(min_out_second) <= 0:
        print("[FAIL] computed route minAmountOut is zero", file=sys.stderr)
        return 1

    encode_request = {
        "chainId": chain_id,
        "tokenIn": token_in,
        "tokenOut": token_out,
        "amountIn": amounts[0],
        "minAmountOut": min_out_second,
        "settlementAddress": settlement,
        "tychoRouterAddress": tycho_router,
        "swapKind": "MultiSwap",
        "segments": [
            {
                "kind": "MultiSwap",
                "shareBps": 0,
                "hops": [
                    {
                        "tokenIn": token_in,
                        "tokenOut": mid_token,
                        "swaps": [
                            {
                                "pool": {
                                    "protocol": protocol_first,
                                    "componentId": pool_first["pool"],
                                    "poolAddress": pool_first["pool_address"],
                                },
                                "tokenIn": token_in,
                                "tokenOut": mid_token,
                                "splitBps": 0,
                            }
                        ],
                    },
                    {
                        "tokenIn": mid_token,
                        "tokenOut": token_out,
                        "swaps": [
                            {
                                "pool": {
                                    "protocol": protocol_second,
                                    "componentId": pool_second["pool"],
                                    "poolAddress": pool_second["pool_address"],
                                },
                                "tokenIn": mid_token,
                                "tokenOut": token_out,
                                "splitBps": 0,
                            }
                        ],
                    },
                ],
            }
        ],
        "requestId": f"encode-smoke-{uuid.uuid4().hex[:8]}",
    }

    if args.verbose:
        print(json.dumps(encode_request, indent=2))

    try:
        status, encode_response, elapsed = request_json(
            args.encode_url, encode_request, args.timeout
        )
    except (HTTPError, URLError) as exc:
        print(f"[FAIL] encode request failed: {exc}")
        return 1

    if status != 200:
        print(f"[FAIL] encode HTTP {status}")
        return 1

    try:
        interactions_len = validate_encode_response(encode_response, tycho_router, token_in)
    except AssertionError as exc:
        print(f"[FAIL] encode response validation failed: {exc}")
        return 1

    if args.verbose:
        print(json.dumps(encode_response, indent=2))

    print(
        f"[OK] encode route {elapsed:.3f}s chain={chain_label(chain_id)}:{chain_id} "
        f"path={token_in_symbol}->{mid_symbol}->{token_out_symbol} interactions={interactions_len}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
