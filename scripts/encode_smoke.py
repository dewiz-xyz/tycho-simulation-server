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

from presets import TOKENS, default_amounts_for_token

DEFAULT_SETTLEMENT = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41"
DEFAULT_TYCHO_ROUTER = "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35"
DEFAULT_SLIPPAGE_BPS = 25


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
            if name.strip() == key:
                return value.strip()
    except OSError:
        return None
    return None


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
    required = ["pool", "amounts_out", "gas_used", "block_number", "pool_name", "pool_address"]
    for key in required:
        if key not in pool:
            raise AssertionError(f"{label}: missing {key}")
    return pool


def protocol_from_pool_name(pool_name: str) -> str:
    if "::" in pool_name:
        return pool_name.split("::", 1)[0]
    return "unknown"


def validate_encode_response(response: dict, expected_router: str) -> int:
    if response.get("swapKind") != "MultiSwap":
        raise AssertionError("swapKind mismatch")
    token_in = response.get("tokenIn")
    assert_hex(token_in, "tokenIn")
    normalized = response.get("normalizedRoute", {})
    segments = normalized.get("segments")
    if not isinstance(segments, list) or not segments:
        raise AssertionError("normalizedRoute segments missing")
    if segments[0].get("kind") != "MultiSwap":
        raise AssertionError("segment kind mismatch")
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
        if approval.get("target", "").lower() != token_in.lower():
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

    if first.get("target", "").lower() != token_in.lower():
        raise AssertionError("approval target mismatch (tokenIn)")
    if second.get("target", "").lower() != token_in.lower():
        raise AssertionError("approval target mismatch (tokenIn)")
    if third.get("target", "").lower() != expected_router.lower():
        raise AssertionError("router interaction target mismatch")
    return 3


def validate_pool_ordering(response: dict, pool_first: dict, pool_second: dict) -> None:
    normalized = response.get("normalizedRoute", {})
    segments = normalized.get("segments")
    if not isinstance(segments, list) or len(segments) != 1:
        raise AssertionError("normalizedRoute segments length mismatch")

    hops = segments[0].get("hops")
    if not isinstance(hops, list) or len(hops) != 2:
        raise AssertionError("normalizedRoute hops length mismatch")

    first_swaps = hops[0].get("swaps")
    second_swaps = hops[1].get("swaps")
    if not isinstance(first_swaps, list) or not first_swaps:
        raise AssertionError("normalizedRoute first hop swaps missing")
    if not isinstance(second_swaps, list) or not second_swaps:
        raise AssertionError("normalizedRoute second hop swaps missing")

    first_pool = first_swaps[0].get("pool", {}).get("componentId")
    second_pool = second_swaps[0].get("pool", {}).get("componentId")
    if first_pool != pool_first["pool"]:
        raise AssertionError("normalizedRoute first hop pool mismatch")
    if second_pool != pool_second["pool"]:
        raise AssertionError("normalizedRoute second hop pool mismatch")


def main() -> int:
    parser = argparse.ArgumentParser(description="Smoke test POST /encode (spec schema)")
    parser.add_argument("--encode-url", default="http://localhost:3000/encode")
    parser.add_argument("--simulate-url", default="http://localhost:3000/simulate")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--allow-status", default="ready", help="Comma-separated allowed meta.status values")
    parser.add_argument("--allow-failures", action="store_true", help="Allow meta.failures to be non-empty")
    parser.add_argument("--verbose", action="store_true")

    args = parser.parse_args()

    repo = Path(args.repo)
    settlement = load_env_value(repo, "COW_SETTLEMENT_CONTRACT") or DEFAULT_SETTLEMENT
    tycho_router = load_env_value(repo, "TYCHO_ROUTER_ADDRESS") or DEFAULT_TYCHO_ROUTER
    allowed_statuses = {status.strip() for status in args.allow_status.split(",") if status.strip()}
    if not allowed_statuses:
        print("Error: --allow-status produced no values", file=sys.stderr)
        return 2

    dai = TOKENS.get("DAI")
    usdc = TOKENS.get("USDC")
    usdt = TOKENS.get("USDT")
    if not dai or not usdc or not usdt:
        print("Error: DAI/USDC/USDT must be present in presets", file=sys.stderr)
        return 2

    amounts = default_amounts_for_token(dai)

    simulate_payload = {
        "request_id": f"encode-smoke-{uuid.uuid4().hex[:8]}",
        "token_in": dai,
        "token_out": usdc,
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
        print(f"[FAIL] simulate dai->usdc expected {sorted(allowed_statuses)}, got {status_value}")
        return 1
    if failures_list and not args.allow_failures:
        print(f"[FAIL] simulate dai->usdc had {len(failures_list)} failures")
        return 1

    pool_first = select_pool(response, "simulate dai->usdc")

    hop_amounts_out = pool_first["amounts_out"]
    hop_amounts_in = [apply_slippage(value, DEFAULT_SLIPPAGE_BPS) for value in hop_amounts_out]
    simulate_payload_second = {
        "request_id": f"encode-smoke-hop-{uuid.uuid4().hex[:8]}",
        "token_in": usdc,
        "token_out": usdt,
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
        print(f"[FAIL] simulate usdc->usdt expected {sorted(allowed_statuses)}, got {status_value}")
        return 1
    if failures_list and not args.allow_failures:
        print(f"[FAIL] simulate usdc->usdt had {len(failures_list)} failures")
        return 1

    pool_second = select_pool(response_second, "simulate usdc->usdt")

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
        "chainId": 1,
        "tokenIn": dai,
        "tokenOut": usdt,
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
                        "shareBps": 10_000,
                        "tokenIn": dai,
                        "tokenOut": usdc,
                        "swaps": [
                            {
                                "pool": {
                                    "protocol": protocol_first,
                                    "componentId": pool_first["pool"],
                                    "poolAddress": pool_first["pool_address"],
                                },
                                "tokenIn": dai,
                                "tokenOut": usdc,
                                "splitBps": 0,
                            }
                        ],
                    },
                    {
                        "shareBps": 10_000,
                        "tokenIn": usdc,
                        "tokenOut": usdt,
                        "swaps": [
                            {
                                "pool": {
                                    "protocol": protocol_second,
                                    "componentId": pool_second["pool"],
                                    "poolAddress": pool_second["pool_address"],
                                },
                                "tokenIn": usdc,
                                "tokenOut": usdt,
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
        interactions_len = validate_encode_response(encode_response, tycho_router)
        validate_pool_ordering(encode_response, pool_first, pool_second)
    except AssertionError as exc:
        print(f"[FAIL] encode response validation failed: {exc}")
        return 1

    if args.verbose:
        print(json.dumps(encode_response, indent=2))

    print(f"[OK] encode route {elapsed:.3f}s interactions={interactions_len}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
