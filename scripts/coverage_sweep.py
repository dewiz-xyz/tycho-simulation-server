#!/usr/bin/env python3
"""Coverage sweep for POST /simulate."""

from __future__ import annotations

import argparse
import json
import sys
import time
import uuid
from collections import Counter
from dataclasses import asdict
from pathlib import Path
from urllib import request
from urllib.error import HTTPError, URLError

from presets import default_amounts_for_token, list_suites, list_tokens, parse_amounts, parse_pairs, suite_pairs


def request_simulate(url: str, payload: dict, timeout: float) -> tuple[int, bytes, float]:
    data = json.dumps(payload).encode("utf-8")
    req = request.Request(url, data=data, headers={"Content-Type": "application/json"})
    start = time.perf_counter()
    with request.urlopen(req, timeout=timeout) as response:
        body = response.read()
        elapsed = time.perf_counter() - start
        return response.status, body, elapsed


def protocol_from_pool_name(pool_name: str | None) -> str:
    if not pool_name:
        return "unknown"
    if "::" in pool_name:
        return pool_name.split("::", 1)[0]
    return pool_name


def main() -> int:
    parser = argparse.ArgumentParser(description="Coverage sweep for POST /simulate")
    parser.add_argument("--url", default="http://localhost:3000/simulate")
    parser.add_argument("--suite", default="core", help="Named pair suite from presets")
    parser.add_argument("--pair", action="append", help="token_in:token_out (symbol or address)")
    parser.add_argument("--pairs", help="Comma-separated token_in:token_out pairs")
    parser.add_argument("--amounts", help="Comma-separated amounts in wei")
    parser.add_argument("--allow-status", default="ready", help="Comma-separated allowed meta.status values")
    parser.add_argument("--allow-failures", action="store_true", help="Allow meta.failures to be non-empty")
    parser.add_argument("--expect-protocols", help="Comma-separated protocol names expected in pool_name")
    parser.add_argument("--out", help="Write JSON report to this path")
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--request-id-prefix", default="coverage")
    parser.add_argument("--list-suites", action="store_true", help="List available suites and exit")
    parser.add_argument("--list-tokens", action="store_true", help="List available token symbols and exit")
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
        if not pairs:
            pairs = suite_pairs(args.suite)
        amounts_override = parse_amounts(args.amounts) if args.amounts else None
    except ValueError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 2

    allowed_statuses = {s.strip() for s in args.allow_status.split(",") if s.strip()}
    if not allowed_statuses:
        print("Error: --allow-status produced no values", file=sys.stderr)
        return 2

    expected_protocols: set[str] = set()
    if args.expect_protocols:
        expected_protocols = {p.strip().lower() for p in args.expect_protocols.split(",") if p.strip()}

    report = {
        "url": args.url,
        "suite": args.suite,
        "pairs": [asdict(p) for p in pairs],
        "amounts": amounts_override,
        "amounts_strategy": "explicit" if amounts_override else "per_token_decimals",
        "requests": [],
        "pools": [],
        "summary": {},
    }

    pool_protocols: Counter[str] = Counter()
    pools_seen: dict[str, dict] = {}
    meta_statuses: Counter[str] = Counter()
    failure_kinds: Counter[str] = Counter()

    failures = 0

    for idx, pair in enumerate(pairs, start=1):
        amounts = amounts_override or default_amounts_for_token(pair.token_in)
        payload = {
            "request_id": f"{args.request_id_prefix}-{idx}-{uuid.uuid4().hex[:8]}",
            "token_in": pair.token_in,
            "token_out": pair.token_out,
            "amounts": amounts,
        }

        try:
            status_code, body, elapsed = request_simulate(args.url, payload, args.timeout)
        except HTTPError as exc:
            failures += 1
            report["requests"].append(
                {"pair": asdict(pair), "error": f"http_error:{exc.code}", "elapsed_s": None}
            )
            continue
        except URLError as exc:
            failures += 1
            report["requests"].append(
                {"pair": asdict(pair), "error": f"url_error:{exc.reason}", "elapsed_s": None}
            )
            continue
        except Exception as exc:  # pragma: no cover - unexpected errors
            failures += 1
            report["requests"].append({"pair": asdict(pair), "error": str(exc), "elapsed_s": None})
            continue

        if status_code != 200:
            failures += 1
            report["requests"].append(
                {"pair": asdict(pair), "error": f"http_status:{status_code}", "elapsed_s": elapsed}
            )
            continue

        try:
            response_json = json.loads(body)
        except json.JSONDecodeError:
            failures += 1
            report["requests"].append(
                {"pair": asdict(pair), "error": "invalid_json", "elapsed_s": elapsed}
            )
            continue

        meta = response_json.get("meta", {})
        status = meta.get("status")
        meta_statuses[str(status)] += 1

        failures_list = meta.get("failures", []) if isinstance(meta, dict) else []
        if isinstance(failures_list, list):
            for item in failures_list:
                kind = item.get("kind") if isinstance(item, dict) else None
                if kind is not None:
                    failure_kinds[str(kind)] += 1

        if status not in allowed_statuses:
            failures += 1
            report["requests"].append(
                {
                    "pair": asdict(pair),
                    "elapsed_s": elapsed,
                    "meta": meta,
                    "error": f"meta_status:{status}",
                }
            )
            continue

        if failures_list and not args.allow_failures:
            failures += 1
            report["requests"].append(
                {"pair": asdict(pair), "elapsed_s": elapsed, "meta": meta, "error": "meta_failures"}
            )
            continue

        data = response_json.get("data", [])
        pool_count = len(data) if isinstance(data, list) else 0

        if isinstance(data, list):
            for entry in data:
                if not isinstance(entry, dict):
                    continue
                pool_address = entry.get("pool_address")
                pool_name = entry.get("pool_name")
                pool_id = entry.get("pool")
                protocol = protocol_from_pool_name(pool_name).lower()
                pool_protocols[protocol] += 1
                if isinstance(pool_address, str):
                    pools_seen.setdefault(
                        pool_address.lower(),
                        {
                            "pool": pool_id,
                            "pool_name": pool_name,
                            "pool_address": pool_address,
                            "protocol": protocol,
                        },
                    )

        report["requests"].append(
            {"pair": asdict(pair), "elapsed_s": elapsed, "meta": meta, "pool_count": pool_count, "amounts": amounts}
        )

        if args.verbose:
            print(
                f"[OK] {pair.token_in}->{pair.token_out} {elapsed:.3f}s "
                f"status={status} pools={pool_count}"
            )

    observed_protocols = sorted(pool_protocols.keys())
    missing_expected = sorted([p for p in expected_protocols if p not in set(observed_protocols)])

    report["summary"] = {
        "pairs": len(pairs),
        "failures": failures,
        "meta_statuses": dict(meta_statuses),
        "failure_kinds": dict(failure_kinds),
        "unique_pools": len(pools_seen),
        "observed_protocols": observed_protocols,
        "protocol_counts": dict(pool_protocols),
        "missing_expected_protocols": missing_expected,
    }
    report["pools"] = sorted(
        pools_seen.values(), key=lambda item: (item.get("protocol", ""), item.get("pool_name", ""))
    )

    print("Coverage sweep summary")
    print("=====================")
    print(f"Pairs: {len(pairs)}")
    print(f"Failures: {failures}")
    print(f"Unique pools observed: {len(pools_seen)}")
    print(f"Protocols observed: {', '.join(observed_protocols) if observed_protocols else '(none)'}")

    if pool_protocols:
        print("\nTop protocols by pool appearances (top 10):")
        for protocol, count in pool_protocols.most_common(10):
            print(f"- {protocol}: {count}")

    if missing_expected:
        print("\nMissing expected protocols:")
        for protocol in missing_expected:
            print(f"- {protocol}")

    if args.out:
        out_path = Path(args.out).expanduser().resolve()
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(report, indent=2))
        print(f"\nWrote report: {out_path}")

    if missing_expected:
        return 1
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
