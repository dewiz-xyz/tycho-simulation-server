#!/usr/bin/env python3
"""Smoke test helper for POST /simulate."""

from __future__ import annotations

import argparse
import json
import sys
import time
import uuid
from urllib import request
from urllib.error import HTTPError, URLError

from presets import (
    TOKENS,
    default_amounts_for_token,
    list_suites,
    list_tokens,
    parse_amounts,
    parse_pairs,
    suite_pairs,
)

ADDRESS_TO_SYMBOL = {address: symbol for (symbol, address) in TOKENS.items()}


def request_simulate(url, payload, timeout):
    data = json.dumps(payload).encode("utf-8")
    req = request.Request(url, data=data, headers={"Content-Type": "application/json"})
    start = time.perf_counter()
    with request.urlopen(req, timeout=timeout) as response:
        body = response.read()
        elapsed = time.perf_counter() - start
        return response.status, body, elapsed


def fmt_token(address: str) -> str:
    address = address.lower()
    symbol = ADDRESS_TO_SYMBOL.get(address)
    if symbol:
        return symbol
    return address


def is_int_string(value) -> bool:
    if not isinstance(value, str) or not value:
        return False
    return value.isdigit()


def validate_pool_entry(entry, expected_len: int) -> tuple[bool, str]:
    if not isinstance(entry, dict):
        return False, "pool entry is not an object"

    amounts_out = entry.get("amounts_out")
    if not isinstance(amounts_out, list) or len(amounts_out) != expected_len:
        return False, f"amounts_out length mismatch (expected {expected_len})"
    if not all(is_int_string(v) for v in amounts_out):
        return False, "amounts_out contains non-integer strings"

    gas_used = entry.get("gas_used")
    if not isinstance(gas_used, list) or len(gas_used) != expected_len:
        return False, f"gas_used length mismatch (expected {expected_len})"
    if not all(isinstance(v, int) and v >= 0 for v in gas_used):
        return False, "gas_used contains non-integers"

    block_number = entry.get("block_number")
    if not isinstance(block_number, int) or block_number < 0:
        return False, "block_number is invalid"

    prev = -1
    for raw in amounts_out:
        current = int(raw)
        if current < prev:
            return False, "amounts_out is not monotonic"
        prev = current

    return True, ""


def main() -> int:
    parser = argparse.ArgumentParser(description="Smoke test POST /simulate")
    parser.add_argument("--url", default="http://localhost:3000/simulate")
    parser.add_argument("--suite", default="smoke", help="Named pair suite from presets")
    parser.add_argument("--pair", action="append", help="token_in:token_out (symbol or address)")
    parser.add_argument("--pairs", help="Comma-separated token_in:token_out pairs")
    parser.add_argument("--amounts", help="Comma-separated amounts in wei")
    parser.add_argument("--allow-status", default="ready", help="Comma-separated allowed meta.status values")
    parser.add_argument("--allow-failures", action="store_true", help="Allow meta.failures to be non-empty")
    parser.add_argument("--require-data", action="store_true", help="Fail if response data is empty")
    parser.add_argument(
        "--validate-data",
        action="store_true",
        help="Validate response pool entries (amounts_out/gas_used lengths, ints, monotonicity)",
    )
    parser.add_argument("--list-suites", action="store_true", help="List available suites and exit")
    parser.add_argument("--list-tokens", action="store_true", help="List available token symbols and exit")
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--request-id-prefix", default="smoke")
    parser.add_argument("--dry-run", action="store_true", help="Print payloads without sending")
    parser.add_argument("--verbose", action="store_true")

    args = parser.parse_args()

    if args.list_suites:
        for name in list_suites():
            print(name)
        return 0

    if args.list_tokens:
        for symbol, address in list_tokens():
            print(f"{symbol} {address}")
        return 0

    try:
        pairs = parse_pairs(args.pair, args.pairs)
    except ValueError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 2

    if not pairs:
        try:
            pairs = suite_pairs(args.suite)
        except ValueError as exc:
            print(f"Error: {exc}", file=sys.stderr)
            return 2

    try:
        amounts_override = parse_amounts(args.amounts) if args.amounts else None
    except ValueError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 2

    allowed_statuses = {s.strip() for s in args.allow_status.split(",") if s.strip()}
    if not allowed_statuses:
        print("Error: --allow-status produced no values", file=sys.stderr)
        return 2

    failures = 0

    for idx, pair in enumerate(pairs, start=1):
        token_in = pair.token_in
        token_out = pair.token_out
        amounts = amounts_override or default_amounts_for_token(token_in)
        payload = {
            "request_id": f"{args.request_id_prefix}-{idx}-{uuid.uuid4().hex[:8]}",
            "token_in": token_in,
            "token_out": token_out,
            "amounts": amounts,
        }

        if args.dry_run:
            print(json.dumps(payload, indent=2))
            continue

        try:
            status_code, body, elapsed = request_simulate(args.url, payload, args.timeout)
        except HTTPError as exc:
            print(f"[FAIL] {token_in}->{token_out} HTTP {exc.code}")
            failures += 1
            continue
        except URLError as exc:
            print(f"[FAIL] {token_in}->{token_out} {exc.reason}")
            failures += 1
            continue
        except Exception as exc:  # pragma: no cover - unexpected errors
            print(f"[FAIL] {token_in}->{token_out} {exc}")
            failures += 1
            continue

        pair_label = f"{fmt_token(token_in)}->{fmt_token(token_out)}"

        if status_code != 200:
            print(f"[FAIL] {pair_label} HTTP {status_code}")
            failures += 1
            continue

        try:
            response_json = json.loads(body)
        except json.JSONDecodeError:
            print(f"[FAIL] {pair_label} invalid JSON response")
            failures += 1
            continue

        meta = response_json.get("meta", {})
        status = meta.get("status")
        failure_list = meta.get("failures", [])
        meta_summary = f"status={status} failures={len(failure_list)}"

        if status not in allowed_statuses:
            print(f"[FAIL] {pair_label} {elapsed:.3f}s {meta_summary}")
            failures += 1
            continue

        if failure_list and not args.allow_failures:
            print(f"[FAIL] {pair_label} {elapsed:.3f}s {meta_summary}")
            failures += 1
            continue

        data = response_json.get("data", [])
        if args.require_data and (not isinstance(data, list) or len(data) == 0):
            print(f"[FAIL] {pair_label} {elapsed:.3f}s {meta_summary} data=empty")
            failures += 1
            continue

        if args.validate_data and isinstance(data, list):
            for entry in data:
                ok, error = validate_pool_entry(entry, expected_len=len(amounts))
                if not ok:
                    print(f"[FAIL] {pair_label} invalid pool entry: {error}")
                    failures += 1
                    break
            else:
                print(f"[OK] {pair_label} {elapsed:.3f}s {meta_summary} pools={len(data)}")
                if args.verbose:
                    print(json.dumps(meta, indent=2))
                continue
            continue

        pools_count = len(data) if isinstance(data, list) else 0
        print(f"[OK] {pair_label} {elapsed:.3f}s {meta_summary} pools={pools_count}")
        if args.verbose:
            print(json.dumps(meta, indent=2))

    if args.dry_run:
        return 0

    total = len(pairs)
    success = total - failures
    print(f"\nSummary: {success}/{total} successful")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
