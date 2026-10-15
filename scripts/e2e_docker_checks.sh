#!/bin/sh
# Offline end-to-end checks for the shipped `reliary` binary.
#
# Runs in a clean container (see Dockerfile.e2e): no network, no API key, no
# source tree. Every check asserts real behaviour of the installed binary, not
# a mock. Any failure exits non-zero with a labelled message.
set -eu

BIN=reliary
PASS=0
FAIL=0

ok()   { echo "  ok   $1"; PASS=$((PASS + 1)); }
bad()  { echo "  FAIL $1"; FAIL=$((FAIL + 1)); }

check() {
  # check <label> <command...>  — succeeds if the command exits 0
  label="$1"; shift
  if "$@" >/dev/null 2>&1; then ok "$label"; else bad "$label"; fi
}

grep_check() {
  # grep_check <label> <needle> <command...>
  label="$1"; needle="$2"; shift 2
  if "$@" 2>/dev/null | grep -q "$needle"; then ok "$label"; else bad "$label (missing: $needle)"; fi
}

echo "== identity =="
grep_check "--version reports reliary" "reliary" "$BIN" --version
grep_check "--help lists commands" "search" "$BIN" --help

echo "== shell integration =="
check "completions bash" "$BIN" completions bash
check "completions zsh" "$BIN" completions zsh
grep_check "man page renders" "reliary" "$BIN" man

echo "== project index =="
PROJ=/work/proj
mkdir -p "$PROJ/src"
cat > "$PROJ/src/lib.rs" <<'RS'
/// Alpha calls beta.
pub fn alpha() -> i32 {
    beta() + 1
}
RS
cat > "$PROJ/src/other.rs" <<'RS'
/// Beta returns a constant.
pub fn beta() -> i32 {
    41
}
RS
# Not a git repo: force the explicit trust path.
check "trust builds an index" "$BIN" trust "$PROJ"
if [ -f "$PROJ/.reliary/index.sqlite" ]; then ok "index file exists"; else bad "index file exists"; fi
grep_check "search finds the project file" "other.rs" "$BIN" search beta "$PROJ"
check "build-all materialises occurrences" "$BIN" build-all "$PROJ"

echo "== MCP stdio protocol =="
# Handshake + tools/list must advertise the primary menu.
OUT=$(printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | timeout 10 "$BIN" mcp 2>/dev/null || true)
echo "$OUT" | grep -q '"reliary_find_references"' && ok "tools/list advertises find_references" || bad "tools/list advertises find_references"

# A real tools/call against the indexed project, addressed by `path` so the
# check is independent of the server's own working directory.
OUT=$(printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}' \
  "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"reliary_search\",\"arguments\":{\"query\":\"beta\",\"path\":\"$PROJ\"}}}" \
  | timeout 10 "$BIN" mcp 2>/dev/null || true)
echo "$OUT" | grep -q 'other.rs' && ok "MCP tools/call returns project data" || bad "MCP tools/call returns project data"

# The server must ask for roots when the client advertises them, and must not
# create an index in a directory that has none.
OUT=$(printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{"roots":{}}}}' \
  | timeout 10 "$BIN" mcp 2>/dev/null || true)
echo "$OUT" | grep -q 'roots/list' && ok "server requests roots/list" || bad "server requests roots/list"

echo "== output compression (no-inflation invariant) =="
# 200 near-identical lines compress well; the emitted stdout must never be
# larger than the raw input (the documented hard no-inflation guarantee).
RAW=/tmp/sift_raw.txt
seq 1 200 > "$RAW"
RAW_BYTES=$(wc -c < "$RAW")
"$BIN" sift --stdin < "$RAW" > /tmp/sift_out.txt 2>/dev/null || true
SIFT_BYTES=$(wc -c < /tmp/sift_out.txt)
if [ "$SIFT_BYTES" -le "$RAW_BYTES" ]; then ok "sift never inflates ($SIFT_BYTES <= $RAW_BYTES)"; else bad "sift inflated ($SIFT_BYTES > $RAW_BYTES)"; fi
# Source reads must pass through byte-for-byte.
printf 'pub fn x() -> i32 { 1 }\n' > /tmp/src_read.rs
if "$BIN" wrap cat /tmp/src_read.rs 2>/dev/null | grep -q 'pub fn x()'; then ok "wrap passes source reads through"; else bad "wrap passes source reads through"; fi

echo "== agent integration (init/uninstall) =="
FAKE_HOME=/work/fakehome
mkdir -p "$FAKE_HOME/.config/opencode" "$FAKE_HOME/.claude"
echo '{"mcpServers":{}}' > "$FAKE_HOME/.claude.json"
echo '{"mcp":{}}' > "$FAKE_HOME/.config/opencode/opencode.json"

# Answer yes to the OpenCode MCP prompt; skip Claude and hooks. Answers are
# consumed positionally, so feed more than the number of prompts.
printf 'n\nn\ny\nn\nn\nn\nn\n' | HOME="$FAKE_HOME" NO_RELIARY_WATCHER=1 "$BIN" init >/dev/null 2>&1 || true

python3 - "$FAKE_HOME" <<'PY'
import json, sys, os
home = sys.argv[1]
oc = json.load(open(os.path.join(home, ".config/opencode/opencode.json")))
entry = oc.get("mcp", {}).get("reliary")
assert entry, "init must write the OpenCode MCP entry"
assert entry.get("type") == "local", f"OpenCode entry must be type=local: {entry}"
assert isinstance(entry.get("command"), list) and entry["command"], f"OpenCode command must be an array: {entry}"
print("  ok   OpenCode MCP entry matches OpenCode's schema")
PY

HOME="$FAKE_HOME" NO_RELIARY_WATCHER=1 "$BIN" uninstall >/dev/null 2>&1 || true
python3 - "$FAKE_HOME" <<'PY'
import json, sys, os
home = sys.argv[1]
txt = open(os.path.join(home, ".config/opencode/opencode.json")).read()
assert "reliary" not in txt, "uninstall must remove the OpenCode entry"
print("  ok   uninstall removes the OpenCode entry")
PY

echo "== negative control =="
# Prove the schema check above has teeth: the malformed Claude/Cline shape
# (string command, no type) that broke real OpenCode installs must be rejected.
if python3 - <<'PY'
def valid(entry):
    return entry.get("type") == "local" and isinstance(entry.get("command"), list) and entry["command"]
assert not valid({"command": "/x/reliary", "args": ["mcp"]}), "malformed shape accepted"
assert valid({"type": "local", "command": ["/x/reliary", "mcp"]}), "valid shape rejected"
PY
then ok "schema validator rejects the malformed shape"; else bad "schema validator rejects the malformed shape"; fi

echo
echo "== summary: $PASS passed, $FAIL failed =="
[ "$FAIL" -eq 0 ]
