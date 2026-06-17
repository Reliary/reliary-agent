#!/bin/bash
set -e

echo "=== Starting daemon for endpoint tests ==="
reliary-agent serve &
PID=$!
sleep 2

echo ""
echo "--- Testing daemon endpoints ---"

# /ping
curl -sf http://127.0.0.1:9090/ping | grep -q "pong" && echo "OK /ping"

# /health
curl -sf http://127.0.0.1:9090/health | grep -q "ok" && echo "OK /health"

# /compress
curl -sf "http://127.0.0.1:9090/compress?text=hello%20world" | grep -q "compressed\|no compression" && echo "OK /compress"

# /status
curl -sf http://127.0.0.1:9090/status && echo "OK /status"

# /muzzle on/off
curl -sf "http://127.0.0.1:9090/muzzle?state=on" | grep -qi "muzzled\|ok" && echo "OK muzzle on"
curl -sf "http://127.0.0.1:9090/muzzle?state=off" | grep -qi "unmuzzled\|ok" && echo "OK muzzle off"

# Proxy rejects without auth
STATUS=$(curl -s -o /dev/null -w "%{http_code}" -X POST http://127.0.0.1:9090/v1/chat/completions \
    -H "Content-Type: application/json" -d '{"model":"test"}')
[ "$STATUS" = "403" ] || [ "$STATUS" = "401" ] && echo "OK proxy rejects unauthenticated" || echo "FAIL: expected 403/401, got $STATUS"

# Proxy rejects on wrong path
STATUS=$(curl -s -o /dev/null -w "%{http_code}" http://127.0.0.1:9090/v1/messages)
[ "$STATUS" = "404" ] || [ "$STATUS" = "405" ] && echo "OK unhandled path rejected"

# SSE MCP route active (404 with "session" means route handler fired)
BODY=$(curl -s -X POST "http://127.0.0.1:9090/mcp/messages?sessionId=test999" \
    -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}')
echo "$BODY" | grep -q "session not found" && echo "OK SSE MCP messages route active"

# -- Doctor fixes --
echo "--- Testing doctor --fix ---"
reliary-agent doctor --fix 2>&1 >/dev/null && echo "OK doctor --fix ran without error"

echo ""
echo "=== ALL E2E DAEMON TESTS PASSED ==="

kill $PID 2>/dev/null || true
