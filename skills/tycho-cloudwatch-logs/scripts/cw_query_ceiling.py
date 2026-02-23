#!/usr/bin/env python3
"""Run CloudWatch query presets across time windows with safety ceilings.

This script is meant for high-volume presets where a single query's 10k row limit
is too small for representative reporting. It executes one preset across many
non-overlapping windows, concatenates results, and stops when any safety ceiling
is hit.
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

from cw_time import parse_spec


@dataclass
class Ceilings:
    max_rows: int
    max_scanned_bytes: float
    max_queries: int


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--preset", required=True, help="cw_query.zsh preset name")
    parser.add_argument("--since", default="24h", help="Window start spec (for example 24h, 2d, ISO-8601)")
    parser.add_argument("--until", default="now", help="Window end spec")
    parser.add_argument("--window-seconds", type=int, default=900, help="Per-query window size in seconds")
    parser.add_argument("--limit", type=int, default=10000, help="Per-query row limit (cw API max is 10000)")
    parser.add_argument("--max-rows", type=int, default=6_000_000, help="Stop after this many combined rows")
    parser.add_argument(
        "--max-scanned-gb",
        type=float,
        default=60.0,
        help="Stop once combined bytesScanned crosses this many GB (decimal)",
    )
    parser.add_argument("--max-queries", type=int, default=1200, help="Stop after this many sub-queries")
    parser.add_argument("--log-group", default="", help="Optional log group override")
    parser.add_argument("--verbose", action="store_true", help="Print progress to stderr")
    return parser.parse_args()


def run_window_query(
    script_dir: Path,
    preset: str,
    start_s: int,
    end_s: int,
    limit: int,
    log_group: str,
) -> dict:
    cmd = [
        "zsh",
        str(script_dir / "cw_query.zsh"),
        "--preset",
        preset,
        "--since",
        str(start_s),
        "--until",
        str(end_s),
        "--limit",
        str(limit),
    ]
    if log_group:
        cmd.extend(["--log-group", log_group])

    completed = subprocess.run(cmd, text=True, capture_output=True)
    if completed.returncode != 0:
        raise RuntimeError(
            f"Window query failed ({start_s}-{end_s})\nSTDOUT:\n{completed.stdout}\nSTDERR:\n{completed.stderr}"
        )

    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            f"Failed to parse cw_query output for window {start_s}-{end_s}: {exc}\n"
            f"STDOUT prefix:\n{completed.stdout[:2000]}"
        ) from exc


def window_ranges(start_s: int, end_s: int, window_seconds: int) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    cursor = start_s
    while cursor < end_s:
        nxt = min(end_s, cursor + window_seconds)
        ranges.append((cursor, nxt))
        cursor = nxt
    return ranges


def update_stats(acc: dict[str, float], stats: dict) -> None:
    for key in (
        "recordsMatched",
        "recordsScanned",
        "estimatedRecordsSkipped",
        "bytesScanned",
        "estimatedBytesSkipped",
        "logGroupsScanned",
    ):
        value = stats.get(key, 0)
        try:
            acc[key] = acc.get(key, 0.0) + float(value)
        except (TypeError, ValueError):
            continue


def main() -> None:
    args = parse_args()
    script_dir = Path(__file__).resolve().parent

    start_s = parse_spec(args.since)
    end_s = parse_spec(args.until)
    if start_s >= end_s:
        raise SystemExit("--since must be earlier than --until")
    if args.window_seconds <= 0:
        raise SystemExit("--window-seconds must be > 0")
    if args.limit <= 0 or args.limit > 10000:
        raise SystemExit("--limit must be in [1, 10000]")

    ceilings = Ceilings(
        max_rows=max(1, args.max_rows),
        max_scanned_bytes=max(0.0, args.max_scanned_gb) * 1_000_000_000.0,
        max_queries=max(1, args.max_queries),
    )

    windows = window_ranges(start_s, end_s, args.window_seconds)
    total_windows = len(windows)

    combined_results: list = []
    combined_stats: dict[str, float] = {}
    query_language = "CWLI"
    rows_returned = 0
    queries_executed = 0
    capped_windows = 0
    truncated = False
    truncated_reason = ""

    for idx, (w_start, w_end) in enumerate(windows, start=1):
        if queries_executed >= ceilings.max_queries:
            truncated = True
            truncated_reason = "max_queries"
            break

        payload = run_window_query(
            script_dir=script_dir,
            preset=args.preset,
            start_s=w_start,
            end_s=w_end,
            limit=args.limit,
            log_group=args.log_group,
        )
        queries_executed += 1
        query_language = payload.get("queryLanguage", query_language)

        stats = payload.get("statistics", {}) if isinstance(payload, dict) else {}
        update_stats(combined_stats, stats)

        rows = payload.get("results", []) if isinstance(payload, dict) else []
        if len(rows) >= args.limit:
            capped_windows += 1

        remaining = ceilings.max_rows - rows_returned
        if remaining <= 0:
            truncated = True
            truncated_reason = "max_rows"
            break

        if len(rows) > remaining:
            combined_results.extend(rows[:remaining])
            rows_returned += remaining
            truncated = True
            truncated_reason = "max_rows"
            break

        combined_results.extend(rows)
        rows_returned += len(rows)

        bytes_scanned = combined_stats.get("bytesScanned", 0.0)
        if ceilings.max_scanned_bytes > 0 and bytes_scanned >= ceilings.max_scanned_bytes:
            truncated = True
            truncated_reason = "max_scanned_gb"
            break

        if args.verbose and (idx == 1 or idx == total_windows or idx % 25 == 0):
            print(
                f"[{idx}/{total_windows}] rows={rows_returned:,} scanned_gb={bytes_scanned / 1e9:.2f} capped_windows={capped_windows}",
                file=sys.stderr,
            )

    if capped_windows > 0 and not truncated:
        # We completed all windows, but at least one hit the per-query row cap.
        # Treat as truncated coverage and make the reason explicit.
        truncated = True
        truncated_reason = "window_row_cap"

    output = {
        "queryLanguage": query_language,
        "results": combined_results,
        "statistics": combined_stats,
        "status": "Complete",
        "meta": {
            "pagination": {
                "preset": args.preset,
                "since": args.since,
                "until": args.until,
                "start_epoch_s": start_s,
                "end_epoch_s": end_s,
                "window_seconds": args.window_seconds,
                "windows_planned": total_windows,
                "queries_executed": queries_executed,
                "rows_returned": rows_returned,
                "max_rows": ceilings.max_rows,
                "max_scanned_gb": args.max_scanned_gb,
                "max_queries": ceilings.max_queries,
                "capped_windows": capped_windows,
                "truncated": truncated,
                "truncated_reason": truncated_reason,
                "estimated_cost_usd_eu_central_1": round(
                    (combined_stats.get("bytesScanned", 0.0) / 1_000_000_000.0) * 0.0063, 4
                ),
            }
        },
    }

    json.dump(output, sys.stdout, separators=(",", ":"))
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
