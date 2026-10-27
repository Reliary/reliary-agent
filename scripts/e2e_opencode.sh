#!/usr/bin/env bash
# Layer 2 integration test: a real OpenCode agent, driven headlessly with a
# keyless free model, must actually call reliary's MCP tools and answer a
# symbol question correctly.
#
# This is the end-to-end proof that the client half works: OpenCode's own
# config parser, MCP spawn, handshake, tools/list and tools/call all run for
# real against the installed binary. Layer 1 (Dockerfile.e2e) proves the
# binary in isolation; this proves the integration.
#
# Requires: `opencode` on PATH, a built `reliary` binary, and network access to
# OpenCode's free tier. NO API KEY: with OPENCODE_API_KEY unset, OpenCode sends
# anonymous "public" requests and enables only its zero-cost models.
#
# Env:
#   RELIARY_BIN         path to the reliary binary (default: reliary on PATH)
#   OPENCODE_BIN        path to the opencode binary (default: opencode on PATH)
#   RELIARY_E2E_MODEL   model id (default: opencode/deepseek-v4.1-flash-free)
#   RELIARY_E2E_TIMEOUT per-run seconds (default: 300)
set -euo pipefail

BIN="${RELIARY_BIN:-reliary}"
OPENCODE="${OPENCODE_BIN:-opencode}"
MODEL="${RELIARY_E2E_MODEL:-opencode/deepseek-v4.1-flash-free}"
RUN_TIMEOUT="${RELIARY_E2E_TIMEOUT:-300}"

command -v "$OPENCODE" >/dev/null || { echo "opencode not found on PATH"; exit 1; }
command -v "$BIN" >/dev/null || { echo "reliary not found on PATH"; exit 1; }

PASS=0
FAIL=0
ok()  { echo "  ok   $1"; PASS=$((PASS + 1)); }
bad() { echo "  FAIL $1"; FAIL=$((FAIL + 1)); }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
HOME_DIR="$WORK/home"
PROJ="$WORK/proj"
mkdir -p "$HOME_DIR/.config/opencode" "$PROJ/src"

# Fixture with a uniquely named symbol the agent cannot know except from the code.
cat > "$PROJ/src/widget.rs" <<'RS'
/// Frobnicate the widget by scaling it up.
pub fn frobnicate_widget() -> i32 {
    widget_scale() + 1
}
RS
cat > "$PROJ/src/scale.rs" <<'RS'
/// Return the widget scale factor.
pub fn widget_scale() -> i32 {
    41
}
RS

# Redirect ALL user-scoped config/data inside the sandbox.
#
# GitHub Actions exports XDG_CONFIG_HOME and XDG_DATA_HOME, and `reliary init`
# honours them (correctly: OpenCode resolves `XDG_CONFIG_HOME || ~/.config`).
# Without this the seeded config under `$HOME/.config` is invisible to `init`,
# it prints "No OpenCode config found (optional)" and wires nothing — which is
# exactly why this workflow was red on every run from the day it was added.
# The Rust harness isolates the same two variables; this script did not.
export HOME="$HOME_DIR"
export XDG_CONFIG_HOME="$HOME_DIR/.config"
export XDG_DATA_HOME="$HOME_DIR/.local/share"
mkdir -p "$XDG_CONFIG_HOME/opencode" "$XDG_DATA_HOME"

# Seed an empty OpenCode config so `reliary init` detects OpenCode and wires it.
echo '{"$schema":"https://opencode.ai/config.json","mcp":{}}' \
  > "$XDG_CONFIG_HOME/opencode/opencode.json"
export NO_COLOR=1
export OPENCODE_DISABLE_AUTOUPDATE=1
export NO_RELIARY_WATCHER=1
# Deliberately unset any key so the free tier is used anonymously.
unset OPENCODE_API_KEY || true

echo "== setup =="
# `reliary init` writes the MCP entry using the exact path a user would take.
printf 'y\ny\ny\ny\ny\ny\n' | "$BIN" init >/dev/null 2>&1 || true

python3 - "$XDG_CONFIG_HOME/opencode/opencode.json" <<'PY'
import json, sys
cfg = json.load(open(sys.argv[1]))
entry = cfg.get("mcp", {}).get("reliary")
assert entry, "init did not wire reliary into OpenCode"
assert entry.get("type") == "local", f"OpenCode entry must be type=local: {entry}"
assert isinstance(entry.get("command"), list) and entry["command"], f"command must be an array: {entry}"
print("  ok   init wired reliary with OpenCode's schema")
PY

"$BIN" trust "$PROJ" >/dev/null 2>&1
"$BIN" build-all "$PROJ" >/dev/null 2>&1
[ -f "$PROJ/.reliary/index.sqlite" ] && ok "project indexed" || bad "project indexed"

# Run OpenCode headlessly and return "tool1,tool2||final answer".
run_agent() {
  # run_agent <cwd> <prompt>
  ( cd "$1" && timeout "$RUN_TIMEOUT" "$OPENCODE" run "$2" --model "$MODEL" --format json 2>/dev/null || true ) \
    | python3 -c '
import sys, json
tools, texts = [], []
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    try: e = json.loads(line)
    except Exception: continue
    if e.get("type") == "tool_use":
        tools.append(e.get("part", {}).get("tool", ""))
    elif e.get("type") == "text":
        texts.append(e.get("part", {}).get("text", ""))
print(",".join(tools) + "||" + " ".join(texts))
'
}

# Give the agent the same guidance the product ships (AGENTS.md), so it uses
# reliary the way a real project would. Without it the model falls back to grep
# even when reliary is wired — which is exactly what AGENTS.md exists to fix.
cat > "$PROJ/AGENTS.md" <<'MD'
# Agent guide
Use the reliary MCP tools for symbol questions instead of grep/read:
- "where is X defined?" / "who calls X?" -> reliary_find_references(name="X")
- "what does X call?" -> reliary_call_graph(name="X", direction="outbound")
- "explain X" -> reliary_describe(name="X")
Prefer these over Grep/Read for locating and explaining symbols.
MD

echo "== agent loop (project root) =="
OUT="$(run_agent "$PROJ" "What does the function frobnicate_widget do? Answer in one sentence.")"
TOOLS="${OUT%%||*}"; ANSWER="${OUT#*||}"
echo "  tools: ${TOOLS:-<none>}"
# reliary MCP tools appear as "<server>_<tool>", i.e. "reliary_reliary_*".
case ",$TOOLS," in
  *",reliary_"*) ok "agent called a reliary tool" ;;
  *) bad "agent called a reliary tool (tools: ${TOOLS:-none})" ;;
esac
case "$ANSWER" in
  *frobnicate_widget*) ok "answer references the fixture symbol" ;;
  *) bad "answer references the fixture symbol (got: ${ANSWER:0:120})" ;;
esac

echo "== agent loop (from a subdirectory — walk-up) =="
OUT="$(run_agent "$PROJ/src" "In one sentence, what does widget_scale return?")"
TOOLS="${OUT%%||*}"; ANSWER="${OUT#*||}"
echo "  tools: ${TOOLS:-<none>}"
case ",$TOOLS," in
  *",reliary_"*) ok "agent used reliary from a subdirectory" ;;
  *) bad "agent used reliary from a subdirectory (tools: ${TOOLS:-none})" ;;
esac
case "$ANSWER" in
  *41*|*widget_scale*) ok "answer resolves the subdirectory query" ;;
  *) bad "answer resolves the subdirectory query (got: ${ANSWER:0:120})" ;;
esac

echo "== negative control: unwired project =="
# Fresh HOME with NO reliary MCP entry: the agent must NOT call a reliary tool.
# This proves the positive assertions above can actually be false.
NC_HOME="$WORK/nchome"; NC_PROJ="$WORK/ncproj"
mkdir -p "$NC_HOME/.config/opencode" "$NC_PROJ/src"
echo '{"mcp":{}}' > "$NC_HOME/.config/opencode/opencode.json"
cat > "$NC_PROJ/src/lib.rs" <<'RS'
pub fn frobnicate_widget() -> i32 { 7 }
RS
# Same guidance as the wired project, so the ONLY difference is that reliary is
# not wired in — proving the positive assertions can actually be false.
cp "$PROJ/AGENTS.md" "$NC_PROJ/AGENTS.md"
OUT="$(HOME="$NC_HOME" run_agent "$NC_PROJ" "What does frobnicate_widget do?")"
TOOLS="${OUT%%||*}"
echo "  tools: ${TOOLS:-<none>}"
case ",$TOOLS," in
  *",reliary_"*) bad "unwired agent must not call reliary tools" ;;
  *) ok "unwired agent did not call reliary tools" ;;
esac

echo
echo "== summary: $PASS passed, $FAIL failed =="
[ "$FAIL" -eq 0 ]
