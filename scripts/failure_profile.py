#!/usr/bin/env python3
"""Profile /simulate failure kinds and status distribution under load."""

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
from pathlib import Path
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
    parser = argparse.ArgumentParser(description="Failure profiling for POST /simulate")
    parser.add_argument("--url", default="http://localhost:3000/simulate")
    parser.add_argument("--requests", type=int, default=500)
    parser.add_argument("--concurrency", type=int, default=50)
    parser.add_argument("--suite", default="core", help="Named pair suite from presets (used unless --random)")
    parser.add_argument("--random", action="store_true", help="Pick random pairs from --tokens instead of a suite")
    parser.add_argument("--pair", action="append", help="token_in:token_out (symbol or address)")
    parser.add_argument("--pairs", help="Comma-separated token_in:token_out pairs")
    parser.add_argument("--tokens", help="Comma-separated token symbols or addresses")
    parser.add_argument("--amounts", help="Comma-separated amounts in wei")
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--seed", type=int)
    parser.add_argument("--request-id-prefix", default="profile")
    parser.add_argument("--out", help="Write JSON report to this path")
    parser.add_argument("--list-suites", action="store_true", help="List available suites and exit")
    parser.add_argument("--list-tokens", action="store_true", help="List available token symbols and exit")
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
        print(
            json.dumps(
                {
                    "url": args.url,
                    "requests": args.requests,
                    "concurrency": args.concurrency,
                    "timeout": args.timeout,
                    "sample_payload": payload,
                },
                indent=2,
            )
        )
        return 0

    start_time = time.perf_counter()
    response_times = []
    http_failures = 0
    json_failures = 0
    error_reasons: dict[str, int] = {}
    status_counts: dict[str, int] = {}
    failure_kinds: dict[str, int] = {}

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
            return status_code, body, elapsed, None
        except HTTPError as exc:
            return None, None, None, f"http:{exc.code}"
        except URLError as exc:
            return None, None, None, f"url:{exc.reason}"
        except Exception as exc:  # pragma: no cover - unexpected errors
            return None, None, None, f"error:{exc}"

    with ThreadPoolExecutor(max_workers=args.concurrency) as executor:
        futures = [executor.submit(worker, idx) for idx in range(args.requests)]
        for future in as_completed(futures):
            status_code, body, elapsed, error = future.result()
            if error is not None:
                error_reasons[error] = error_reasons.get(error, 0) + 1
                continue
            if status_code != 200:
                http_failures += 1
                error_reasons[f"http:{status_code}"] = error_reasons.get(f"http:{status_code}", 0) + 1
                continue

            response_times.append(elapsed)

            try:
                response_json = json.loads(body)
            except json.JSONDecodeError:
                json_failures += 1
                continue

            meta = response_json.get("meta", {})
            status = str(meta.get("status", "unknown"))
            status_counts[status] = status_counts.get(status, 0) + 1

            failures_list = meta.get("failures", [])
            if isinstance(failures_list, list):
                for item in failures_list:
                    if not isinstance(item, dict):
                        continue
                    kind = item.get("kind")
                    if kind is None:
                        continue
                    kind = str(kind)
                    failure_kinds[kind] = failure_kinds.get(kind, 0) + 1

    total_time = time.perf_counter() - start_time

    if not response_times:
        print("No successful requests.")
        print(f"Errors: {sum(error_reasons.values()) + http_failures + json_failures}")
        return 1

    response_times.sort()
    avg = statistics.mean(response_times)
    p50 = percentile(response_times, 0.50)
    p90 = percentile(response_times, 0.90)
    p99 = percentile(response_times, 0.99)

    print("Failure profile (HTTP 200 responses)")
    print("====================================")
    print(f"Requests: {args.requests}")
    print(f"Successes: {len(response_times)}")
    print(f"HTTP failures: {http_failures}")
    print(f"JSON failures: {json_failures}")
    print(f"Other errors: {sum(error_reasons.values())}")
    print(f"Total wall time: {total_time:.4f}")
    print(f"Average: {avg:.4f}")
    print(f"p50: {p50:.4f}")
    print(f"p90: {p90:.4f}")
    print(f"p99: {p99:.4f}")
    print(f"Min: {response_times[0]:.4f}")
    print(f"Max: {response_times[-1]:.4f}")

    throughput = len(response_times) / total_time if total_time > 0 else 0.0
    print(f"Throughput: {throughput:.2f} req/s")

    if status_counts:
        print("\nmeta.status distribution:")
        for status, count in sorted(status_counts.items(), key=lambda kv: kv[1], reverse=True):
            print(f"- {status}: {count}")

    if failure_kinds:
        print("\nFailure kinds (top 10):")
        for kind, count in sorted(failure_kinds.items(), key=lambda kv: kv[1], reverse=True)[:10]:
            print(f"- {kind}: {count}")

    if error_reasons:
        print("\nError reasons (top 8):")
        for reason, count in sorted(error_reasons.items(), key=lambda kv: kv[1], reverse=True)[:8]:
            print(f"- {reason}: {count}")

    if args.out:
        report = {
            "url": args.url,
            "requests": args.requests,
            "concurrency": args.concurrency,
            "suite": args.suite,
            "random": args.random,
            "status_counts": status_counts,
            "failure_kinds": failure_kinds,
            "http_failures": http_failures,
            "json_failures": json_failures,
            "error_reasons": error_reasons,
            "latency": {
                "avg": avg,
                "p50": p50,
                "p90": p90,
                "p99": p99,
                "min": response_times[0],
                "max": response_times[-1],
            },
        }
        out_path = Path(args.out).expanduser().resolve()
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(report, indent=2))
        print(f"\nWrote report: {out_path}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
