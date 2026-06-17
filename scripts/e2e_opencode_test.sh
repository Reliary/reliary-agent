#!/bin/bash
set -e

echo "=== OpenCode E2E Test ==="
echo ""

BIN="${1:-}"
if [ -z "$BIN" ]; then
  BIN="$(which reliary-agent 2>/dev/null || echo "/usr/local/bin/reliary-agent")"
fi
echo "Binary: $BIN"

TEST_PORT=9191
FAKE_HOME=$(mktemp -d)
TEST_API_KEY="sk-test-opencode-e2e-$(date +%s)"

echo "Fake home: $FAKE_HOME"
echo "Test port: $TEST_PORT"
echo ""

# ── Setup fake OpenCode config ──
mkdir -p "$FAKE_HOME/.config/opencode"
cat > "$FAKE_HOME/.config/opencode/opencode.json" << EOF
{
  "provider": {
    "deepseek": {
      "options": {
        "apiKey": "$TEST_API_KEY",
        "baseURL": "https://api.deepseek.com"
      }
    }
  }
}
EOF

# Also create Claude config so init doesn't error
echo '{"mcpServers":{}}' > "$FAKE_HOME/.claude.json"

# Clear env vars so init doesn't pick up real keys
OLD_DEEPSEEK_KEY="${DEEPSEEK_API_KEY:-}"
OLD_ANTHRO_KEY="${ANTHROPIC_API_KEY:-}"
unset DEEPSEEK_API_KEY
unset ANTHROPIC_API_KEY

echo "1. Mock OpenCode config:"
cat "$FAKE_HOME/.config/opencode/opencode.json"
echo ""

# ── Run init skipping daemon install ──
echo "2. Running init..."
echo -e "N\nY\nY\nY\nN\nN" | env HOME=$FAKE_HOME timeout 15 "$BIN" init 2>&1 || echo "  (init completed with exit code $?)"
echo ""

# ── Verify SSE MCP entry ──
echo "3. Verifying SSE MCP entry..."
SSE_URL=$(python3 -c "
import json
with open('$FAKE_HOME/.config/opencode/opencode.json') as f:
    cfg = json.load(f)
print(cfg.get('mcpServers', {}).get('reliary', {}).get('url', ''))
")
if [ "$SSE_URL" = "http://127.0.0.1:9090/mcp/sse" ]; then
  echo "  ✓ SSE MCP URL correct: $SSE_URL"
else
  echo "  ✗ SSE MCP URL mismatch: '$SSE_URL'" >&2
  exit 1
fi

# ── Verify provider baseURL was mutated ──
echo "4. Verifying provider baseURL mutation..."
BASE_URL=$(python3 -c "
import json
with open('$FAKE_HOME/.config/opencode/opencode.json') as f:
    cfg = json.load(f)
print(cfg.get('provider', {}).get('deepseek', {}).get('options', {}).get('baseURL', ''))
")
if [ "$BASE_URL" = "http://127.0.0.1:9090/v1" ]; then
  echo "  ✓ Provider baseURL updated: $BASE_URL"
else
  echo "  ✗ Provider baseURL not updated: '$BASE_URL'" >&2
  exit 1
fi

# ── Verify proxy-routes.json ──
echo "5. Verifying proxy-routes.json..."
ROUTES_FILE="$FAKE_HOME/.reliary/proxy-routes.json"
if [ -f "$ROUTES_FILE" ]; then
  ROUTED_KEY=$(python3 -c "
import json
with open('$ROUTES_FILE') as f:
    routes = json.load(f)
# Find the entry that is NOT __backups
for k, v in routes.items():
    if k != '__backups':
        print(k)
        break
")
  if [ "$ROUTED_KEY" = "$TEST_API_KEY" ]; then
    echo "  ✓ proxy-routes.json maps test API key correctly"
  else
    echo "  ⚠ proxy-routes.json key mismatch: expected $TEST_API_KEY, got $ROUTED_KEY"
    cat "$ROUTES_FILE"
  fi
else
  echo "  ✗ proxy-routes.json not found at $ROUTES_FILE" >&2
  exit 1
fi
echo ""

echo "6. Starting daemon on port $TEST_PORT..."
HOME="$FAKE_HOME" "$BIN" serve "$TEST_PORT" &
DAEMON_PID=$!
sleep 3
sleep 3

# Test SSE MCP route
echo "7. Testing SSE MCP route..."
BODY=$(curl -s -X POST "http://127.0.0.1:${TEST_PORT}/mcp/messages?sessionId=oc-test-1" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}')
if echo "$BODY" | grep -q "session not found"; then
  echo "  ✓ SSE MCP route active"
else
  echo "  ✗ SSE MCP unexpected: $BODY" >&2
  kill $DAEMON_PID 2>/dev/null || true
  exit 1
fi

# Test proxy rejects unknown key
echo "8. Testing proxy rejects unknown key..."
STATUS=$(curl -s -o /dev/null -w "%{http_code}" -X POST "http://127.0.0.1:${TEST_PORT}/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer unknown-key" \
  -d '{"model":"test","messages":[{"role":"user","content":"hi"}]}')
if [ "$STATUS" = "403" ]; then
  echo "  ✓ Proxy correctly rejected unknown key (HTTP $STATUS)"
else
  echo "  ✗ Expected 403 for unknown key, got $STATUS" >&2
fi

# Test proxy accepts key from proxy-routes.json
echo "9. Testing proxy accepts known key..."
STATUS=$(curl -s -o /dev/null -w "%{http_code}" -X POST "http://127.0.0.1:${TEST_PORT}/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TEST_API_KEY" \
  -d '{"model":"test","messages":[{"role":"user","content":"hi"}]}')
# Key is fake → upstream will reject. But proxy should NOT return 403
# (that would mean auth routing failed to find the key)
if [ "$STATUS" != "403" ] && [ "$STATUS" != "401" ]; then
  echo "  ✓ Proxy accepted known key (HTTP $STATUS — auth routing works)"
else
  echo "  ✗ Proxy rejected known key (HTTP $STATUS) — auth routing broken" >&2
  kill $DAEMON_PID 2>/dev/null || true
  exit 1
fi

kill $DAEMON_PID 2>/dev/null || true
wait $DAEMON_PID 2>/dev/null || true
echo ""

# ── Test uninstall restores baseURL ──
echo "10. Testing uninstall restores original baseURL..."
echo -e "N" | HOME=$FAKE_HOME "$BIN" uninstall 2>&1 | tail -3
RESTORED_URL=$(python3 -c "
import json
with open('$FAKE_HOME/.config/opencode/opencode.json') as f:
    cfg = json.load(f)
print(cfg.get('provider', {}).get('deepseek', {}).get('options', {}).get('baseURL', ''))
")
if [ "$RESTORED_URL" = "https://api.deepseek.com" ]; then
  echo "  ✓ Provider baseURL restored: $RESTORED_URL"
elif [ "$RESTORED_URL" = "http://127.0.0.1:9090/v1" ]; then
  echo "  ⚠ Provider baseURL was not restored (still proxy)"
else
  echo "  ⚠ Unexpected baseURL after uninstall: $RESTORED_URL"
fi

# ── Cleanup ──
rm -rf "$FAKE_HOME"
[ -n "$OLD_DEEPSEEK_KEY" ] && export DEEPSEEK_API_KEY="$OLD_DEEPSEEK_KEY"
[ -n "$OLD_ANTHRO_KEY" ] && export ANTHROPIC_API_KEY="$OLD_ANTHRO_KEY"

echo ""
echo "=== OpenCode E2E Test Complete ==="
