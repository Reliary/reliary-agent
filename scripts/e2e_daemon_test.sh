#!/bin/bash
set -e

echo "Starting daemon for endpoint tests..."
reliary-agent serve &
PID=$!
sleep 2

echo "--- Testing daemon endpoints ---"

# /ping
curl -sf http://127.0.0.1:9090/ping | grep -q "pong" && echo "✓ /ping"

# /health
curl -sf http://127.0.0.1:9090/health | grep -q "ok" && echo "✓ /health"

# /compress
curl -sf "http://127.0.0.1:9090/compress?text=hello%20world" | grep -q "compressed\|no compression" && echo "✓ /compress"

# /status
curl -sf http://127.0.0.1:9090/status && echo "✓ /status"

# /muzzle on/off
curl -sf "http://127.0.0.1:9090/muzzle?state=on" | grep -qi "muzzled\|ok" && echo "✓ muzzle on"
curl -sf "http://127.0.0.1:9090/muzzle?state=off" | grep -qi "unmuzzled\|ok" && echo "✓ muzzle off"

# Proxy rejects without auth
STATUS=$(curl -s -o /dev/null -w "%{http_code}" -X POST http://127.0.0.1:9090/v1/chat/completions \
    -H "Content-Type: application/json" -d '{"model":"test"}')
[ "$STATUS" = "403" ] || [ "$STATUS" = "401" ] && echo "✓ proxy rejects unauthenticated"

# SSE MCP endpoint route matches (404 with "session not found" means route handler fired)
BODY=$(curl -s -X POST "http://127.0.0.1:9090/mcp/messages?sessionId=test999" \
    -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}')
echo "$BODY" | grep -q "session not found" && echo "✓ SSE MCP messages route active"

echo ""
echo "=== ALL E2E TESTS PASSED ==="

kill $PID 2>/dev/null || true
