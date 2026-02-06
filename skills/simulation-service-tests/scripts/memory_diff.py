#!/usr/bin/env python3
"""Append jemalloc memory diff table based on the latest memory_breakdown line."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import os
import re
import sys

REQUIRED_KEYS = [
    "baseline",
    "after_state_reset",
    "after_state_drop",
    "after_db_update",
    "after_db_reset",
]

ORDERED_KEYS = [
    "after_state_reset",
    "after_state_drop",
    "after_db_update",
    "after_db_reset",
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Append a memory diff table to service-memory-tracking.md",
    )
    parser.add_argument(
        "--repo",
        default=".",
        help="Repo root containing .codex/memory-task/service-memory-tracking.md",
    )
    parser.add_argument(
        "--file",
        help="Override the tracking markdown path",
    )
    return parser.parse_args()


def find_latest_breakdown(text: str) -> dict[str, int]:
    matches = list(re.finditer(r"memory_breakdown[^\n]*", text))
    if not matches:
        raise ValueError("no memory_breakdown line found")

    line = matches[-1].group(0)
    pairs = dict(re.findall(r"(\w+)=([0-9]+)", line))

    missing = [key for key in REQUIRED_KEYS if key not in pairs]
    if missing:
        raise ValueError(f"missing keys in memory_breakdown line: {', '.join(missing)}")

    return {key: int(value) for key, value in pairs.items()}


def format_delta(delta: int) -> str:
    sign = "+" if delta >= 0 else "-"
    return f"{sign}{abs(delta)}"


def append_table(path: str, values: dict[str, int]) -> None:
    baseline = values["baseline"]
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    lines = [
        "",
        f"## Memory diff ({timestamp})",
        f"Baseline bytes: {baseline}",
        "",
        "| metric | value_bytes | delta_from_baseline_bytes |",
        "| --- | --- | --- |",
    ]
    for key in ORDERED_KEYS:
        value = values[key]
        delta = value - baseline
        lines.append(f"| {key} | {value} | {format_delta(delta)} |")
    lines.append("")

    with open(path, "a", encoding="utf-8") as handle:
        handle.write("\n".join(lines))


def main() -> int:
    args = parse_args()
    if args.file:
        path = args.file
    else:
        path = os.path.join(
            os.path.abspath(args.repo),
            ".codex",
            "memory-task",
            "service-memory-tracking.md",
        )

    if not os.path.exists(path):
        print(f"tracking file not found: {path}", file=sys.stderr)
        return 1

    with open(path, "r", encoding="utf-8") as handle:
        contents = handle.read()

    try:
        values = find_latest_breakdown(contents)
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 1

    append_table(path, values)
    print(f"appended memory diff table to {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
