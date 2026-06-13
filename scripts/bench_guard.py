#!/usr/bin/env python3
"""Benchmark regression guard. Runs 1 paired benchmark, compares against threshold."""
import sys, os, json, subprocess, re

THRESHOLD = 20  # allow up to 20% regression (2.7x variance)
HISTORY_FILE = os.path.expanduser("~/.reliary/bench-history.jsonl")

def run_bench():
    script = os.path.join(os.path.dirname(__file__), "bench_paired.py")
    r = subprocess.run([sys.executable, script, "1"], capture_output=True, timeout=360)
    out = r.stdout.decode()
    # Parse "Weighted cost change: +12.3%" or "Weighted cost change: -34.1%"
    m = re.search(r"Weighted cost change:\s*([+-]\d+\.?\d*)%", out)
    if not m:
        print(f"FAIL: could not parse delta from output:\n{out[:500]}")
        return None
    delta = float(m.group(1))
    # Record in history
    os.makedirs(os.path.dirname(HISTORY_FILE), exist_ok=True)
    with open(HISTORY_FILE, "a") as f:
        f.write(json.dumps({"delta": delta, "timestamp": int(__import__("time").time())}) + "\n")
    return delta

def main():
    print("[bench-guard] Running regression check...")
    delta = run_bench()
    if delta is None:
        print("FAIL: Benchmark failed to run")
        sys.exit(1)
    print(f"[bench-guard] Gate vs baseline: {delta:+.1f}%")
    if delta > THRESHOLD:
        print(f"[bench-guard] FAIL: Gate regressed {delta:.1f}% (threshold: {THRESHOLD}%)")
        sys.exit(1)
    elif delta < -THRESHOLD:
        print(f"[bench-guard] PASS: Gate improved {delta:.1f}%")
    else:
        print(f"[bench-guard] PASS: Gate within envelope ({delta:+.1f}%)")
    sys.exit(0)

if __name__ == "__main__":
    main()
