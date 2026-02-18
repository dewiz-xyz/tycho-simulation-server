#!/usr/bin/env python3
"""Analyze Tycho CloudWatch snapshot query outputs and render a markdown report."""

from __future__ import annotations

import argparse
import json
import math
import re
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path
from statistics import mean


TOKEN_MAP = {
    "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2": "WETH",
    "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48": "USDC",
    "0xdac17f958d2ee523a2206206994597c13d831ec7": "USDT",
    "0x6b175474e89094c44da98b954eedeac495271d0f": "DAI",
    "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599": "WBTC",
    "0xf939e0a03fb07f59a73314e73794be0e57ac1b4e": "crvUSD",
    "0x7f39c581f595b53c5cb19bd0b3f8da6c935e2ca0": "wstETH",
    "0xae78736cd615f374d3085123a210448e74fc6393": "rETH",
    "0xbe9895146f7af43049ca1c1ae358b0541ea49704": "cbETH",
    "0xd5f7838f5c461feff7fe49ea5ebaf7728bb0adfa": "mETH",
    "0xa35b1b31ce002fbf2058d22f30f95d405200a15b": "ETHx",
    "0xf951e335afb289353dc249e82926178eac7ded78": "swETH",
    "0xac3e018457b222d93114458476f3e3416abbe38f": "sfrxETH",
    "0x9d39a5de30e57443bff2a8307a4256c8797a3497": "sUSDe",
    "0x4c9edd5852cd905f086c759e8383e09bff1e68b3": "USDe",
    "0x83f20f44975d03b1b09e64809b757c47f942beea": "sDAI",
    "0xbf5495efe5db9ce00f80364c8b423567e58d2110": "ezETH",
    "0xcd5fe23c85820f7b72d0926fc9b05b43e359b7ee": "weETH",
    "0xfe18be6b3bd88a2d2a7f928d00292e7a9879067f": "sBTC",
    "0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf": "cbBTC",
    "0x8236a87084f8b84306f72007f36f2618a5634494": "LBTC",
    "0x853d955acef822db058eb8505911ed77f175b99e": "FRAX",
    "0x45804880de22913dafe09f4980848ece6ecbaf78": "PAXG",
}

ADDRESS_RE = re.compile(r"0x[a-fA-F0-9]{40}")


def to_int(value: str | None, default: int = 0) -> int:
    if value is None or value == "":
        return default
    try:
        return int(float(value))
    except (TypeError, ValueError):
        return default


def to_float(value: str | None, default: float = 0.0) -> float:
    if value is None or value == "":
        return default
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def fmt_int(value: int | float) -> str:
    return f"{int(round(value)):,}"


def fmt_float(value: float, digits: int = 2) -> str:
    return f"{value:.{digits}f}"


def short_addr(address: str) -> str:
    clean = address.lower()
    if clean in TOKEN_MAP:
        return TOKEN_MAP[clean]
    return f"{clean[:10]}..."


def resolve_text(text: str) -> str:
    if not text:
        return "unknown"

    def repl(match: re.Match[str]) -> str:
        return short_addr(match.group(0))

    return ADDRESS_RE.sub(repl, text)


def resolve_token(address: str | None) -> str:
    if not address:
        return "unknown"
    return short_addr(address)


def percentile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    sorted_vals = sorted(values)
    if len(sorted_vals) == 1:
        return sorted_vals[0]
    rank = (len(sorted_vals) - 1) * (p / 100.0)
    lower = math.floor(rank)
    upper = math.ceil(rank)
    if lower == upper:
        return sorted_vals[lower]
    fraction = rank - lower
    return sorted_vals[lower] + (sorted_vals[upper] - sorted_vals[lower]) * fraction


def stats(values: list[float]) -> dict[str, float]:
    if not values:
        return {
            "min": 0.0,
            "p10": 0.0,
            "p25": 0.0,
            "p50": 0.0,
            "mean": 0.0,
            "p75": 0.0,
            "p90": 0.0,
            "p95": 0.0,
            "p99": 0.0,
            "max": 0.0,
        }
    return {
        "min": min(values),
        "p10": percentile(values, 10),
        "p25": percentile(values, 25),
        "p50": percentile(values, 50),
        "mean": mean(values),
        "p75": percentile(values, 75),
        "p90": percentile(values, 90),
        "p95": percentile(values, 95),
        "p99": percentile(values, 99),
        "max": max(values),
    }


def hbar(value: float, max_value: float, width: int = 28, fill: str = "█") -> str:
    if max_value <= 0:
        filled = 0
    else:
        filled = int(round((value / max_value) * width))
    filled = max(0, min(width, filled))
    return (fill * filled) + (" " * (width - filled))


def downsample(values: list[float], max_points: int = 48) -> list[float]:
    if len(values) <= max_points:
        return values[:]
    chunk = len(values) / max_points
    sampled = []
    for i in range(max_points):
        start = int(i * chunk)
        end = int((i + 1) * chunk)
        end = max(start + 1, end)
        sampled.append(mean(values[start:end]))
    return sampled


def sparkline(values: list[float], max_points: int = 48) -> str:
    if not values:
        return "(no data)"
    chars = "▁▂▃▄▅▆▇█"
    sampled = downsample(values, max_points=max_points)
    low, high = min(sampled), max(sampled)
    if math.isclose(low, high):
        return "▅" * len(sampled)
    out = []
    for value in sampled:
        idx = int(round((value - low) / (high - low) * (len(chars) - 1)))
        out.append(chars[idx])
    return "".join(out)


def summary_box(lines: list[str]) -> str:
    width = max((len(line) for line in lines), default=0) + 2
    top = f"  ┌{'─' * width}┐"
    body = [f"  │ {line.ljust(width - 1)}│" for line in lines]
    bottom = f"  └{'─' * width}┘"
    return "\n".join([top, *body, bottom])


def row_to_dict(row: list[dict[str, str]] | dict[str, str]) -> dict[str, str]:
    if isinstance(row, dict):
        return {str(k): str(v) for k, v in row.items()}
    output: dict[str, str] = {}
    for item in row:
        field = item.get("field")
        value = item.get("value")
        if field is not None:
            output[str(field)] = "" if value is None else str(value)
    return output


def load_query(path: Path) -> tuple[dict, list[dict[str, str]]]:
    data = json.loads(path.read_text())
    rows = [row_to_dict(row) for row in data.get("results", [])]
    return data, rows


def parse_minute(value: str) -> str:
    for fmt in ("%Y-%m-%d %H:%M:%S.%f", "%Y-%m-%d %H:%M:%S"):
        try:
            return datetime.strptime(value, fmt).strftime("%H:%M")
        except ValueError:
            continue
    return value[-8:-3] if len(value) >= 5 else value


def format_timestamp_utc(value: str | None) -> str:
    if not value:
        return "unknown"
    normalized = value.strip()
    if normalized.endswith("Z"):
        normalized = normalized[:-1] + "+00:00"
    try:
        parsed = datetime.fromisoformat(normalized)
    except ValueError:
        return value
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")


def fenced_text(block: str | list[str]) -> str:
    text = block if isinstance(block, str) else "\n".join(block)
    return f"```text\n{text}\n```"


def render_distribution_compact(label: str, values: list[float]) -> list[str]:
    s = stats(values)
    return [
        f"{label:<10} min {fmt_int(s['min']):>6}  p25 {fmt_int(s['p25']):>6}  p50 {fmt_int(s['p50']):>6}  mean {fmt_float(s['mean'], 1):>7}",
        f"{'':<10} p75 {fmt_int(s['p75']):>6}  p90 {fmt_int(s['p90']):>6}  p99 {fmt_int(s['p99']):>6}  max  {fmt_int(s['max']):>6}",
    ]


# Known Curve pool name prefixes (these don't use the protocol::pair format)
_VM_CURVE_PREFIXES = (
    "crvUSD", "Trycrypto", "TricryptoUSD", "tricrypto", "3pool", "steth",
    "USDe", "FRAX", "sUSDe", "fraxusdc", "Curve", "lusd", "PayPool",
)


def extract_protocol(pool_name: str) -> str:
    if not pool_name:
        return "unknown"
    pool = resolve_text(pool_name)
    if "::" in pool:
        return pool.split("::", 1)[0]
    # Classify bare pool names from VM protocols
    for prefix in _VM_CURVE_PREFIXES:
        if pool.startswith(prefix):
            return "vm:curve"
    # Fallback: first word-like token, truncated
    match = re.match(r"[A-Za-z0-9_+.-]+", pool)
    return (match.group(0) if match else pool)[:20]


def render_distribution(label: str, values: list[float], percentiles: list[str]) -> str:
    s = stats(values)
    cols = [f"{p}: {fmt_int(s[p])}" if p != "mean" else f"mean: {fmt_float(s['mean'], 1)}" for p in percentiles]
    return f"{label:<12} " + "  ".join(cols)


def mk_pair(token_in: str | None, token_out: str | None) -> str:
    return f"{resolve_token(token_in)}/{resolve_token(token_out)}"


def build_report(input_dir: Path, period: str) -> str:
    runs_raw, runs_rows = load_query(input_dir / "simulate-runs.json")
    completions_raw, completions_rows = load_query(input_dir / "simulate-completions.json")
    _, rpm_rows = load_query(input_dir / "simulate-runs-per-minute.json")
    _, runs_auction_rows = load_query(input_dir / "simulate-runs-per-auction.json")
    _, requests_auction_rows = load_query(input_dir / "simulate-requests-per-auction.json")
    metrics = json.loads((input_dir / "cw-metrics.json").read_text())

    requests_per_minute = [to_int(row.get("requests")) for row in rpm_rows]
    runs_per_minute = [to_int(row.get("total_runs")) for row in rpm_rows]
    minutes = [parse_minute(row.get("minute", "")) for row in rpm_rows]

    total_requests = sum(requests_per_minute)
    total_runs = sum(runs_per_minute)
    unique_auctions = len({row.get("auction_id", "") for row in requests_auction_rows if row.get("auction_id")})
    runs_per_request = (total_runs / total_requests) if total_requests else 0.0

    run_count_values = [to_int(row.get("simulation_runs")) for row in runs_rows]
    native_pool_values = [to_int(row.get("scheduled_native_pools")) for row in runs_rows]
    vm_pool_values = [to_int(row.get("scheduled_vm_pools")) for row in runs_rows]
    requests_with_vm = sum(1 for value in vm_pool_values if value > 0)
    sample_request_count = len(run_count_values)

    total_native_pools = sum(native_pool_values)
    total_vm_pools = sum(vm_pool_values)
    total_scheduled_pools = total_native_pools + total_vm_pools
    native_share = (total_native_pools / total_scheduled_pools * 100) if total_scheduled_pools else 0.0
    vm_share = (total_vm_pools / total_scheduled_pools * 100) if total_scheduled_pools else 0.0
    vm_request_share = (requests_with_vm / sample_request_count * 100) if sample_request_count else 0.0

    latency_values = [to_int(row.get("latency_ms")) for row in completions_rows if row.get("latency_ms")]
    latency_by_status: dict[str, list[int]] = defaultdict(list)
    latency_by_bucket: dict[str, list[int]] = defaultdict(list)
    failures_by_status: dict[str, int] = defaultdict(int)
    status_counts: Counter[str] = Counter()

    for row in completions_rows:
        status = row.get("status") or "Unknown"
        latency = to_int(row.get("latency_ms"))
        failures = to_int(row.get("failures"))
        scheduled = to_int(row.get("scheduled_native_pools")) + to_int(row.get("scheduled_vm_pools"))
        status_counts[status] += 1
        failures_by_status[status] += failures
        if latency:
            latency_by_status[status].append(latency)
            if scheduled <= 5:
                latency_by_bucket["1-5"].append(latency)
            elif scheduled <= 10:
                latency_by_bucket["6-10"].append(latency)
            elif scheduled <= 20:
                latency_by_bucket["11-20"].append(latency)
            elif scheduled <= 40:
                latency_by_bucket["21-40"].append(latency)
            else:
                latency_by_bucket["41+"].append(latency)

    pool_counter: Counter[str] = Counter()
    protocol_counter: Counter[str] = Counter()
    for row in completions_rows:
        pool_name = resolve_text(row.get("top_pool_name", ""))
        if pool_name and pool_name != "unknown":
            pool_counter[pool_name] += 1
            protocol_counter[extract_protocol(pool_name)] += 1

    pair_counter: Counter[str] = Counter()
    for row in completions_rows:
        pair_counter[mk_pair(row.get("token_in"), row.get("token_out"))] += 1

    runs_per_auction = [to_int(row.get("total_runs")) for row in runs_auction_rows]
    ratio_per_auction = []
    top_auctions_rows = []
    for row in runs_auction_rows:
        req = to_int(row.get("requests"))
        runs = to_int(row.get("total_runs"))
        ratio = (runs / req) if req else 0.0
        ratio_per_auction.append(ratio)
        top_auctions_rows.append((row.get("auction_id", "unknown"), runs, req, ratio))
    top_auctions_rows.sort(key=lambda item: item[1], reverse=True)

    cpu_series = [
        to_float(item.get("avg"))
        for item in metrics.get("series", [])
        if item.get("metric") == "TaskCpuUtilization"
    ]
    mem_series = [
        to_float(item.get("avg"))
        for item in metrics.get("series", [])
        if item.get("metric") == "TaskMemoryUtilization"
    ]

    cpu_summary_rows = []
    mem_summary_rows = []
    for item in metrics.get("summary", []):
        metric = item.get("metric")
        if item.get("min") in ("-", None) or item.get("mean") in ("-", None) or item.get("max") in ("-", None):
            continue
        parsed = {
            "task_id": item.get("task_id", "unknown"),
            "min": to_float(item.get("min")),
            "mean": to_float(item.get("mean")),
            "max": to_float(item.get("max")),
        }
        if metric == "TaskCpuUtilization":
            cpu_summary_rows.append(parsed)
        elif metric == "TaskMemoryUtilization":
            mem_summary_rows.append(parsed)

    def agg_metric(rows: list[dict[str, float]]) -> dict[str, float]:
        if not rows:
            return {"min": 0.0, "mean": 0.0, "p90": 0.0, "max": 0.0}
        means = [row["mean"] for row in rows]
        mins = [row["min"] for row in rows]
        maxes = [row["max"] for row in rows]
        return {
            "min": min(mins),
            "mean": mean(means),
            "p90": percentile(means, 90),
            "max": max(maxes),
        }

    cpu_agg = agg_metric(cpu_summary_rows)
    mem_agg = agg_metric(mem_summary_rows)

    high_cpu_tasks = [row for row in cpu_summary_rows if row["min"] > 80.0]
    high_mem_tasks = [row for row in mem_summary_rows if row["min"] > 85.0]

    memory_monotonic = all(mem_series[i] >= mem_series[i - 1] - 0.05 for i in range(1, len(mem_series)))
    memory_growth = (mem_series[-1] - mem_series[0]) if len(mem_series) >= 2 else 0.0
    memory_leak_signal = memory_monotonic and memory_growth > 1.0

    runs_minute_stats = stats([float(v) for v in runs_per_minute])
    req_minute_stats = stats([float(v) for v in requests_per_minute])
    runs_per_request_stats = stats([float(v) for v in run_count_values])
    runs_auction_stats = stats([float(v) for v in runs_per_auction])
    ratio_auction_stats = stats(ratio_per_auction)
    latency_stats = stats([float(v) for v in latency_values])

    protocol_total = sum(protocol_counter.values())
    top_protocols = protocol_counter.most_common()
    top_pools = pool_counter.most_common(20)
    top_pairs = pair_counter.most_common(10)

    # Ensure maverick doesn't disappear in any non-zero sample.
    if any(proto != "vm:maverick_v2" for proto, _ in top_protocols):
        maverick_wins = protocol_counter.get("vm:maverick_v2", 0)
        if maverick_wins:
            top_protocols.append(("vm:maverick_v2", maverick_wins))
            protocol_order = [protocol for protocol, _ in top_protocols]
            dedup_protocols: list[tuple[str, int]] = []
            seen_protocols = set()
            for proto, wins in top_protocols:
                if proto in seen_protocols:
                    continue
                seen_protocols.add(proto)
                dedup_protocols.append((proto, wins))
            top_protocols = dedup_protocols

    # Keep protocol report stable and inclusive: show all winners sorted by count.
    top_protocols = sorted(top_protocols, key=lambda item: item[1], reverse=True)

    max_protocol = top_protocols[0][1] if top_protocols else 0
    max_pair = top_pairs[0][1] if top_pairs else 0
    max_bucket_p95 = 0.0
    bucket_rows = []
    for bucket in ["1-5", "6-10", "11-20", "21-40", "41+"]:
        values = latency_by_bucket.get(bucket, [])
        if not values:
            bucket_rows.append((bucket, 0, 0.0, 0.0))
            continue
        p50 = percentile([float(v) for v in values], 50)
        p95 = percentile([float(v) for v in values], 95)
        max_bucket_p95 = max(max_bucket_p95, p95)
        bucket_rows.append((bucket, len(values), p50, p95))

    rendered_bucket_rows = []
    for bucket, count, p50, p95 in bucket_rows:
        bar = hbar(p95, max_bucket_p95, width=24)
        rendered_bucket_rows.append(
            f"| {bucket:>5} | {fmt_int(count):>6} | {fmt_int(p50):>5} | {fmt_int(p95):>5} | {bar} |"
        )

    status_total = sum(status_counts.values())
    ready_count = status_counts.get("Ready", 0)
    # Accept both labels so snapshots remain accurate across log schema variants.
    partial_count = status_counts.get("PartialSuccess", 0) + status_counts.get("PartialFailure", 0)
    other_count = max(0, status_total - ready_count - partial_count)
    status_bar_width = 48
    if status_total:
        ready_share = ready_count / status_total * 100
        partial_share = partial_count / status_total * 100
        ready_w = int(round(ready_share / 100 * status_bar_width))
        partial_w = int(round(partial_share / 100 * status_bar_width))
        if ready_w + partial_w > status_bar_width:
            partial_w = max(0, status_bar_width - ready_w)
        status_bar = ("█" * ready_w) + ("░" * partial_w) + (" " * (status_bar_width - ready_w - partial_w))
    else:
        ready_share = 0.0
        partial_share = 0.0
        status_bar = " " * status_bar_width

    other_status_parts = [
        f"{status}={fmt_int(count)}"
        for status, count in status_counts.most_common()
        if status not in ("Ready", "PartialSuccess", "PartialFailure")
    ]

    coverage_notes = []
    runs_matched = to_int(str(runs_raw.get("statistics", {}).get("recordsMatched", 0)))
    completions_matched = to_int(str(completions_raw.get("statistics", {}).get("recordsMatched", 0)))
    if runs_matched > len(runs_rows):
        coverage_notes.append(
            f"`simulate-runs` returned {fmt_int(len(runs_rows))} rows out of {fmt_int(runs_matched)} matched (CloudWatch row cap)."
        )
    if completions_matched > len(completions_rows):
        coverage_notes.append(
            f"`simulate-completions` returned {fmt_int(len(completions_rows))} rows out of {fmt_int(completions_matched)} matched (CloudWatch row cap)."
        )

    run_lines = []
    if minutes:
        idxs = list(range(len(minutes)))
        if len(idxs) > 16:
            step = len(idxs) / 16
            idxs = [min(len(minutes) - 1, int(i * step)) for i in range(16)]
            idxs = sorted(set(idxs))
        max_runs_minute = max(runs_per_minute) if runs_per_minute else 0
        for idx in idxs:
            run_lines.append(
                f"{minutes[idx]}  {hbar(runs_per_minute[idx], max_runs_minute, width=30)}  {fmt_int(runs_per_minute[idx])}"
            )

    protocol_lines = []
    for protocol, wins in top_protocols:
        pct = (wins / protocol_total * 100) if protocol_total else 0.0
        protocol_lines.append(f"  {protocol:<20s}  {hbar(wins, max_protocol, width=28):<28s}  {pct:5.1f}%")

    pair_lines = []
    for pair, count in top_pairs:
        pct = (count / (sum(pair_counter.values()) or 1) * 100)
        pair_lines.append(f"  {pair:<20s}  {hbar(count, max_pair, width=28):<28s}  {pct:5.1f}%")

    status_latency_lines = []
    max_status_p95 = 0.0
    status_latency_stats = []
    for status, values in sorted(latency_by_status.items(), key=lambda item: len(item[1]), reverse=True):
        if not values:
            continue
        p50 = percentile([float(v) for v in values], 50)
        p95 = percentile([float(v) for v in values], 95)
        max_status_p95 = max(max_status_p95, p95)
        status_latency_stats.append((status, len(values), p50, p95))
    for status, count, p50, p95 in status_latency_stats:
        status_latency_lines.append(
            f"{status:<16} p50 {fmt_int(p50):>5} ms, p95 {fmt_int(p95):>5} ms, n={fmt_int(count):>6}  {hbar(p95, max_status_p95, width=16)}"
        )

    window_since = format_timestamp_utc(metrics.get("meta", {}).get("since"))
    window_until = format_timestamp_utc(metrics.get("meta", {}).get("until"))

    split_width = 40
    native_w = int(round(native_share / 100 * split_width)) if total_scheduled_pools else 0
    native_w = max(0, min(split_width, native_w))
    vm_w = split_width - native_w
    native_vm_bar = "[" + ("█" * native_w) + ("▓" * vm_w) + "]"

    latency_ladder_values = [
        fmt_int(latency_stats["min"]),
        fmt_int(latency_stats["p50"]),
        fmt_int(latency_stats["p75"]),
        fmt_int(latency_stats["p90"]),
        fmt_int(latency_stats["p95"]),
        fmt_int(latency_stats["p99"]),
        fmt_int(latency_stats["max"]),
    ]
    latency_ladder = "  " + "─────".join(latency_ladder_values)
    latency_ladder_labels = "        p50      p75      p90      p95      p99      max"

    report_lines: list[str] = []
    report_lines.append(f"# Tycho Simulation Production Snapshot ({period})")
    report_lines.append("")
    report_lines.append(f"Window: {window_since} to {window_until}.")
    if coverage_notes:
        report_lines.append("")
        report_lines.append("Coverage notes:")
        for note in coverage_notes:
            report_lines.append(f"- {note}")

    report_lines.append("")
    report_lines.append("## 1. Volume at a glance")
    report_lines.append("")
    report_lines.append(
        fenced_text(
            summary_box(
                [
                    f"Requests: {fmt_int(total_requests)}  |  Simulation runs: {fmt_int(total_runs)}",
                    f"Runs/request: {fmt_float(runs_per_request, 2)}  |  Unique auctions: {fmt_int(unique_auctions)}",
                    f"Per-request sample size for deep stats: {fmt_int(sample_request_count)}",
                ]
            )
        )
    )

    report_lines.append("")
    report_lines.append("## 2. Throughput over time")
    report_lines.append("")
    throughput_block = [
        f"Runs/min sparkline      {sparkline([float(v) for v in runs_per_minute], max_points=48)}",
        f"Requests/min sparkline  {sparkline([float(v) for v in requests_per_minute], max_points=48)}",
        "",
        "Runs per minute bars:",
        *(run_lines if run_lines else ["(no per-minute data)"]),
    ]
    report_lines.append(fenced_text(throughput_block))
    report_lines.append("")
    report_lines.append(
        fenced_text(
            summary_box(
                [
                    *render_distribution_compact("Runs/min", [float(v) for v in runs_per_minute]),
                    *render_distribution_compact("Reqs/min", [float(v) for v in requests_per_minute]),
                ]
            )
        )
    )

    report_lines.append("")
    report_lines.append("## 3. Protocol win rates")
    report_lines.append("")
    report_lines.append(fenced_text(protocol_lines if protocol_lines else ["No top pool winners in sample."]))

    report_lines.append("")
    report_lines.append("## 4. Top winning pools")
    report_lines.append("")
    report_lines.append("| Rank | Pool | Wins | Share |")
    report_lines.append("| ---: | :--- | ---: | ---: |")
    for idx, (pool, wins) in enumerate(top_pools, start=1):
        share = (wins / (sum(pool_counter.values()) or 1) * 100)
        report_lines.append(f"| {idx} | {pool} | {fmt_int(wins)} | {share:.1f}% |")
    if not top_pools:
        report_lines.append("| 1 | No data | 0 | 0.0% |")

    report_lines.append("")
    report_lines.append("## 5. Pools per request")
    report_lines.append("")
    report_lines.append(
        fenced_text(
            summary_box(
                [
                    render_distribution(
                        "sim_runs",
                        [float(v) for v in run_count_values],
                        ["p10", "p25", "p50", "p75", "p90", "p95", "max"],
                    ),
                    f"Native scheduled pools: {fmt_int(total_native_pools)} ({native_share:.1f}%)",
                    f"VM scheduled pools:     {fmt_int(total_vm_pools)} ({vm_share:.1f}%)",
                    f"Requests with VM pools: {fmt_int(requests_with_vm)} / {fmt_int(sample_request_count)} ({vm_request_share:.1f}%)",
                ]
            )
        )
    )
    report_lines.append("")
    report_lines.append(
        fenced_text(
            [
                f"Native vs VM split {native_vm_bar}",
                f"(native {native_share:.1f}%, vm {vm_share:.1f}%)",
            ]
        )
    )

    report_lines.append("")
    report_lines.append("## 6. Latency")
    report_lines.append("")
    report_lines.append(fenced_text([latency_ladder, latency_ladder_labels]))
    report_lines.append("")
    report_lines.append(
        fenced_text(
            summary_box(
                [
                    f"Latency ms  p50: {fmt_int(latency_stats['p50'])}  p75: {fmt_int(latency_stats['p75'])}  p90: {fmt_int(latency_stats['p90'])}",
                    f"            p95: {fmt_int(latency_stats['p95'])}  p99: {fmt_int(latency_stats['p99'])}  max: {fmt_int(latency_stats['max'])}",
                ]
            )
        )
    )
    report_lines.append("")
    report_lines.append("Latency by pool-count bucket:")
    report_lines.append("| Bucket | Count | p50ms | p95ms | p95 bar |")
    report_lines.append("| ---: | ---: | ---: | ---: | :--- |")
    report_lines.extend(rendered_bucket_rows)
    report_lines.append("")
    report_lines.append("Latency by status:")
    report_lines.append(fenced_text(status_latency_lines if status_latency_lines else ["No latency data by status."]))

    report_lines.append("")
    report_lines.append("## 7. Reliability")
    report_lines.append("")
    reliability_lines = [
        f"  [{status_bar}]",
        f"   Ready: {ready_share:5.1f}% ({fmt_int(ready_count)})          Partial: {partial_share:5.1f}% ({fmt_int(partial_count)})",
    ]
    if other_status_parts:
        reliability_lines.append(f"   Other: {', '.join(other_status_parts)}")
    report_lines.append(fenced_text(reliability_lines))

    failure_lines = [
        f"Total failures reported: {fmt_int(sum(failures_by_status.values()))}",
        f"Non-ready requests in sample: {fmt_int(status_total - ready_count)} / {fmt_int(status_total)}",
    ]
    for status, count in status_counts.most_common():
        if status != "Ready":
            failure_lines.append(
                f"{status}: {fmt_int(count)} requests, {fmt_int(failures_by_status.get(status, 0))} failures"
            )
    report_lines.append(fenced_text(summary_box(failure_lines)))

    report_lines.append("")
    report_lines.append("## 8. Auction structure")
    report_lines.append("")
    auctions_per_min = (unique_auctions / len(rpm_rows)) if rpm_rows else 0.0
    report_lines.append(
        fenced_text(
            summary_box(
                [
                    f"Auction cadence: {fmt_float(auctions_per_min, 2)} unique auctions/minute",
                    *render_distribution_compact("runs/auction", [float(v) for v in runs_per_auction]),
                    f"Runs/request per auction p50: {fmt_float(ratio_auction_stats['p50'], 2)}, p90: {fmt_float(ratio_auction_stats['p90'], 2)}, max: {fmt_float(ratio_auction_stats['max'], 2)}",
                ]
            )
        )
    )
    report_lines.append("")
    report_lines.append("Top auctions by simulation runs:")
    report_lines.append("| Auction | Runs | Requests | Runs/Req |")
    report_lines.append("| :--- | ---: | ---: | ---: |")
    for auction_id, runs, req, ratio in top_auctions_rows[:10]:
        report_lines.append(f"| {auction_id} | {fmt_int(runs)} | {fmt_int(req)} | {ratio:.2f} |")

    report_lines.append("")
    report_lines.append("## 9. Token pair traffic")
    report_lines.append("")
    report_lines.append(fenced_text(pair_lines if pair_lines else ["No token pair data."]))

    report_lines.append("")
    report_lines.append("## 10. ECS Resource Utilization")
    report_lines.append("")
    report_lines.append(
        fenced_text(
            summary_box(
                [
                    f"CPU%    min: {fmt_float(cpu_agg['min'], 2)}  mean: {fmt_float(cpu_agg['mean'], 2)}  p90: {fmt_float(cpu_agg['p90'], 2)}  max: {fmt_float(cpu_agg['max'], 2)}",
                    f"Memory% min: {fmt_float(mem_agg['min'], 2)}  mean: {fmt_float(mem_agg['mean'], 2)}  p90: {fmt_float(mem_agg['p90'], 2)}  max: {fmt_float(mem_agg['max'], 2)}",
                ]
            )
        )
    )
    report_lines.append("")
    report_lines.append(
        fenced_text(
            [
                f"Fleet avg CPU (5m):    {sparkline(cpu_series, max_points=48)}",
                f"Fleet avg Memory (5m): {sparkline(mem_series, max_points=48)}",
                f"{window_since} -> {window_until}",
            ]
        )
    )
    report_lines.append("")
    if high_cpu_tasks or high_mem_tasks:
        report_lines.append("Sustained high-utilization tasks:")
        for row in high_cpu_tasks:
            report_lines.append(
                f"- CPU >80% sustained: task {row['task_id']} (min {row['min']:.2f}%, mean {row['mean']:.2f}%, max {row['max']:.2f}%)"
            )
        for row in high_mem_tasks:
            report_lines.append(
                f"- Memory >85% sustained: task {row['task_id']} (min {row['min']:.2f}%, mean {row['mean']:.2f}%, max {row['max']:.2f}%)"
            )
    else:
        report_lines.append("Sustained >80% CPU or >85% memory tasks: none detected.")
    leak_note = (
        f"Memory trend: monotonic growth detected (+{memory_growth:.2f}%), possible leak signal."
        if memory_leak_signal
        else f"Memory trend: stable, no monotonic leak pattern (+{memory_growth:.2f}% from start to end)."
    )
    report_lines.append(leak_note)

    report_lines.append("")
    report_lines.append("## 11. TLDR")
    report_lines.append("")
    top_protocol = top_protocols[0][0] if top_protocols else "n/a"
    top_protocol_share = (
        f"{(top_protocols[0][1] / protocol_total * 100):.1f}%" if top_protocols and protocol_total else "0.0%"
    )
    top_pair = top_pairs[0][0] if top_pairs else "n/a"
    report_lines.append(
        f"Traffic in this {period} window was {fmt_int(total_requests)} requests and {fmt_int(total_runs)} simulation runs, averaging {runs_per_request:.2f} runs per request across {fmt_int(unique_auctions)} auctions."
    )
    report_lines.append(
        f"Throughput was bursty, runs/min ranged from {fmt_int(runs_minute_stats['min'])} to {fmt_int(runs_minute_stats['max'])} with p50 {fmt_int(runs_minute_stats['p50'])}, and request/min p50 was {fmt_int(req_minute_stats['p50'])}."
    )
    report_lines.append(
        f"Latency stayed tight with p50 {fmt_int(latency_stats['p50'])} ms and p99 {fmt_int(latency_stats['p99'])} ms (max {fmt_int(latency_stats['max'])} ms), while status mix was Ready {ready_share:.1f}% and PartialSuccess {partial_share:.1f}%."
    )
    report_lines.append(
        f"Top quote wins were led by {top_protocol} at {top_protocol_share}, and the busiest token pair in the sample was {top_pair}."
    )
    if high_cpu_tasks or high_mem_tasks:
        utilization_signals = []
        if high_cpu_tasks:
            utilization_signals.append(f"{fmt_int(len(high_cpu_tasks))} task(s) with sustained CPU >80%")
        if high_mem_tasks:
            utilization_signals.append(f"{fmt_int(len(high_mem_tasks))} task(s) with sustained memory >85%")
        utilization_tldr = "; ".join(utilization_signals)
    else:
        utilization_tldr = "no sustained high-utilization tasks"

    memory_tldr = (
        f"monotonic memory growth signal observed (+{memory_growth:.2f}% from start to end)"
        if memory_leak_signal
        else f"no monotonic leak pattern (+{memory_growth:.2f}% from start to end)"
    )
    report_lines.append(
        f"ECS utilization averaged CPU {fmt_float(cpu_agg['mean'], 2)}% and memory {fmt_float(mem_agg['mean'], 2)}%, with {utilization_tldr}; {memory_tldr}."
    )

    return "\n".join(report_lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input-dir", required=True, type=Path, help="Directory with CloudWatch JSON files")
    parser.add_argument("--period", required=True, help="Period label (for example 30m, 12h)")
    parser.add_argument("--output", required=True, type=Path, help="Output markdown path")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    report = build_report(args.input_dir, args.period)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(report)
    print(str(args.output))


if __name__ == "__main__":
    main()
