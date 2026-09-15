#!/usr/bin/env bash
# V14: End-to-end integration test for the sift bash auto-rewrite hook.
#
# Simulates exactly what Claude Code does:
#   1. Sends JSON to hook on stdin
#   2. Hook outputs modified tool_input (or nothing for pass-through)
#   3. If modified, runs the rewritten command
#
# Tests all 8 critical paths:
#   - Rewrite: git, cargo, pytest, ls
#   - Pass-through: echo, shell chain, --no-sift, Read tool

set -u

HOOK="$HOME/src/reliary8/hooks/claude-pretooluse.sh"
BIN="$HOME/.cargo/bin/reliary-agent"
PASS=0
FAIL=0

check_rewrite() {
    local desc="$1"
    local input="$2"
    local expected_substr="$3"
    local got
    got=$(echo "$input" | "$HOOK" 2>/dev/null)
    if [[ "$got" == *"$expected_substr"* ]]; then
        echo "  ✓ $desc"
        PASS=$((PASS+1))
    else
        echo "  ✗ $desc (expected: $expected_substr, got: $got)"
        FAIL=$((FAIL+1))
    fi
}

check_passthrough() {
    local desc="$1"
    local input="$2"
    local got
    got=$(echo "$input" | "$HOOK" 2>/dev/null)
    if [[ -z "$got" ]]; then
        echo "  ✓ $desc"
        PASS=$((PASS+1))
    else
        echo "  ✗ $desc (expected: empty, got: $got)"
        FAIL=$((FAIL+1))
    fi
}

check_execute() {
    local desc="$1"
    local input="$2"
    local must_contain="$3"
    local modified
    modified=$(echo "$input" | "$HOOK" 2>/dev/null)
    if [[ -z "$modified" ]]; then
        echo "  ✗ $desc (hook didn't rewrite)"
        FAIL=$((FAIL+1))
        return
    fi
    local cmd
    cmd=$(echo "$modified" | python3 -c "import sys, json; d=json.load(sys.stdin); print(d.get('hookSpecificOutput',d).get('updatedInput',d.get('tool_input',{})).get('command',''))" 2>/dev/null)
    local out
    out=$(bash -c "$cmd" 2>&1)
    if [[ "$out" == *"$must_contain"* ]]; then
        echo "  ✓ $desc"
        PASS=$((PASS+1))
    else
        echo "  ✗ $desc (expected '$must_contain' in output, got: ${out:0:100})"
        FAIL=$((FAIL+1))
    fi
}

echo "=== Phase 1: Rewrite decisions ==="
check_rewrite "git status -> wrap" \
    '{"tool_name":"Bash","tool_input":{"command":"git status"}}' \
    "wrap bash -c 'git status'"
check_rewrite "cargo test -> wrap" \
    '{"tool_name":"Bash","tool_input":{"command":"cargo test"}}' \
    "wrap bash -c 'cargo test'"
check_rewrite "pytest -v -> wrap" \
    '{"tool_name":"Bash","tool_input":{"command":"pytest -v tests/"}}' \
    "wrap bash -c 'pytest -v tests/'"
check_rewrite "ls -la -> wrap" \
    '{"tool_name":"Bash","tool_input":{"command":"ls -la"}}' \
    "wrap bash -c 'ls -la'"
check_rewrite "go test -> wrap" \
    '{"tool_name":"Bash","tool_input":{"command":"go test ./..."}}' \
    "wrap bash -c 'go test ./...'"

echo ""
echo "=== Phase 2: Pass-through decisions ==="
check_passthrough "echo hello (not in REWRITE_PROGRAMS)" \
    '{"tool_name":"Bash","tool_input":{"command":"echo hello"}}'
check_passthrough "shell chain (&&)" \
    '{"tool_name":"Bash","tool_input":{"command":"git status && ls"}}'
check_passthrough "shell chain (pipe)" \
    '{"tool_name":"Bash","tool_input":{"command":"git status | grep branch"}}'
check_passthrough "--no-sift override" \
    '{"tool_name":"Bash","tool_input":{"command":"git status --no-sift"}}'
check_passthrough "Read tool (not bash)" \
    '{"tool_name":"Read","tool_input":{"file":"foo.rs"}}'
check_passthrough "Edit tool (not bash)" \
    '{"tool_name":"Edit","tool_input":{"file":"foo.rs"}}'

echo ""
echo "=== Phase 3: End-to-end execution ==="
check_execute "git status through wrap" \
    '{"tool_name":"Bash","tool_input":{"command":"git -C $HOME/src/reliary8 status"}}' \
    "reliary-compressed"
check_execute "ls through wrap" \
    '{"tool_name":"Bash","tool_input":{"command":"ls -la $HOME/src/reliary8/hooks/"}}' \
    "reliary-compressed"

echo ""
echo "=== Phase 4: Determinism (cache safety) ==="
HASH1=$(cat $HOME/src/reliary8/hooks/claude-pretooluse.sh | "$BIN" wrap bash -c 'cat $HOME/src/reliary8/hooks/claude-pretooluse.sh' 2>&1 | grep -oP 'reliary-compressed \K[a-f0-9]+')
HASH2=$(cat $HOME/src/reliary8/hooks/claude-pretooluse.sh | "$BIN" wrap bash -c 'cat $HOME/src/reliary8/hooks/claude-pretooluse.sh' 2>&1 | grep -oP 'reliary-compressed \K[a-f0-9]+')
if [[ "$HASH1" == "$HASH2" && -n "$HASH1" ]]; then
    echo "  ✓ identical input → identical hash ($HASH1)"
    PASS=$((PASS+1))
else
    echo "  ✗ hash mismatch: $HASH1 vs $HASH2"
    FAIL=$((FAIL+1))
fi

echo ""
echo "================================================="
echo "Passed: $PASS    Failed: $FAIL"
echo "================================================="

exit $FAIL