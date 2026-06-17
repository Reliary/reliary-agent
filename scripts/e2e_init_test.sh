#!/bin/bash
set -e
echo "=== Init dry run ==="
mkdir -p /tmp/fakehome/.config
echo '{"mcpServers":{}}' > /tmp/fakehome/.claude.json
echo '{"mcpServers":{}}' > /tmp/fakehome/.config/opencode.json

# Feed answers: 3 Y's for Claude, OpenCode, proxy routing, then N for daemon
echo -e "Y\nY\nY\nN" | HOME=/tmp/fakehome reliary-agent init 2>&1

# Verify Claude config was modified
if grep -q "reliary" /tmp/fakehome/.claude.json; then
  echo "✓ init injected MCP into Claude"
else
  echo "✗ init failed to modify Claude config" >&2
  cat /tmp/fakehome/.claude.json >&2
  exit 1
fi
