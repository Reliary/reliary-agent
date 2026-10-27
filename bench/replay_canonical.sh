#!/usr/bin/env bash
# Replay the canonical benchmark tape with zero API calls.
#
# Verifies the recorded 3-way comparison (A=reliary, B=altbackend, C=grep,
# deepseek-v4-flash, seeds 42/17/123/456) byte-for-byte against a fresh
# corpus checkout. No API key, no network, $0.
#
# Requirements:
#   - reliary binary built from the commit that recorded the tape
#     (cargo build --release -p reliary-agent)
#   - python3
#   - For condition B only: codebase-memory-mcp v0.7.0 on PATH, with the
#     corpus indexed (the runner indexes it for you if the CLI is present).
#     A and C replay without it.
#
# Usage:
#   bench/replay_canonical.sh [--corpus-dir DIR]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORPUS_COMMIT="5c8b6244"          # public commit this tape was recorded against
TAPE="$ROOT/bench/cassettes/canonical-v11"
BIN="$ROOT/target/release/reliary"
CORPUS_DIR="${HOME}/src/v75-canon-replay"

while [ $# -gt 0 ]; do
  case "$1" in
    --corpus-dir) CORPUS_DIR="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ ! -x "$BIN" ]; then
  echo "building reliary..." >&2
  (cd "$ROOT" && cargo build --release -p reliary-agent)
fi

if [ ! -d "$CORPUS_DIR/.git" ] && [ ! -f "$CORPUS_DIR/.git" ]; then
  echo "creating corpus worktree at $CORPUS_DIR ($CORPUS_COMMIT)..." >&2
  rm -rf "$CORPUS_DIR"
  git -C "$ROOT" worktree add --detach "$CORPUS_DIR" "$CORPUS_COMMIT"
fi

echo "indexing corpus..." >&2
rm -rf "$CORPUS_DIR/.reliary"
(cd "$CORPUS_DIR" && "$BIN" trust . >/dev/null)
# The tape was recorded against a fully-built occurrence table. `trust` only
# populates phrases, so without this the first `describe`/`search` result
# differs and every A/C run is a strict replay miss. This is a reproducibility
# step, not an optimisation.
(cd "$CORPUS_DIR" && "$BIN" build-all . >/dev/null 2>&1)

ALTBACKEND=""
PROJECT="$CORPUS_DIR"
PROJECT="${PROJECT#/}"; PROJECT="${PROJECT//\//-}"; PROJECT="${PROJECT//./-}"

# Locate codebase-memory-mcp: PATH first, then the usual install location.
CBM=""
if command -v codebase-memory-mcp >/dev/null 2>&1; then
  CBM="$(command -v codebase-memory-mcp)"
elif [ -x "$HOME/.local/bin/codebase-memory-mcp" ]; then
  CBM="$HOME/.local/bin/codebase-memory-mcp"
fi

if [ -n "$CBM" ]; then
  echo "indexing corpus for altbackend ($PROJECT)..." >&2
  "$CBM" cli index_repository "{\"repo_path\":\"$CORPUS_DIR\"}" >/dev/null 2>&1 || true
  ALTBACKEND="--altbackend-project $PROJECT"
  # The runner spawns the MCP server by name; make sure it resolves.
  case ":$PATH:" in
    *":$(dirname "$CBM"):"*) ;;
    *) PATH="$(dirname "$CBM"):$PATH"; export PATH ;;
  esac
else
  echo "note: codebase-memory-mcp not found — condition B will be skipped" >&2
fi

# Condition C runs arbitrary shell commands through `bash -c`, so its output
# embeds absolute paths and depends on the checkout location. A and B replay
# byte-identically anywhere; C only at the recorded path. This is a limit of
# the grep condition itself, not of reliary.
if [ "$CORPUS_DIR" != "${HOME}/src/v75-canon-replay" ]; then
  echo "note: C is not replayable at a different corpus path (its shell output embeds it);" >&2
  echo "      use REPLAY_CONDS=A,B for a byte-identical check here." >&2
fi

echo "replaying tape (zero API calls)..." >&2
OUT="$ROOT/bench/results/canonical_replay.jsonl"
# All three conditions now replay byte-identically on a fresh corpus at a
# different path (verified). B needs codebase-memory-mcp installed and
# indexed; when it is absent the script falls back to A,C automatically.
if [ -n "$CBM" ]; then
  CONDS="${REPLAY_CONDS:-A,B,C}"
else
  CONDS="${REPLAY_CONDS:-A,C}"
fi
RELIARY_CASSETTE="$TAPE" \
RELIARY_CASSETTE_MODE=replay-strict \
RELIARY_CASSETTE_COMPACT=1 \
RELIARY_CORPUS="$CORPUS_DIR" \
python3 "$ROOT/bench/run_snapshot_bench.py" \
  --bin "$BIN" \
  --corpus "$CORPUS_DIR" \
  $ALTBACKEND \
  --conds "$CONDS" \
  --seeds 42 17 123 456 \
  --out "$OUT"

echo >&2
echo "results: $OUT" >&2
echo "compare against the recorded run:" >&2
echo "  python3 bench/compare_replay.py bench/cassettes/canonical-v11/record.jsonl $OUT" >&2
