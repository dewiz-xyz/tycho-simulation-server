#!/usr/bin/env python3
"""Check Curve pools against tycho-simulation-server /simulate responses.

Workflow:
1) Fetch all Ethereum mainnet pools from Curve API.
2) For each pool, choose a token pair from the pool's coin addresses.
3) Call POST /simulate with a 1-wei amount.
4) Verify whether the Curve pool address appears in returned pool candidates.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import uuid
from concurrent.futures import Future, ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib import request
from urllib.error import HTTPError, URLError

HEX_ADDRESS_RE = re.compile(r"^0x[a-fA-F0-9]{40}$")
ZERO_ADDRESS = "0x0000000000000000000000000000000000000000"
ETH_SENTINEL_ADDRESS = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
DEFAULT_CURVE_URL = "https://api.curve.finance/api/getPools/ethereum/main"
FACTORY_STABLE_NG_URL = "https://api.curve.finance/api/getPools/ethereum/factory-stable-ng"
DEFAULT_HTTP_HEADERS = {
    "Accept": "application/json",
    # Curve API may reject Python's default urllib user-agent with 403.
    "User-Agent": "Mozilla/5.0 (compatible; tycho-simulation-check/1.0)",
}


@dataclass(frozen=True)
class CurvePoolCase:
    index: int
    source: str
    curve_id: str
    curve_name: str
    curve_address: str
    token_in: str
    token_out: str


@dataclass(frozen=True)
class CaseResult:
    case: CurvePoolCase
    ok: bool
    elapsed_s: float | None
    status: str
    matched: bool
    returned_pools: int
    error: str | None



def normalize_address(value: str | None) -> str | None:
    if not isinstance(value, str):
        return None
    value = value.strip()
    if not HEX_ADDRESS_RE.match(value):
        return None
    return value.lower()



def is_zero_address(value: str) -> bool:
    return value.lower() == ZERO_ADDRESS



def normalize_token_address(value: str | None) -> str | None:
    addr = normalize_address(value)
    if not addr:
        return None
    # Curve API uses 0xEeee... sentinel for native ETH in some pools.
    if addr == ETH_SENTINEL_ADDRESS:
        return ZERO_ADDRESS
    return addr



def request_json(url: str, timeout: float) -> dict[str, Any]:
    req = request.Request(url, headers=DEFAULT_HTTP_HEADERS)
    with request.urlopen(req, timeout=timeout) as response:
        body = response.read()
    return json.loads(body)



def post_simulate(url: str, payload: dict[str, Any], timeout: float) -> tuple[int, dict[str, Any], float]:
    data = json.dumps(payload).encode("utf-8")
    headers = dict(DEFAULT_HTTP_HEADERS)
    headers["Content-Type"] = "application/json"
    req = request.Request(
        url,
        data=data,
        headers=headers,
    )
    started = time.perf_counter()
    with request.urlopen(req, timeout=timeout) as response:
        body = response.read()
        elapsed = time.perf_counter() - started
    return response.status, json.loads(body), elapsed



def extract_curve_address(pool: dict[str, Any]) -> str | None:
    # Prefer explicit pool address; fall back to id only when it is a hex address.
    direct = normalize_address(pool.get("address"))
    if direct:
        return direct
    return normalize_address(pool.get("id"))



def extract_pool_tokens(pool: dict[str, Any], allow_zero: bool) -> list[str]:
    tokens: list[str] = []
    seen: set[str] = set()

    for raw in pool.get("coinsAddresses", []):
        addr = normalize_token_address(raw)
        if not addr:
            continue
        if not allow_zero and is_zero_address(addr):
            continue
        if addr in seen:
            continue
        seen.add(addr)
        tokens.append(addr)

    # Fallback to nested coins[] if needed.
    if len(tokens) < 2:
        for coin in pool.get("coins", []):
            if not isinstance(coin, dict):
                continue
            addr = normalize_token_address(coin.get("address"))
            if not addr:
                continue
            if not allow_zero and is_zero_address(addr):
                continue
            if addr in seen:
                continue
            seen.add(addr)
            tokens.append(addr)

    return tokens



def parse_curve_cases(
    payload: dict[str, Any], allow_zero: bool, source: str
) -> tuple[list[CurvePoolCase], list[str]]:
    data = payload.get("data")
    if not isinstance(data, dict):
        raise ValueError("Curve response missing object field: data")

    pool_data = data.get("poolData")
    if not isinstance(pool_data, list):
        raise ValueError("Curve response missing array field: data.poolData")

    cases: list[CurvePoolCase] = []
    skipped: list[str] = []

    for idx, pool in enumerate(pool_data, start=1):
        if not isinstance(pool, dict):
            skipped.append(f"#{idx}: poolData entry is not an object")
            continue

        curve_id = str(pool.get("id", ""))
        curve_name = str(pool.get("name", ""))

        curve_address = extract_curve_address(pool)
        if not curve_address:
            skipped.append(f"#{idx} id={curve_id}: missing/invalid address")
            continue

        tokens = extract_pool_tokens(pool, allow_zero=allow_zero)
        if len(tokens) < 2:
            skipped.append(f"#{idx} id={curve_id} address={curve_address}: <2 usable token addresses")
            continue

        cases.append(
            CurvePoolCase(
                index=idx,
                source=source,
                curve_id=curve_id,
                curve_name=curve_name,
                curve_address=curve_address,
                token_in=tokens[0],
                token_out=tokens[1],
            )
        )

    return cases, skipped



def simulate_case(
    case: CurvePoolCase,
    simulate_url: str,
    timeout: float,
    amount: str,
    request_id_prefix: str,
    allowed_statuses: set[str],
) -> CaseResult:
    payload = {
        "request_id": f"{request_id_prefix}-{case.index}-{uuid.uuid4().hex[:8]}",
        "token_in": case.token_in,
        "token_out": case.token_out,
        "amounts": [amount],
    }

    try:
        status_code, body, elapsed = post_simulate(simulate_url, payload, timeout)
    except HTTPError as exc:
        return CaseResult(
            case=case,
            ok=False,
            elapsed_s=None,
            status=f"http_error:{exc.code}",
            matched=False,
            returned_pools=0,
            error=f"HTTP error: {exc.code}",
        )
    except URLError as exc:
        return CaseResult(
            case=case,
            ok=False,
            elapsed_s=None,
            status="url_error",
            matched=False,
            returned_pools=0,
            error=f"URL error: {exc.reason}",
        )
    except json.JSONDecodeError as exc:
        return CaseResult(
            case=case,
            ok=False,
            elapsed_s=None,
            status="invalid_json",
            matched=False,
            returned_pools=0,
            error=f"Invalid JSON: {exc}",
        )
    except Exception as exc:  # pragma: no cover
        return CaseResult(
            case=case,
            ok=False,
            elapsed_s=None,
            status="request_error",
            matched=False,
            returned_pools=0,
            error=str(exc),
        )

    if status_code != 200:
        return CaseResult(
            case=case,
            ok=False,
            elapsed_s=elapsed,
            status=f"http_status:{status_code}",
            matched=False,
            returned_pools=0,
            error=f"Unexpected HTTP status: {status_code}",
        )

    meta = body.get("meta", {}) if isinstance(body, dict) else {}
    simulate_status = str(meta.get("status", "unknown"))
    data = body.get("data", []) if isinstance(body, dict) else []

    if not isinstance(data, list):
        return CaseResult(
            case=case,
            ok=False,
            elapsed_s=elapsed,
            status=simulate_status,
            matched=False,
            returned_pools=0,
            error="Response field 'data' is not an array",
        )

    addresses_seen: set[str] = set()
    for entry in data:
        if not isinstance(entry, dict):
            continue
        for key in ("pool_address", "pool"):
            addr = normalize_address(entry.get(key))
            if addr:
                addresses_seen.add(addr)

    matched = case.curve_address in addresses_seen

    # "ok" means request succeeded and status is acceptable for comparison.
    ok = simulate_status in allowed_statuses

    return CaseResult(
        case=case,
        ok=ok,
        elapsed_s=elapsed,
        status=simulate_status,
        matched=matched,
        returned_pools=len(data),
        error=None if ok else f"Disallowed meta.status={simulate_status}",
    )



def main() -> int:
    parser = argparse.ArgumentParser(
        description="Check whether Curve pools appear in tycho /simulate results",
    )
    parser.add_argument(
        "--curve-url",
        action="append",
        help=(
            "Curve API endpoint. Can be provided multiple times. "
            f"Defaults to: {DEFAULT_CURVE_URL}"
        ),
    )
    parser.add_argument(
        "--include-factory-stable-ng",
        action="store_true",
        help=(
            "Include factory stable-ng pools "
            f"({FACTORY_STABLE_NG_URL}) in addition to --curve-url values"
        ),
    )
    parser.add_argument(
        "--simulate-url",
        default="http://localhost:3000/simulate",
        help="tycho-simulation-server /simulate endpoint",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=20.0,
        help="HTTP timeout in seconds for each request",
    )
    parser.add_argument(
        "--amount",
        default="1",
        help="Single amount in wei to send in /simulate amounts array",
    )
    parser.add_argument(
        "--max-pools",
        type=int,
        help="Process only the first N parsed pools",
    )
    parser.add_argument(
        "--concurrency",
        type=int,
        default=4,
        help="Number of concurrent /simulate calls",
    )
    parser.add_argument(
        "--allow-status",
        default="ready,partial_failure",
        help="Comma-separated allowed meta.status values",
    )
    parser.add_argument(
        "--allow-zero-address",
        action="store_true",
        help="Allow 0x000... token as token_in/token_out candidate",
    )
    parser.add_argument(
        "--request-id-prefix",
        default="curve-check",
        help="Request id prefix",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Fetch and parse Curve pools, but skip /simulate calls",
    )
    parser.add_argument(
        "--out",
        help="Write detailed JSON report to this file",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Print per-pool details including names",
    )

    args = parser.parse_args()

    if args.concurrency < 1:
        print("Error: --concurrency must be >= 1", file=sys.stderr)
        return 2

    allowed_statuses = {value.strip() for value in args.allow_status.split(",") if value.strip()}
    if not allowed_statuses:
        print("Error: --allow-status produced no values", file=sys.stderr)
        return 2

    curve_urls = args.curve_url[:] if args.curve_url else [DEFAULT_CURVE_URL]
    if args.include_factory_stable_ng and FACTORY_STABLE_NG_URL not in curve_urls:
        curve_urls.append(FACTORY_STABLE_NG_URL)

    all_cases: list[CurvePoolCase] = []
    skipped: list[str] = []
    total_curve_pools = 0

    for curve_url in curve_urls:
        try:
            curve_payload = request_json(curve_url, args.timeout)
        except HTTPError as exc:
            print(f"Error fetching Curve API ({curve_url}): HTTP {exc.code}", file=sys.stderr)
            return 1
        except URLError as exc:
            print(f"Error fetching Curve API ({curve_url}): {exc.reason}", file=sys.stderr)
            return 1
        except json.JSONDecodeError as exc:
            print(f"Error: Curve API returned invalid JSON ({curve_url}): {exc}", file=sys.stderr)
            return 1
        except Exception as exc:
            print(f"Error fetching Curve API ({curve_url}): {exc}", file=sys.stderr)
            return 1

        try:
            source_cases, source_skipped = parse_curve_cases(
                curve_payload,
                allow_zero=args.allow_zero_address,
                source=curve_url,
            )
        except ValueError as exc:
            print(f"Error parsing Curve API payload ({curve_url}): {exc}", file=sys.stderr)
            return 1

        total_curve_pools += (
            len(curve_payload.get("data", {}).get("poolData", []))
            if isinstance(curve_payload, dict)
            else 0
        )
        all_cases.extend(source_cases)
        skipped.extend([f"{curve_url} :: {item}" for item in source_skipped])

    # De-duplicate by address when reading multiple endpoints.
    deduped: dict[str, CurvePoolCase] = {}
    for case in all_cases:
        deduped.setdefault(case.curve_address, case)

    cases = sorted(
        deduped.values(),
        key=lambda case: (case.curve_address, case.source, case.curve_id),
    )
    # Re-index after merge/dedupe for stable request ids and readable logs.
    cases = [
        CurvePoolCase(
            index=i,
            source=case.source,
            curve_id=case.curve_id,
            curve_name=case.curve_name,
            curve_address=case.curve_address,
            token_in=case.token_in,
            token_out=case.token_out,
        )
        for i, case in enumerate(cases, start=1)
    ]

    if args.max_pools is not None:
        cases = cases[: args.max_pools]

    print("Curve pool simulation check")
    print("===========================")
    print("Curve endpoints:")
    for curve_url in curve_urls:
        print(f"- {curve_url}")
    print(f"Simulate endpoint: {args.simulate_url}")
    print(f"Total pools in Curve payload: {total_curve_pools}")
    print(f"Parsed pools with token pairs: {len(all_cases)}")
    print(f"Unique parsed pools by address: {len(cases)}")
    print(f"Skipped during parsing: {len(skipped)}")

    if skipped and args.verbose:
        print("\nSkipped examples:")
        for line in skipped[:20]:
            print(f"- {line}")
        if len(skipped) > 20:
            print(f"- ... and {len(skipped) - 20} more")

    if not cases:
        print("No eligible pools to process.")
        return 1

    if args.dry_run:
        print("\nDry run enabled. First 10 parsed pool cases:")
        for case in cases[:10]:
            print(
                f"- #{case.index} id={case.curve_id} address={case.curve_address} "
                f"pair={case.token_in}->{case.token_out}"
            )
        if len(cases) > 10:
            print(f"- ... and {len(cases) - 10} more")
        return 0

    results: list[CaseResult] = []

    with ThreadPoolExecutor(max_workers=args.concurrency) as executor:
        futures: dict[Future[CaseResult], CurvePoolCase] = {
            executor.submit(
                simulate_case,
                case,
                args.simulate_url,
                args.timeout,
                args.amount,
                args.request_id_prefix,
                allowed_statuses,
            ): case
            for case in cases
        }

        for future in as_completed(futures):
            result = future.result()
            results.append(result)
            case = result.case

            label = f"#{case.index} id={case.curve_id}"
            if result.error:
                print(f"[FAIL] {label} status={result.status} err={result.error}")
            else:
                hit = "hit" if result.matched else "miss"
                elapsed = f"{result.elapsed_s:.3f}s" if result.elapsed_s is not None else "n/a"
                if args.verbose:
                    print(
                        f"[OK] {label} {hit} pools={result.returned_pools} "
                        f"status={result.status} elapsed={elapsed} "
                        f"name={case.curve_name} source={case.source}"
                    )
                else:
                    print(
                        f"[OK] {label} {hit} pools={result.returned_pools} "
                        f"status={result.status} elapsed={elapsed}"
                    )

    total = len(results)
    request_failures = sum(1 for r in results if r.error is not None)
    status_not_allowed = sum(1 for r in results if r.error and r.status not in {"http_error", "url_error"})
    matched = sum(1 for r in results if r.error is None and r.matched)
    missed = sum(1 for r in results if r.error is None and not r.matched)

    print("\nSummary")
    print("=======")
    print(f"Processed pools: {total}")
    print(f"Request/parse failures: {request_failures}")
    print(f"Disallowed statuses: {status_not_allowed}")
    print(f"Matched pools: {matched}")
    print(f"Missed pools: {missed}")

    if args.out:
        payload = {
            "curve_urls": curve_urls,
            "simulate_url": args.simulate_url,
            "total_curve_pools": total_curve_pools,
            "parsed_cases_raw": len(all_cases),
            "parsed_cases": len(cases),
            "skipped_parse": skipped,
            "results": [
                {
                    "index": r.case.index,
                    "source": r.case.source,
                    "curve_id": r.case.curve_id,
                    "curve_name": r.case.curve_name,
                    "curve_address": r.case.curve_address,
                    "token_in": r.case.token_in,
                    "token_out": r.case.token_out,
                    "status": r.status,
                    "matched": r.matched,
                    "returned_pools": r.returned_pools,
                    "elapsed_s": r.elapsed_s,
                    "error": r.error,
                }
                for r in sorted(results, key=lambda item: item.case.index)
            ],
            "summary": {
                "processed": total,
                "request_failures": request_failures,
                "status_not_allowed": status_not_allowed,
                "matched": matched,
                "missed": missed,
            },
        }

        out_path = Path(args.out).expanduser().resolve()
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(payload, indent=2))
        print(f"Report written: {out_path}")

    # Non-zero exit when there are runtime errors or misses.
    if request_failures > 0 or missed > 0:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
