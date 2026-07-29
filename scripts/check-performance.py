#!/usr/bin/env python3
"""Fail CI when the deterministic performance smoke profile crosses guardrails."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path


def limit(name: str, default: float) -> float:
    return float(os.environ.get(name, default))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("result", type=Path)
    parser.add_argument(
        "--limits",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "benchmarks/event-store-smoke-limits.json",
    )
    args = parser.parse_args()
    result = json.loads(args.result.read_text(encoding="utf-8"))
    limits = json.loads(args.limits.read_text(encoding="utf-8"))
    minimum_throughput = limit("PERF_MIN_THROUGHPUT_OPS", limits["minThroughputOpsPerSecond"])
    maximum_p99 = limit("PERF_MAX_APPEND_P99_US", limits["maxAppendP99Micros"])
    maximum_list = limit("PERF_MAX_LIST_PAGES_MS", limits["maxListPagesElapsedMs"])
    maximum_rss = limit("PERF_MAX_PEAK_RSS_BYTES", limits["maxPeakRssBytes"])

    checks = [
        (
            result["throughputOpsPerSecond"] >= minimum_throughput,
            "throughputOpsPerSecond",
            result["throughputOpsPerSecond"],
            f">= {minimum_throughput:g}",
        ),
        (
            result["appendLatencyMicros"]["p99"] <= maximum_p99,
            "appendLatencyMicros.p99",
            result["appendLatencyMicros"]["p99"],
            f"<= {maximum_p99:g}",
        ),
        (
            result["listPagesElapsedMs"] <= maximum_list,
            "listPagesElapsedMs",
            result["listPagesElapsedMs"],
            f"<= {maximum_list:g}",
        ),
    ]
    peak_rss = result.get("peakRssBytes")
    if peak_rss is not None:
        checks.append(
            (
                peak_rss <= maximum_rss,
                "peakRssBytes",
                peak_rss,
                f"<= {maximum_rss:g}",
            )
        )

    failed = [f"{name}={actual} (expected {expected})" for ok, name, actual, expected in checks if not ok]
    if failed:
        raise SystemExit("performance smoke guard failed: " + "; ".join(failed))
    print("performance smoke guard passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
