#!/bin/bash
set -e
echo "=== Init dry run ==="
mkdir -p /tmp/fakehome/.config/opencode
echo '{"mcpServers":{}}' > /tmp/fakehome/.claude.json
echo '{"mcpServers":{}}' > /tmp/fakehome/.config/opencode/opencode.json

# Cline config directory
CLINE_DIR="/tmp/fakehome/.config/Code/User/globalStorage/rooveterinery.cline"
mkdir -p "$CLINE_DIR"
echo '{"mcpServers":{}}' > "$CLINE_DIR/cline_mcp_settings.json"

# Feed answers: Y(Claude), Y(OpenCode SSE), Y(OpenCode proxy), Y(Cline), N(Daemon)
echo -e "Y\nY\nY\nY\nN" | HOME=/tmp/fakehome reliary-agent init 2>&1

# Verify Claude config was modified
if grep -q "reliary" /tmp/fakehome/.claude.json; then
  echo "✓ init injected MCP into Claude"
else
  echo "✗ init failed to modify Claude config" >&2
  cat /tmp/fakehome/.claude.json >&2
  exit 1
fi

# Verify OpenCode config was modified
if grep -q "reliary" /tmp/fakehome/.config/opencode/opencode.json; then
  echo "✓ init injected MCP into OpenCode"
else
  echo "✗ init failed to modify OpenCode config" >&2
  cat /tmp/fakehome/.config/opencode/opencode.json >&2
  exit 1
fi

# Verify Cline config was modified
if grep -q "reliary" "$CLINE_DIR/cline_mcp_settings.json"; then
  echo "✓ init injected MCP into Cline"
else
  echo "✗ init failed to modify Cline config" >&2
  cat "$CLINE_DIR/cline_mcp_settings.json" >&2
  exit 1
fi

