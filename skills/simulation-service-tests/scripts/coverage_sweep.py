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

from presets import (
    amounts_for_pair,
    chain_label,
    list_suites,
    list_tokens,
    parse_amounts,
    parse_pairs,
    resolve_chain_id,
    suite_pairs,
)


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
        protocol = pool_name.split("::", 1)[0]
    else:
        protocol = pool_name
    # VM pools can use prefixes like vm:maverick_v2; normalize for stable assertions.
    if protocol.startswith("vm:"):
        return protocol.split(":", 1)[1]
    return protocol


def protocol_from_fields(protocol: str | None, pool_name: str | None) -> str:
    if protocol:
        if protocol.startswith("vm:"):
            return protocol.split(":", 1)[1]
        return protocol
    return protocol_from_pool_name(pool_name)


def collect_candidate_protocols(response_json: dict) -> Counter[str]:
    candidate_protocols: Counter[str] = Counter()

    data = response_json.get("data", [])
    if isinstance(data, list):
        for entry in data:
            if not isinstance(entry, dict):
                continue
            protocol = protocol_from_fields(entry.get("protocol"), entry.get("pool_name")).lower()
            candidate_protocols[protocol] += 1

    meta = response_json.get("meta", {})
    pool_results = meta.get("pool_results", []) if isinstance(meta, dict) else []
    if isinstance(pool_results, list):
        for entry in pool_results:
            if not isinstance(entry, dict):
                continue
            protocol = protocol_from_fields(entry.get("protocol"), entry.get("pool_name")).lower()
            candidate_protocols[protocol] += 1

    return candidate_protocols


def resolve_allowed_statuses(
    allow_status: str,
    *,
    allow_failures: bool,
    allow_no_pools: bool,
) -> set[str]:
    allowed_statuses = {status.strip() for status in allow_status.split(",") if status.strip()}
    if allow_failures:
        allowed_statuses.add("partial_success")
    if allow_no_pools:
        allowed_statuses.add("no_liquidity")
    return allowed_statuses


def main() -> int:
    parser = argparse.ArgumentParser(description="Coverage sweep for POST /simulate")
    parser.add_argument("--url", default="http://localhost:3000/simulate")
    parser.add_argument("--chain-id", help="Runtime chain id (1 or 8453); overrides CHAIN_ID env")
    parser.add_argument("--suite", default="core", help="Named pair suite from presets")
    parser.add_argument("--pair", action="append", help="token_in:token_out (symbol or address)")
    parser.add_argument("--pairs", help="Comma-separated token_in:token_out pairs")
    parser.add_argument("--amounts", help="Comma-separated amounts in wei")
    parser.add_argument("--allow-status", default="ready", help="Comma-separated allowed meta.status values")
    parser.add_argument("--allow-failures", action="store_true", help="Allow meta.failures to be non-empty")
    parser.add_argument(
        "--allow-no-pools",
        action="store_true",
        help="Allow no_liquidity responses that only report no_pools failures",
    )
    parser.add_argument("--expect-protocols", help="Comma-separated protocol names expected in pool_name")
    parser.add_argument("--out", help="Write JSON report to this path")
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--request-id-prefix", default="coverage")
    parser.add_argument("--list-suites", action="store_true", help="List available suites and exit")
    parser.add_argument("--list-tokens", action="store_true", help="List available token symbols and exit")
    parser.add_argument("--verbose", action="store_true")

    args = parser.parse_args()

    try:
        chain_id = resolve_chain_id(args.chain_id)
    except ValueError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 2

    if args.list_suites:
        for name in list_suites(chain_id):
            print(name)
        return 0

    if args.list_tokens:
        for symbol, address in list_tokens(chain_id):
            print(f"{symbol} {address}")
        return 0

    try:
        pairs = parse_pairs(args.pair, args.pairs, chain_id)
        if not pairs:
            pairs = suite_pairs(args.suite, chain_id)
        amounts_override = parse_amounts(args.amounts) if args.amounts else None
    except ValueError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 2

    allowed_statuses = resolve_allowed_statuses(
        args.allow_status,
        allow_failures=args.allow_failures,
        allow_no_pools=args.allow_no_pools,
    )
    if not allowed_statuses:
        print("Error: --allow-status produced no values", file=sys.stderr)
        return 2

    expected_protocols: set[str] = set()
    if args.expect_protocols:
        expected_protocols = {p.strip().lower() for p in args.expect_protocols.split(",") if p.strip()}

    report = {
        "chain_id": chain_id,
        "chain": chain_label(chain_id),
        "url": args.url,
        "suite": args.suite,
        "pairs": [asdict(p) for p in pairs],
        "amounts": amounts_override,
        "amounts_strategy": "explicit" if amounts_override else "per_pair_or_token_defaults",
        "requests": [],
        "pools": [],
        "summary": {},
    }

    winner_protocols: Counter[str] = Counter()
    candidate_protocols: Counter[str] = Counter()
    pools_seen: dict[str, dict] = {}
    meta_statuses: Counter[str] = Counter()
    failure_kinds: Counter[str] = Counter()

    failures = 0
    allowed_no_pools = 0

    for idx, pair in enumerate(pairs, start=1):
        amounts = amounts_override or amounts_for_pair(pair.token_in, pair.token_out, chain_id)
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
            if args.allow_no_pools and status == "no_liquidity":
                only_no_pools = all(
                    isinstance(item, dict) and item.get("kind") == "no_pools"
                    for item in failures_list
                )
                if only_no_pools:
                    allowed_no_pools += 1
                    failures_list = []
                else:
                    failures += 1
                    report["requests"].append(
                        {
                            "pair": asdict(pair),
                            "elapsed_s": elapsed,
                            "meta": meta,
                            "error": "meta_failures",
                        }
                    )
                    continue
            else:
                failures += 1
                report["requests"].append(
                    {
                        "pair": asdict(pair),
                        "elapsed_s": elapsed,
                        "meta": meta,
                        "error": "meta_failures",
                    }
                )
                continue

        data = response_json.get("data", [])
        pool_count = len(data) if isinstance(data, list) else 0
        candidate_protocols.update(collect_candidate_protocols(response_json))

        if isinstance(data, list):
            for entry in data:
                if not isinstance(entry, dict):
                    continue
                pool_address = entry.get("pool_address")
                pool_name = entry.get("pool_name")
                pool_id = entry.get("pool")
                protocol = protocol_from_fields(entry.get("protocol"), pool_name).lower()
                winner_protocols[protocol] += 1
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
            {
                "pair": asdict(pair),
                "elapsed_s": elapsed,
                "meta": meta,
                "pool_count": pool_count,
                "amounts": amounts,
            }
        )

        if args.verbose:
            print(
                f"[OK] {pair.token_in}->{pair.token_out} {elapsed:.3f}s "
                f"status={status} pools={pool_count}"
            )

    observed_protocols = sorted(winner_protocols.keys())
    candidate_protocol_list = sorted(candidate_protocols.keys())
    missing_expected = sorted([p for p in expected_protocols if p not in set(candidate_protocol_list)])

    report["summary"] = {
        "pairs": len(pairs),
        "failures": failures,
        "meta_statuses": dict(meta_statuses),
        "failure_kinds": dict(failure_kinds),
        "allowed_no_pools": allowed_no_pools,
        "unique_pools": len(pools_seen),
        "observed_protocols": observed_protocols,
        "protocol_counts": dict(winner_protocols),
        "winner_protocols": observed_protocols,
        "winner_protocol_counts": dict(winner_protocols),
        "candidate_protocols": candidate_protocol_list,
        "candidate_protocol_counts": dict(candidate_protocols),
        "missing_expected_protocols": missing_expected,
    }
    report["winner_protocols"] = observed_protocols
    report["winner_protocol_counts"] = dict(winner_protocols)
    report["candidate_protocols"] = candidate_protocol_list
    report["candidate_protocol_counts"] = dict(candidate_protocols)
    report["pools"] = sorted(
        pools_seen.values(), key=lambda item: (item.get("protocol", ""), item.get("pool_name", ""))
    )

    print(f"Coverage sweep summary ({chain_label(chain_id)}:{chain_id})")
    print("=====================")
    print(f"Pairs: {len(pairs)}")
    print(f"Failures: {failures}")
    if allowed_no_pools:
        print(f"Allowed no_pools: {allowed_no_pools}")
    print(f"Unique pools observed: {len(pools_seen)}")
    print(f"Winner protocols: {', '.join(observed_protocols) if observed_protocols else '(none)'}")
    print(
        f"Candidate protocols: {', '.join(candidate_protocol_list) if candidate_protocol_list else '(none)'}"
    )
    if meta_statuses:
        status_summary = ", ".join(f"{status}={count}" for status, count in meta_statuses.items())
        print(f"Meta statuses: {status_summary}")

    if winner_protocols:
        print("\nTop winner protocols by pool appearances (top 10):")
        for protocol, count in winner_protocols.most_common(10):
            print(f"- {protocol}: {count}")

    if candidate_protocols:
        print("\nTop candidate protocols by pool appearances (top 10):")
        for protocol, count in candidate_protocols.most_common(10):
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
