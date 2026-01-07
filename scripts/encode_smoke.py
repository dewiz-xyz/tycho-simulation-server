#!/usr/bin/env python3
"""Smoke test helper for POST /encode."""

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


def request_json(url: str, payload: dict, timeout: float) -> tuple[int, dict, float]:
    data = json.dumps(payload).encode("utf-8")
    req = request.Request(url, data=data, headers={"Content-Type": "application/json"})
    start = time.perf_counter()
    with request.urlopen(req, timeout=timeout) as response:
        body = response.read()
        elapsed = time.perf_counter() - start
        return response.status, json.loads(body), elapsed


def load_env_contract(repo: Path) -> str | None:
    env_value = os.environ.get("COW_SETTLEMENT_CONTRACT")
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
            key, value = line.split("=", 1)
            if key.strip() == "COW_SETTLEMENT_CONTRACT":
                return value.strip()
    except OSError:
        return None
    return None


def assert_hex(value: str, label: str) -> None:
    if not isinstance(value, str) or not value.startswith("0x"):
        raise AssertionError(f"{label} must be 0x-prefixed hex")
    if len(value) % 2 != 0:
        raise AssertionError(f"{label} must be even-length hex")


def validate_calldata_meta(calldata: dict) -> None:
    assert_hex(calldata.get("sender", ""), "calldata sender")
    assert_hex(calldata.get("receiver", ""), "calldata receiver")


def select_pool(response: dict, label: str) -> dict:
    data = response.get("data")
    if not isinstance(data, list) or not data:
        raise AssertionError(f"{label}: no pool data")
    pool = data[0]
    required = ["pool", "amounts_out", "gas_used", "block_number"]
    for key in required:
        if key not in pool:
            raise AssertionError(f"{label}: missing {key}")
    return pool


def validate_transfer_from(calldata: dict, amounts_len: int) -> None:
    validate_calldata_meta(calldata)
    steps = calldata.get("steps")
    if not isinstance(steps, list) or len(steps) != amounts_len:
        raise AssertionError("calldata.steps length mismatch")
    for step in steps:
        calls = step.get("calls")
        if not isinstance(calls, list) or len(calls) != 1:
            raise AssertionError("transfer_from step should have 1 call")
        call = calls[0]
        if call.get("call_type") != "tycho_router":
            raise AssertionError("transfer_from call_type should be tycho_router")
        assert_hex(call.get("target", ""), "router target")
        assert_hex(call.get("calldata", ""), "router calldata")
        allowances = call.get("allowances", [])
        if not isinstance(allowances, list) or len(allowances) != 1:
            raise AssertionError("transfer_from should include one allowance")


def validate_none_mode(calldata: dict, amounts_len: int) -> None:
    validate_calldata_meta(calldata)
    steps = calldata.get("steps")
    if not isinstance(steps, list) or len(steps) != amounts_len:
        raise AssertionError("calldata.steps length mismatch")
    for step in steps:
        calls = step.get("calls")
        if not isinstance(calls, list) or len(calls) != 2:
            raise AssertionError("none mode step should have 2 calls")
        if calls[0].get("call_type") != "erc20_transfer":
            raise AssertionError("none mode first call should be erc20_transfer")
        if calls[1].get("call_type") != "tycho_router":
            raise AssertionError("none mode second call should be tycho_router")
        assert_hex(calls[0].get("target", ""), "erc20 transfer target")
        assert_hex(calls[1].get("target", ""), "router target")
        assert_hex(calls[0].get("calldata", ""), "erc20 transfer calldata")
        assert_hex(calls[1].get("calldata", ""), "router calldata")
        allowances = calls[1].get("allowances", [])
        if allowances:
            raise AssertionError("none mode router call should not include allowances")


def main() -> int:
    parser = argparse.ArgumentParser(description="Smoke test POST /encode")
    parser.add_argument("--encode-url", default="http://localhost:3000/encode")
    parser.add_argument("--simulate-url", default="http://localhost:3000/simulate")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--verbose", action="store_true")

    args = parser.parse_args()

    repo = Path(args.repo)
    env_contract = load_env_contract(repo)
    settlement = env_contract or DEFAULT_SETTLEMENT
    has_env_default = env_contract is not None

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

    pool = select_pool(response, "simulate dai->usdc")

    hop_amounts_out = pool["amounts_out"]
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

    route_single = {
        "route_id": "route-single",
        "hops": [
            {"pool_id": pool["pool"], "token_in": dai, "token_out": usdc},
        ],
        "amounts": amounts,
        "amounts_out": pool["amounts_out"],
        "gas_used": pool["gas_used"],
        "block_number": pool["block_number"],
    }

    route_multi = {
        "route_id": "route-multi",
        "hops": [
            {"pool_id": pool["pool"], "token_in": dai, "token_out": usdc},
            {"pool_id": pool_second["pool"], "token_in": usdc, "token_out": usdt},
        ],
        "amounts": amounts,
        "amounts_out": pool_second["amounts_out"],
        "gas_used": pool_second["gas_used"],
        "block_number": pool_second["block_number"],
    }

    route_invalid = {
        "route_id": "route-invalid",
        "hops": [
            {"pool_id": pool["pool"], "token_in": dai, "token_out": usdc},
            {"pool_id": pool_second["pool"], "token_in": dai, "token_out": usdt},
        ],
        "amounts": amounts,
        "amounts_out": pool_second["amounts_out"],
        "gas_used": pool_second["gas_used"],
        "block_number": pool_second["block_number"],
    }

    transfer_from_payload = {
        "request_id": f"encode-smoke-transfer-{uuid.uuid4().hex[:8]}",
        "token_in": dai,
        "token_out": usdc,
        "calldata": {
            "mode": "tycho_router_transfer_from",
            "sender": settlement,
            "receiver": settlement,
        },
        "routes": [route_single, route_invalid],
    }

    try:
        status, encode_response, _ = request_json(
            args.encode_url, transfer_from_payload, args.timeout
        )
    except (HTTPError, URLError) as exc:
        print(f"[FAIL] encode transfer_from failed: {exc}")
        return 1

    if status != 200:
        print(f"[FAIL] encode transfer_from HTTP {status}")
        return 1

    meta_status = encode_response.get("meta", {}).get("status")
    if meta_status != "partial_failure":
        print(f"[FAIL] transfer_from expected partial_failure, got {meta_status}")
        return 1

    data = encode_response.get("data", [])
    if len(data) != 2:
        print("[FAIL] transfer_from expected 2 routes")
        return 1

    if data[0].get("route_id") != "route-single" or data[1].get("route_id") != "route-invalid":
        print("[FAIL] transfer_from route ordering mismatch")
        return 1

    if "calldata" not in data[0]:
        print("[FAIL] transfer_from missing calldata for valid route")
        return 1

    if "calldata" in data[1]:
        print("[FAIL] invalid route should not include calldata")
        return 1

    validate_transfer_from(data[0]["calldata"], len(amounts))

    none_payload = {
        "request_id": f"encode-smoke-none-{uuid.uuid4().hex[:8]}",
        "token_in": dai,
        "token_out": usdc,
        "calldata": {
            "mode": "tycho_router_none",
            "sender": settlement,
            "receiver": settlement,
        },
        "routes": [route_single],
    }

    try:
        status, none_response, _ = request_json(args.encode_url, none_payload, args.timeout)
    except (HTTPError, URLError) as exc:
        print(f"[FAIL] encode none failed: {exc}")
        return 1

    if status != 200:
        print(f"[FAIL] encode none HTTP {status}")
        return 1

    meta_status = none_response.get("meta", {}).get("status")
    if meta_status != "ready":
        print(f"[FAIL] none mode expected ready, got {meta_status}")
        return 1

    none_data = none_response.get("data", [])
    if len(none_data) != 1 or "calldata" not in none_data[0]:
        print("[FAIL] none mode missing calldata")
        return 1

    validate_none_mode(none_data[0]["calldata"], len(amounts))

    multi_payload = {
        "request_id": f"encode-smoke-multi-{uuid.uuid4().hex[:8]}",
        "token_in": dai,
        "token_out": usdt,
        "calldata": {
            "mode": "tycho_router_transfer_from",
            "sender": settlement,
            "receiver": settlement,
        },
        "routes": [route_multi],
    }

    try:
        status, multi_response, _ = request_json(args.encode_url, multi_payload, args.timeout)
    except (HTTPError, URLError) as exc:
        print(f"[FAIL] encode multi-hop failed: {exc}")
        return 1

    if status != 200:
        print(f"[FAIL] encode multi-hop HTTP {status}")
        return 1

    multi_status = multi_response.get("meta", {}).get("status")
    if multi_status != "ready":
        print(f"[FAIL] multi-hop expected ready, got {multi_status}")
        return 1

    multi_data = multi_response.get("data", [])
    if len(multi_data) != 1 or "calldata" not in multi_data[0]:
        print("[FAIL] multi-hop missing calldata")
        return 1

    validate_transfer_from(multi_data[0]["calldata"], len(amounts))

    defaults_payload = {
        "request_id": f"encode-smoke-defaults-{uuid.uuid4().hex[:8]}",
        "token_in": dai,
        "token_out": usdc,
        "calldata": {"mode": "tycho_router_transfer_from"},
        "routes": [route_single],
    }

    try:
        status, defaults_response, _ = request_json(
            args.encode_url, defaults_payload, args.timeout
        )
    except (HTTPError, URLError) as exc:
        print(f"[FAIL] encode defaults failed: {exc}")
        return 1

    if status != 200:
        print(f"[FAIL] encode defaults HTTP {status}")
        return 1

    defaults_status = defaults_response.get("meta", {}).get("status")
    if has_env_default:
        if defaults_status != "ready":
            print(f"[FAIL] defaults expected ready, got {defaults_status}")
            return 1
        if "calldata" not in defaults_response.get("data", [{}])[0]:
            print("[FAIL] defaults missing calldata")
            return 1
    else:
        if defaults_status != "invalid_request":
            print(f"[FAIL] defaults expected invalid_request, got {defaults_status}")
            return 1
        if defaults_response.get("data"):
            print("[FAIL] defaults invalid_request should return empty data")
            return 1

    duplicate_payload = {
        "request_id": f"encode-smoke-dup-{uuid.uuid4().hex[:8]}",
        "token_in": dai,
        "token_out": usdc,
        "calldata": {
            "mode": "tycho_router_transfer_from",
            "sender": settlement,
            "receiver": settlement,
        },
        "routes": [
            {**route_single, "route_id": "dup"},
            {**route_single, "route_id": "dup"},
        ],
    }

    try:
        status, dup_response, _ = request_json(args.encode_url, duplicate_payload, args.timeout)
    except (HTTPError, URLError) as exc:
        print(f"[FAIL] encode duplicate failed: {exc}")
        return 1

    if status != 200:
        print(f"[FAIL] encode duplicate HTTP {status}")
        return 1

    dup_status = dup_response.get("meta", {}).get("status")
    if dup_status != "invalid_request":
        print(f"[FAIL] duplicate expected invalid_request, got {dup_status}")
        return 1
    if dup_response.get("data"):
        print("[FAIL] duplicate invalid_request should return empty data")
        return 1

    if args.verbose:
        print(json.dumps({"transfer_from": encode_response, "none": none_response}, indent=2))

    print("[OK] encode smoke passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
