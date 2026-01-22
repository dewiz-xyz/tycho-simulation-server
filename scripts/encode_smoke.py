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


def validate_encode_response(
    response: dict,
    pool_first: str,
    pool_second: str,
) -> None:
    if response.get("schemaVersion") != "2026-01-22":
        raise AssertionError("schemaVersion mismatch")
    calls = response.get("calls")
    if not isinstance(calls, list) or len(calls) != 2:
        raise AssertionError("calls length mismatch")

    expected_hops = [pool_first, pool_second]
    for idx, call in enumerate(calls):
        assert_hex(call.get("target", ""), "call target")
        assert_hex(call.get("calldata", ""), "call calldata")
        if call.get("kind") != "TYCHO_SINGLE_SWAP":
            raise AssertionError("call kind mismatch")
        if call.get("hopPath") != f"segment[0].hop[{idx}]":
            raise AssertionError("hopPath mismatch")
        pool = call.get("pool", {})
        if pool.get("componentId") != expected_hops[idx]:
            raise AssertionError("pool componentId mismatch")
        if call.get("minAmountOut") in ("0", 0, None):
            raise AssertionError("minAmountOut must be non-zero")
        approvals = call.get("approvals", [])
        if not isinstance(approvals, list) or not approvals:
            raise AssertionError("approvals missing")
        assert_hex(approvals[0].get("token", ""), "approval token")
        assert_hex(approvals[0].get("spender", ""), "approval spender")


def main() -> int:
    parser = argparse.ArgumentParser(description="Smoke test POST /encode (spec schema)")
    parser.add_argument("--encode-url", default="http://localhost:3000/encode")
    parser.add_argument("--simulate-url", default="http://localhost:3000/simulate")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--verbose", action="store_true")

    args = parser.parse_args()

    repo = Path(args.repo)
    settlement = load_env_value(repo, "COW_SETTLEMENT_CONTRACT") or DEFAULT_SETTLEMENT
    tycho_router = load_env_value(repo, "TYCHO_ROUTER_ADDRESS") or DEFAULT_TYCHO_ROUTER

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

    status_value = response.get("meta", {}).get("status")
    if status_value != "ready":
        print(f"[FAIL] simulate dai->usdc expected ready, got {status_value}")
        return 1

    pool_first = select_pool(response, "simulate dai->usdc")

    hop_amounts_out = pool_first["amounts_out"]
    simulate_payload_second = {
        "request_id": f"encode-smoke-hop-{uuid.uuid4().hex[:8]}",
        "token_in": usdc,
        "token_out": usdt,
        "amounts": hop_amounts_out,
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

    status_value = response_second.get("meta", {}).get("status")
    if status_value != "ready":
        print(f"[FAIL] simulate usdc->usdt expected ready, got {status_value}")
        return 1

    pool_second = select_pool(response_second, "simulate usdc->usdt")

    protocol_first = protocol_from_pool_name(pool_first.get("pool_name", ""))
    protocol_second = protocol_from_pool_name(pool_second.get("pool_name", ""))

    encode_request = {
        "chainId": 1,
        "tokenIn": dai,
        "tokenOut": usdt,
        "amountIn": amounts[0],
        "settlementAddress": settlement,
        "tychoRouterAddress": tycho_router,
        "slippageBps": 25,
        "segments": [
            {
                "shareBps": 10_000,
                "hops": [
                    {
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
                                "splitBps": 10_000,
                            }
                        ],
                    },
                    {
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
                                "splitBps": 10_000,
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
        validate_encode_response(
            encode_response,
            pool_first["pool"],
            pool_second["pool"],
        )
    except AssertionError as exc:
        print(f"[FAIL] encode response validation failed: {exc}")
        return 1

    if args.verbose:
        print(json.dumps(encode_response, indent=2))

    print(f"[OK] encode route {elapsed:.3f}s calls=2")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
