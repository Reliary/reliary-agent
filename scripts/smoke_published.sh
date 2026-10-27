#!/usr/bin/env bash
# Install the PUBLISHED crate from crates.io and check that it actually works.
#
# Why this exists
# ---------------
# Two releases in a row passed every in-tree gate and shipped broken:
#
#   0.9.1  a one-line populated-body declaration was invisible, and `def_only`
#          answered `Src::*` (the directory) instead of the declared type.
#   0.9.3  `path: "."` resolved to `<cwd>/.`, so the MCP search hint printed an
#          ABSOLUTE path while every other tool printed a relative one.
#
# Both were only visible by installing what a user installs and reading its
# output. In-tree tests and `cargo package` cannot catch either, because they
# build from the workspace and never assert the shipped artefact's behaviour.
#
# So this script is the release gate: crates.io -> cargo install -> fixture ->
# assert the answer. It runs at the end of the publish workflow.
#
# Usage:
#   scripts/smoke_published.sh <version>        install that version from crates.io
#   scripts/smoke_published.sh --local [PATH]    test an already-built binary
#
# The crates.io PACKAGE is `reliary-agent`; the installed BINARY is `reliary`.
# `--local` runs the SAME assertions against the workspace binary, so CI can
# gate a release BEFORE the tag (crates.io is immutable, so a post-publish
# failure is only an alarm, not a fixable state).
set -euo pipefail

MODE="registry"
VERSION=""
LOCAL_BIN=""
case "${1:-}" in
  --local) MODE="local"; LOCAL_BIN="${2:-target/release/reliary}" ;;
  *) VERSION="${1:?usage: smoke_published.sh <version> | --local [PATH]}" ;;
esac
PKG="reliary-agent"
BIN_NAME="reliary"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/reliary-smoke.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

export CARGO_TARGET_DIR="$WORK/target"
ROOT="$WORK/root"
mkdir -p "$ROOT"

if [ "$MODE" = "local" ]; then
  BIN="$(cd "$(dirname "$LOCAL_BIN")" && pwd)/$(basename "$LOCAL_BIN")"
  [ -x "$BIN" ] || { echo "local binary not found or not executable: $BIN" >&2; exit 1; }
  echo "== local binary =="
  "$BIN" --version
else
echo "== waiting for reliary-agent $VERSION on crates.io =="
ok=0
for i in $(seq 1 20); do
  if cargo install "$PKG" --version "$VERSION" --root "$ROOT" --quiet 2>"$WORK/install.err"; then
    ok=1
    break
  fi
  # "failed to select a version" is index propagation, not a real failure.
  if grep -qi 'failed to select a version\|no matching package\|could not find' "$WORK/install.err"; then
    echo "   not indexed yet (attempt $i), waiting 30s"
    sleep 30
    continue
  fi
  echo "   install failed:" >&2
  sed -n '1,15p' "$WORK/install.err" >&2
  exit 1
done
[ "$ok" = 1 ] || { echo "timed out waiting for $VERSION" >&2; exit 1; }

BIN="$ROOT/bin/$BIN_NAME"
[ -x "$BIN" ] || { echo "installed binary missing at $BIN" >&2; exit 1; }

echo "== version =="
"$BIN" --version
"$BIN" --version | grep -q " $VERSION\$" || {
  echo "version mismatch: expected $VERSION" >&2
  exit 1
}
fi

# ── fixture ────────────────────────────────────────────────────────────────
# Rust + Python, one symbol each, plus a one-line populated-body struct: the
# exact shape that broke 0.9.1.
FIX="$WORK/fixture"
mkdir -p "$FIX/src"
cd "$FIX"
git init -q .
printf 'pub struct Alpha { pub tag: u8 }\n\nimpl Alpha {\n    pub fn new(tag: u8) -> Self { Self { tag } }\n}\n' > src/a.rs
printf 'def beta(x):\n    return x + 1\n' > src/b.py

echo "== trust =="
"$BIN" trust . >/dev/null

echo "== CLI search =="
"$BIN" search Alpha | grep -q 'src/a.rs' || {
  echo "CLI search did not find src/a.rs" >&2
  exit 1
}

# ── MCP: the surface that shipped broken ────────────────────────────────────
mcp_call() { # tool, json-args
  {
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"%s","arguments":%s}}\n' "$1" "$2"
  } | "$BIN" mcp 2>/dev/null | tail -1
}

first_text_line() { # python: pull result.content[0].text, print first line
  python3 -c 'import sys,json
try:
    d=json.load(sys.stdin)
    print(d["result"]["content"][0]["text"].split("\n")[0])
except Exception as e:
    print("PARSE-ERROR", e); sys.exit(1)'
}

echo "== MCP search (path '.') =="
LINE=$(mcp_call reliary_search '{"query":"Alpha","path":"."}' | first_text_line)
echo "   $LINE"
case "$LINE" in
  "Alpha is defined at "*) ;;
  *) echo "search hint must lead with the definition" >&2; exit 1 ;;
esac
case "$LINE" in
  *"$FIX"*) echo "search hint leaked an absolute path (the 0.9.3 bug): $LINE" >&2; exit 1 ;;
esac
case "$LINE" in
  *"src/a.rs:"*) ;;
  *) echo "search hint must name the project-relative file" >&2; exit 1 ;;
esac

echo "== MCP find_references def_only =="
LINE=$(mcp_call reliary_find_references '{"name":"Alpha","path":".","def_only":true}' | first_text_line)
echo "   $LINE"
case "$LINE" in
  *"Alpha"*"src/a.rs"*) ;;
  *) echo "def_only must resolve the one-line struct to src/a.rs" >&2; exit 1 ;;
esac

echo "== MCP find_references def_only (Python) =="
LINE=$(mcp_call reliary_find_references '{"name":"beta","path":".","def_only":true}' | first_text_line)
echo "   $LINE"
case "$LINE" in
  *"beta"*"src/b.py"*) ;;
  *) echo "def_only must resolve a Python function" >&2; exit 1 ;;
esac

if [ "$MODE" = "local" ]; then
  echo "SMOKE OK: workspace binary answers correctly"
else
  echo "SMOKE OK: $VERSION installs from crates.io and answers correctly"
fi
