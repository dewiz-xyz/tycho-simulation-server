#!/usr/bin/env python3
"""Latency percentile runner for POST /simulate (p50/p90/p99)."""

from __future__ import annotations

import argparse
import json
import math
import random
import statistics
import sys
import time
import uuid
from concurrent.futures import ThreadPoolExecutor, as_completed
from urllib import request
from urllib.error import HTTPError, URLError

from presets import (
    TOKENS,
    default_amounts_for_token,
    list_suites,
    list_tokens,
    parse_amounts,
    parse_pairs,
    resolve_token,
    suite_pairs,
)


def percentile(sorted_values, pct):
    if not sorted_values:
        return 0.0
    if len(sorted_values) == 1:
        return sorted_values[0]
    idx = (len(sorted_values) - 1) * pct
    lower = math.floor(idx)
    upper = math.ceil(idx)
    if lower == upper:
        return sorted_values[int(idx)]
    weight = idx - lower
    return sorted_values[lower] + (sorted_values[upper] - sorted_values[lower]) * weight


def request_simulate(url, payload, timeout):
    data = json.dumps(payload).encode("utf-8")
    req = request.Request(url, data=data, headers={"Content-Type": "application/json"})
    start = time.perf_counter()
    with request.urlopen(req, timeout=timeout) as response:
        body = response.read()
        elapsed = time.perf_counter() - start
        return response.status, body, elapsed


def parse_tokens(tokens_csv: str | None) -> list[str]:
    if not tokens_csv:
        return list(TOKENS.values())
    tokens = [token.strip() for token in tokens_csv.split(",") if token.strip()]
    if len(tokens) < 2:
        raise ValueError("Provide at least two tokens")
    return [resolve_token(token) for token in tokens]


def main() -> int:
    parser = argparse.ArgumentParser(description="Latency percentiles for POST /simulate")
    parser.add_argument("--url", default="http://localhost:3000/simulate")
    parser.add_argument("--requests", type=int, default=200)
    parser.add_argument("--concurrency", type=int, default=50)
    parser.add_argument("--suite", default="core", help="Named pair suite from presets (used unless --random)")
    parser.add_argument("--random", action="store_true", help="Pick random pairs from --tokens instead of a suite")
    parser.add_argument("--pair", action="append", help="token_in:token_out (symbol or address)")
    parser.add_argument("--pairs", help="Comma-separated token_in:token_out pairs")
    parser.add_argument("--tokens", help="Comma-separated token symbols or addresses")
    parser.add_argument("--amounts", help="Comma-separated amounts in wei")
    parser.add_argument("--allow-status", default="ready", help="Comma-separated allowed meta.status values")
    parser.add_argument("--allow-failures", action="store_true", help="Allow meta.failures to be non-empty")
    parser.add_argument("--list-suites", action="store_true", help="List available suites and exit")
    parser.add_argument("--list-tokens", action="store_true", help="List available token symbols and exit")
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--seed", type=int)
    parser.add_argument("--request-id-prefix", default="latency")
    parser.add_argument("--dry-run", action="store_true", help="Print config without sending")

    args = parser.parse_args()

    if args.list_suites:
        for name in list_suites():
            print(name)
        return 0

    if args.list_tokens:
        for symbol, address in list_tokens():
            print(f"{symbol} {address}")
        return 0

    if args.requests < 1:
        print("Error: --requests must be >= 1", file=sys.stderr)
        return 2
    if args.concurrency < 1:
        print("Error: --concurrency must be >= 1", file=sys.stderr)
        return 2

    if args.seed is not None:
        random.seed(args.seed)

    try:
        explicit_pairs = parse_pairs(args.pair, args.pairs)
        amounts_override = parse_amounts(args.amounts) if args.amounts else None
        tokens = parse_tokens(args.tokens)
    except ValueError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 2

    allowed_statuses = {s.strip() for s in args.allow_status.split(",") if s.strip()}
    if not allowed_statuses:
        print("Error: --allow-status produced no values", file=sys.stderr)
        return 2

    if args.random and len(tokens) < 2:
        print("Error: need at least two tokens to pick random pairs", file=sys.stderr)
        return 2

    pairs = []
    if explicit_pairs:
        pairs = explicit_pairs
    elif not args.random:
        try:
            pairs = suite_pairs(args.suite)
        except ValueError as exc:
            print(f"Error: {exc}", file=sys.stderr)
            return 2

    def pick_pair():
        if pairs:
            pair = random.choice(pairs)
            return pair.token_in, pair.token_out
        return tuple(random.sample(tokens, 2))

    def amounts_for_token(token_in: str) -> list[str]:
        return amounts_override or default_amounts_for_token(token_in)

    if args.dry_run:
        sample_pair = pick_pair()
        payload = {
            "request_id": f"{args.request_id_prefix}-sample",
            "token_in": sample_pair[0],
            "token_out": sample_pair[1],
            "amounts": amounts_for_token(sample_pair[0]),
        }
        print("Dry run configuration:")
        print(json.dumps(
            {
                "url": args.url,
                "requests": args.requests,
                "concurrency": args.concurrency,
                "timeout": args.timeout,
                "sample_payload": payload,
            },
            indent=2,
        ))
        return 0

    start_time = time.perf_counter()
    response_times = []
    failures = 0
    failure_reasons: dict[str, int] = {}

    def worker(index):
        token_in, token_out = pick_pair()
        payload = {
            "request_id": f"{args.request_id_prefix}-{index}-{uuid.uuid4().hex[:8]}",
            "token_in": token_in,
            "token_out": token_out,
            "amounts": amounts_for_token(token_in),
        }
        try:
            status_code, body, elapsed = request_simulate(args.url, payload, args.timeout)
            if status_code != 200:
                return None, f"HTTP {status_code}"
            try:
                response_json = json.loads(body)
            except json.JSONDecodeError:
                return None, "invalid_json"

            meta = response_json.get("meta", {})
            status = meta.get("status")
            failures_list = meta.get("failures", [])

            if status not in allowed_statuses:
                return None, f"meta_status:{status}"

            if failures_list and not args.allow_failures:
                return None, "meta_failures"

            return elapsed, None
        except HTTPError as exc:
            return None, f"HTTP {exc.code}"
        except URLError as exc:
            return None, str(exc.reason)
        except Exception as exc:  # pragma: no cover - unexpected errors
            return None, str(exc)

    with ThreadPoolExecutor(max_workers=args.concurrency) as executor:
        futures = [executor.submit(worker, idx) for idx in range(args.requests)]
        for future in as_completed(futures):
            elapsed, error = future.result()
            if elapsed is None:
                failures += 1
                if error:
                    failure_reasons[error] = failure_reasons.get(error, 0) + 1
            else:
                response_times.append(elapsed)

    total_time = time.perf_counter() - start_time

    if not response_times:
        print("No successful requests.")
        print(f"Failures: {failures}")
        return 1

    response_times.sort()
    avg = statistics.mean(response_times)
    p50 = percentile(response_times, 0.50)
    p90 = percentile(response_times, 0.90)
    p99 = percentile(response_times, 0.99)

    print("Latency results (seconds)")
    print("========================")
    print(f"Requests: {args.requests}")
    print(f"Successes: {len(response_times)}")
    print(f"Failures: {failures}")
    print(f"Total wall time: {total_time:.4f}")
    print(f"Average: {avg:.4f}")
    print(f"p50: {p50:.4f}")
    print(f"p90: {p90:.4f}")
    print(f"p99: {p99:.4f}")
    print(f"Min: {response_times[0]:.4f}")
    print(f"Max: {response_times[-1]:.4f}")

    throughput = len(response_times) / total_time if total_time > 0 else 0.0
    print(f"Throughput: {throughput:.2f} req/s")

    if failure_reasons:
        print("\nFailure reasons (top 8):")
        for reason, count in sorted(failure_reasons.items(), key=lambda kv: kv[1], reverse=True)[:8]:
            print(f"- {reason}: {count}")

    return 0 if failures == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
