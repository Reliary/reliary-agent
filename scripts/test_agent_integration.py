#!/usr/bin/env python3
"""Test non-Pi agent integration: init detects configs, proxy routes, MCP responds."""

import os, json, subprocess, tempfile, shutil

RELIARY = os.path.expanduser("~/.local/bin/reliary-agent")
if not os.path.exists(RELIARY):
    RELIARY = "/home/dev/src/reliary-agent/target/release/reliary-agent"

def test_claude_config_injection():
    print("=== Test 1: Claude Code config injection ===")
    claude_path = os.path.expanduser("~/.claude.json")
    original = {}
    if os.path.exists(claude_path):
        with open(claude_path) as f:
            original = json.load(f)
    
    # Run init (non-interactive)
    subprocess.run([RELIARY, "init"], input=b"n\nn\nn\nn\n", capture_output=True, timeout=30)
    
    if os.path.exists(claude_path):
        with open(claude_path) as f:
            config = json.load(f)
        mcp = config.get("mcpServers", {})
        has_rel = "reliary" in mcp
        print(f"  MCP server injected: {'✅' if has_rel else '❌'}")
        if has_rel:
            print(f"  Command: {mcp['reliary']['command']}")
    else:
        print("  No Claude config found (expected — none existed)")
    
    # Restore
    if original:
        with open(claude_path, "w") as f:
            json.dump(original, f, indent=2)
    print()

def test_proxy_routing():
    print("=== Test 2: Proxy routes by auth header ===")
    proc = subprocess.Popen([RELIARY, "serve", "19840"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    import time; time.sleep(3)
    
    import urllib.request, urllib.error
    
    # Unknown key → 403
    req = urllib.request.Request("http://localhost:19840/v1/chat/completions",
        data=b'{"model":"test","messages":[{"role":"user","content":"hi"}]}',
        headers={"Content-Type":"application/json","Authorization":"Bearer unknown-key"})
    try:
        urllib.request.urlopen(req, timeout=5)
        print("  Unknown key: ❌ Unexpected 200")
    except urllib.error.HTTPError as e:
        print(f"  Unknown key: ✅ 403 ({e.code})")
        assert e.code == 403
    
    # Health check
    req = urllib.request.Request("http://localhost:19840/health")
    try:
        r = urllib.request.urlopen(req, timeout=5)
        body = r.read().decode()
        print(f"  Health: ✅ 200 ({body[:50]})")
    except Exception as e:
        print(f"  Health: ❌ {e}")
    
    subprocess.run(["kill", str(subprocess.check_output(["lsof", "-ti", ":19840"]).decode().strip())],
                   capture_output=True)
    print()

def test_mcp_server():
    print("=== Test 3: MCP server responds to tools/list ===")
    proc = subprocess.Popen([RELIARY, "serve"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE)
    out, _ = proc.communicate(input=b'{"jsonrpc":"2.0","id":1,"method":"tools/list"}\n', timeout=10)
    if b"search" in out and b"risk" in out:
        print(f"  MCP tools: ✅ search + risk found")
    else:
        print(f"  MCP tools: ❌ unexpected: {out[:200]}")
    proc.kill()
    print()

def test_opencode_config_injection():
    print("=== Test 4: OpenCode config injection ===")
    opencode_path = os.path.expanduser("~/.config/opencode/opencode.json")
    if os.path.exists(opencode_path):
        with open(opencode_path) as f:
            original = json.load(f)
        mcp = original.get("mcpServers", {})
        has_rel = "reliary" in mcp
        print(f"  MCP server present: {'✅' if has_rel else '❌'}")
    else:
        print("  No OpenCode config found (expected)")
    print()

if __name__ == "__main__":
    test_claude_config_injection()
    test_proxy_routing()
    test_mcp_server()
    test_opencode_config_injection()
    print("=== All integration tests complete ===")
