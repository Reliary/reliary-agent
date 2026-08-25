#!/bin/bash
# reliary PreToolUse gate for Claude Code
# Blocks the first grep/glob/read per session, redirects to reliary MCP tools.
# Subsequent calls pass through. Nudge, not force.
#
# Install: copy to ~/.claude/hooks/reliary-code-gate and add to settings.json PreToolUse
# Toggle: RELIARY_GATE=0 to disable (default ON)

if [ "${RELIARY_GATE:-1}" != "1" ]; then
  exit 0
fi

# Read tool input JSON from stdin
input=$(cat)
tool_name=$(echo "$input" | jq -r '.tool_name // empty' 2>/dev/null)

# Only gate code discovery tools
case "$tool_name" in
  Grep|grep|Glob|glob|Read|read) ;;
  *) exit 0 ;;
esac

# C5: use mkdir for atomic create (no TOCTOU), unique per-session key
# combining PID + epoch nanoseconds to prevent collisions across sessions.
SESSION_KEY="${PPID:-$$}-$(date +%s%N)"
GATE="/tmp/reliary-gate-${SESSION_KEY}"

if [ -d "$GATE" ]; then
  exit 0  # already gated this session — pass through
fi

if mkdir "$GATE" 2>/dev/null; then
  # Successfully created gate — block this call
  trap 'rmdir "$GATE" 2>/dev/null' EXIT
else
  # Another process beat us — pass through
  exit 0
fi

cat >&2 << 'REDIRECT'
BLOCKED: For finding symbol references, use reliary_find_references_with_source FIRST — it returns file:line + source inline.
For definitions: reliary_goto_def(name)
For call chains: reliary_callgraph(name)
For methods on a type: reliary_methods_on(type_name)
For unknown symbols: reliary_search(query)
If reliary lacks the data, retry this tool.
REDIRECT
exit 2