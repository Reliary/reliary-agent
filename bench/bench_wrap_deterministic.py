#!/usr/bin/env python3
"""Tier 1: Deterministic compression bench for `reliary wrap`.

Measures compression ratio, exit code preservation, and wall time overhead
across 10 realistic commands. No LLM, no variance — pure mechanism proof.

Usage: python3 bench/bench_wrap_deterministic.py [--reliary PATH] [--corpus PATH]
"""
import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

RELIARY = os.environ.get("RELIARY_BIN_PATH", str(Path(__file__).resolve().parent.parent / "target/release/reliary"))

# 10 realistic commands covering different output shapes
COMMANDS = [
    ("cargo test --workspace", "cargo"),
    ("cargo build --release 2>&1", "cargo"),
    ("git status", "git"),
    ("git diff --stat", "git"),
    ("git log --oneline -20", "git"),
    ("grep -rn 'TODO\\|FIXME\\|HACK' crates/ 2>/dev/null || true", "grep"),
    ("grep -rn 'unsafe' crates/reliary-search/src/ 2>/dev/null || true", "grep"),
    ("find crates/ -name '*.rs' | head -50", "find"),
    ("ls -la crates/", "ls"),
    ("wc -l crates/reliary-search/src/*.rs", "wc"),
]

def run_raw(cmd: str, cwd: str) -> tuple:
    """Run raw command, return (exit_code, stdout_bytes, wall_seconds)."""
    start = time.monotonic()
    try:
        r = subprocess.run(
            ["bash", "-c", cmd], cwd=cwd, capture_output=True, timeout=120
        )
        elapsed = time.monotonic() - start
        return r.returncode, len(r.stdout), elapsed
    except subprocess.TimeoutExpired:
        return 124, 0, 120.0

def run_wrapped(cmd: str, cwd: str, reliary: str) -> tuple:
    """Run via `reliary wrap`, return (exit_code, stdout_bytes, wall_seconds)."""
    start = time.monotonic()
    try:
        r = subprocess.run(
            [reliary, "wrap", "bash", "-c", cmd],
            cwd=cwd, capture_output=True, timeout=120
        )
        elapsed = time.monotonic() - start
        return r.returncode, len(r.stdout), elapsed
    except subprocess.TimeoutExpired:
        return 124, 0, 120.0

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--reliary", default=RELIARY)
    parser.add_argument("--corpus", default=str(Path(__file__).resolve().parent.parent))
    args = parser.parse_args()

    reliary_bin = args.reliary

    if not Path(reliary_bin).exists():
        print(f"ERROR: reliary binary not found at {reliary_bin}", file=sys.stderr)
        sys.exit(1)

    results = []
    print(f"{'Command':<50} {'Raw(B)':>8} {'Wrap(B)':>8} {'Ratio':>7} {'ExitR':>6} {'ExitW':>6} {'RawW(s)':>8} {'WrapW(s)':>8} {'Overhead':>9}")
    print("=" * 115)

    all_exit_match = True
    all_compressed = True

    for cmd, category in COMMANDS:
        raw_exit, raw_bytes, raw_wall = run_raw(cmd, args.corpus)
        wrap_exit, wrap_bytes, wrap_wall = run_wrapped(cmd, args.corpus, reliary_bin)

        ratio = wrap_bytes / raw_bytes if raw_bytes > 0 else 1.0
        savings = (1.0 - ratio) * 100
        overhead_ms = (wrap_wall - raw_wall) * 1000
        exit_match = raw_exit == wrap_exit

        if not exit_match: all_exit_match = False
        if ratio >= 1.0 and raw_bytes > 100: all_compressed = False

        results.append({
            "command": cmd, "category": category,
            "raw_bytes": raw_bytes, "wrap_bytes": wrap_bytes,
            "ratio": round(ratio, 4), "savings_pct": round(savings, 1),
            "raw_exit": raw_exit, "wrap_exit": wrap_exit, "exit_match": exit_match,
            "raw_wall_s": round(raw_wall, 3), "wrap_wall_s": round(wrap_wall, 3),
            "overhead_ms": round(overhead_ms, 1),
        })

        print(f"{cmd[:50]:<50} {raw_bytes:>8} {wrap_bytes:>8} {savings:>6.1f}% {raw_exit:>6} {wrap_exit:>6} {raw_wall:>8.3f} {wrap_wall:>8.3f} {overhead_ms:>8.1f}ms")

    # Summary
    avg_savings = sum(r["savings_pct"] for r in results) / len(results) if results else 0
    avg_overhead = sum(r["overhead_ms"] for r in results) / len(results) if results else 0
    total_raw = sum(r["raw_bytes"] for r in results)
    total_wrap = sum(r["wrap_bytes"] for r in results)
    total_savings = (1 - total_wrap / total_raw) * 100 if total_raw > 0 else 0

    print("=" * 115)
    print(f"{'TOTAL':<50} {total_raw:>8} {total_wrap:>8} {total_savings:>6.1f}%")
    print(f"\nAvg savings: {avg_savings:.1f}%  |  Avg overhead: {avg_overhead:.1f}ms")
    print(f"Exit codes preserved: {'YES' if all_exit_match else 'NO — MISMATCH DETECTED'}")
    print(f"All outputs compressed: {'YES' if all_compressed else 'NO — some commands grew'}")

    # Write JSON for aggregation
    out_path = Path(args.corpus) / "bench/results/wrap_deterministic.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps({
        "results": results,
        "summary": {
            "total_raw_bytes": total_raw,
            "total_wrap_bytes": total_wrap,
            "total_savings_pct": round(total_savings, 1),
            "avg_savings_pct": round(avg_savings, 1),
            "avg_overhead_ms": round(avg_overhead, 1),
            "exit_codes_preserved": all_exit_match,
            "all_compressed": all_compressed,
        }
    }, indent=2))
    print(f"\nWritten: {out_path}")

if __name__ == "__main__":
    main()