#!/usr/bin/env python3
import argparse
import datetime as dt
import re
import sys
import time


def parse_spec(spec: str) -> int:
    spec = spec.strip()
    if spec.lower() == "now":
        return int(time.time())

    if re.fullmatch(r"\d+", spec):
        value = int(spec)
        if len(spec) > 10:
            return value // 1000
        return value

    match = re.fullmatch(r"(\d+)([smhdw])", spec)
    if match:
        amount = int(match.group(1))
        unit = match.group(2)
        seconds = amount * {"s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}[unit]
        return int(time.time()) - seconds

    if spec.endswith("Z"):
        spec = spec[:-1] + "+00:00"

    try:
        parsed = dt.datetime.fromisoformat(spec)
    except ValueError as exc:
        print(f"Unsupported time spec: {spec}", file=sys.stderr)
        raise SystemExit(2) from exc

    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=dt.timezone.utc)

    return int(parsed.timestamp())


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("spec")
    parser.add_argument("--ms", action="store_true", help="Output epoch milliseconds")
    args = parser.parse_args()

    epoch_seconds = parse_spec(args.spec)
    if args.ms:
        print(epoch_seconds * 1000)
    else:
        print(epoch_seconds)


if __name__ == "__main__":
    main()
