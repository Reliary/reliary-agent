#!/bin/bash
# run_all.sh — run all benches in sequence
# Usage: ./bench/run_all.sh [--check]

set -e
cd "$(dirname "$0")/.."

BIN="./target/release/reliary"
PIPELINE="/tmp/pipeline"

echo "=== reliary bench suite ==="
echo "binary: $BIN"
echo ""

if [ "$1" = "--check" ]; then
  echo "--- proxy bench --check ---"
  python3 bench/bench_reliary.py --check
  echo ""
  echo "--- homonym bench --check ---"
  python3 bench/bench_homonyms.py --check
  echo ""
  echo "all checks passed"
  exit 0
fi

echo "--- proxy bench (interleaved) ---"
python3 bench/bench_reliary.py --bin "$BIN" --pipeline "$PIPELINE" --runs 3

echo ""
echo "--- homonym bench ---"
python3 bench/bench_homonyms.py --bin "$BIN"

echo ""
echo "=== all done ==="
echo "results: bench/results/"
