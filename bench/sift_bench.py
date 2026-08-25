#!/usr/bin/env python3
"""V14: Sift compression benchmark on real-world corpus.

Measures how well sift compresses representative bash command outputs.
RTK achieves 60-90% savings on these patterns. We compare against that
target.

Usage:
    python3 bench/sift_bench.py [--bin /path/to/reliary]
"""

import argparse
import subprocess
import sys
from pathlib import Path

CORPUS_DIR = Path(__file__).parent / "sift_corpus"

# RTK baseline targets (from RTK README). We use the lower bound as a
# reasonable floor — the upper bound requires command-specific knowledge.
RTK_TARGET_SAVINGS = {
    "cargo_test_pass.txt": (80, 95),     # rtk cargo test: -90%
    "git_diff.txt":        (70, 85),     # rtk git diff: -75%
    "git_status.txt":      (70, 90),     # rtk git status: -80%
    "pytest_pass.txt":     (80, 95),     # rtk pytest: -90%
    "pytest_fail.txt":     (50, 80),     # mixed — errors must stay
    "compiler_error.txt":  (0, 30),      # errors MUST stay verbatim
}


def run_sift(bin_path: Path, fixture: Path) -> str:
    """Run `reliary sift --stdin` on fixture contents."""
    with open(fixture, "rb") as f:
        raw = f.read()
    proc = subprocess.run(
        [str(bin_path), "sift", "--stdin"],
        input=raw,
        capture_output=True,
        timeout=10,
    )
    return proc.stdout.decode("utf-8", errors="replace")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin", default=None,
                        help="Path to reliary binary (default: ./target/release/reliary)")
    args = parser.parse_args()

    bin_path = Path(args.bin) if args.bin else Path(__file__).parent.parent / "target/release/reliary"
    if not bin_path.exists():
        print(f"Binary not found: {bin_path}", file=sys.stderr)
        sys.exit(1)

    fixtures = sorted(CORPUS_DIR.glob("*.txt"))
    if not fixtures:
        print(f"No fixtures in {CORPUS_DIR}", file=sys.stderr)
        sys.exit(1)

    # Header
    print(f"{'Fixture':<25} {'Orig':>7} {'Comp':>7} {'Saved':>7} {'RTK Target':>14}")
    print("-" * 65)

    total_orig = 0
    total_comp = 0
    failures = []

    for fix in fixtures:
        raw = fix.read_text()
        original_len = len(raw)

        try:
            compressed = run_sift(bin_path, fix)
        except subprocess.TimeoutExpired:
            print(f"{fix.name:<25} TIMEOUT")
            failures.append((fix.name, "timeout"))
            continue

        compressed_len = len(compressed)
        saved = 100 - (compressed_len * 100 / original_len) if original_len else 0
        target = RTK_TARGET_SAVINGS.get(fix.name, (None, None))
        target_str = f"{target[0]}-{target[1]}%" if target[0] is not None else "n/a"

        print(f"{fix.name:<25} {original_len:>7} {compressed_len:>7} {saved:>6.1f}% {target_str:>14}")

        total_orig += original_len
        total_comp += compressed_len

        # Verify determinism — run twice, expect byte-identical
        try:
            second = run_sift(bin_path, fix)
            if second != compressed:
                print(f"  ⚠ NON-DETERMINISTIC output!")
                failures.append((fix.name, "non-deterministic"))
        except subprocess.TimeoutExpired:
            pass

        # Verify RTK floor (lower bound of savings target)
        if target[0] is not None and saved < target[0]:
            print(f"  ⚠ Below RTK floor ({target[0]}%)")
            failures.append((fix.name, f"below floor: {saved:.1f}% < {target[0]}%"))

    print("-" * 65)
    if total_orig:
        avg_saved = 100 - (total_comp * 100 / total_orig)
        print(f"{'TOTAL':<25} {total_orig:>7} {total_comp:>7} {avg_saved:>6.1f}%")

    if failures:
        print(f"\n{len(failures)} failure(s):")
        for name, reason in failures:
            print(f"  - {name}: {reason}")
        sys.exit(1)
    else:
        print("\nAll fixtures pass determinism + RTK floor checks.")
        sys.exit(0)


if __name__ == "__main__":
    main()