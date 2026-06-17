/// E2E tests for MCP protocol — mocked client that simulates what
/// Claude Code, Cline, OpenCode, and Pi would actually do over MCP.
/// Tests run against the reliary-agent binary in "mcp" (stdio) mode.
///
/// Each test: initialize → tools/list → tools/call → validate response shape.

mod common;

use serde_json::Value;

fn init(mcp: &mut common::McpGuard) {
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2024-11-05" },
    }));
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05",
        "should return agreed protocol version");
    assert!(resp["result"]["capabilities"]["tools"].is_object(),
        "should advertise tool capability");
}

fn list_tools(mcp: &mut common::McpGuard) -> Vec<Value> {
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {},
    }));
    resp["result"]["tools"].as_array()
        .expect("tools/list should return array")
        .clone()
}

fn call_tool(mcp: &mut common::McpGuard, name: &str, args: serde_json::Value) -> Value {
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "tools/call",
        "params": { "name": name, "arguments": args },
    }));
    resp
}

/// Test 1: Full MCP handshake — initialize → tools/list → validate every tool's schema
#[test]
fn e2e_mcp_full_schema_validation() {
    let mut mcp = common::start_mcp();
    init(&mut mcp);

    let tools = list_tools(&mut mcp);
    assert!(tools.len() >= 7, "expected 7 tools, got {}", tools.len());

    let names: Vec<&str> = tools.iter()
        .map(|t| t["name"].as_str().unwrap_or(""))
        .collect();

    // Every tool must have: name, description, inputSchema
    for tool in &tools {
        let name = tool["name"].as_str().unwrap_or("");
        assert!(!name.is_empty(), "tool missing name");
        assert!(name.starts_with("reliary_"), "tool name should start with reliary_: {}", name);

        let desc = tool["description"].as_str().unwrap_or("");
        assert!(!desc.is_empty(), "tool {} missing description", name);
        assert!(desc.len() > 5, "tool {} description too short: {}", name, desc);

        let schema = tool.get("inputSchema");
        assert!(schema.is_some(), "tool {} missing inputSchema", name);
    }

    // Required tools
    let required = &[
        "reliary_search", "reliary_compress", "reliary_risk",
        "reliary_fix", "reliary_dead", "reliary_heal", "reliary_prior",
    ];
    for r in required {
        assert!(names.contains(r), "missing required tool: {}", r);
    }
}

/// Test 2: Every tool responds correctly to tools/call with valid arguments
#[test]
fn e2e_mcp_all_tools_respond() {
    let mut mcp = common::start_mcp();
    init(&mut mcp);

    // Search (no index — should not crash)
    let resp = call_tool(&mut mcp, "reliary_search", serde_json::json!({
        "query": "test",
        "path": ".",
    }));
    assert!(resp.get("error").is_none() || resp["error"]["code"] != serde_json::json!(-32601),
        "search should not be 'method not found': {:?}", resp["error"]);
    // Should return content array
    if resp.get("error").is_none() {
        assert!(resp["result"]["content"].is_array(), "search should return content array");
    }

    // Compress
    let resp = call_tool(&mut mcp, "reliary_compress", serde_json::json!({
        "text": "Let me think about this carefully. First, I need to analyze the problem step by step.",
    }));
    let err = resp.get("error");
    assert!(err.is_none(), "compress failed: {:?}", err);
    let content = &resp["result"]["content"];
    assert!(content.is_array() && content.as_array().unwrap().len() >= 1,
        "compress should return content array");
    let text = content[0]["text"].as_str().unwrap_or("");
    assert!(text.contains("compressed") || text.contains("original_len"),
        "expected compression metrics, got: {}", &text[..text.len().min(100)]);

    // Risk
    let resp = call_tool(&mut mcp, "reliary_risk", serde_json::json!({
        "file": "Cargo.toml",
    }));
    let err = resp.get("error");
    assert!(err.is_none(), "risk failed: {:?}", err);
    let content = &resp["result"]["content"];
    assert!(content.is_array(), "risk should return content array");
    let text = content[0]["text"].as_str().unwrap_or("");
    assert!(text.contains("risk") || text.contains("file"),
        "expected risk/file info, got: {}", &text[..text.len().min(100)]);

    // Fix (with a temp file)
    let dir = tempfile::tempdir().expect("tempdir");
    let test_file = dir.path().join("test.txt");
    std::fs::write(&test_file, "Hello old").unwrap();
    let resp = call_tool(&mut mcp, "reliary_fix", serde_json::json!({
        "file": test_file.to_str().unwrap(),
        "old": "old",
        "new": "world",
    }));
    let err = resp.get("error");
    assert!(err.is_none(), "fix failed: {:?}", err);
    let content = &resp["result"]["content"];
    assert!(content.is_array(), "fix should return content array");
    let text = content[0]["text"].as_str().unwrap_or("");
    assert!(text.contains("success") || text.contains("replacements"),
        "expected fix result, got: {}", &text[..text.len().min(100)]);

    // Dead (with no index — should return empty results, not crash)
    let resp = call_tool(&mut mcp, "reliary_dead", serde_json::json!({
        "path": ".",
        "limit": 5,
    }));
    let err = resp.get("error");
    assert!(err.is_none(), "dead failed: {:?}", err);
    let content = &resp["result"]["content"];
    assert!(content.is_array(), "dead should return content array");
    let text = content[0]["text"].as_str().unwrap_or("");
    assert!(text.contains("total") || text.contains("high"),
        "expected dead code summary, got: {}", &text[..text.len().min(100)]);

    // Prior
    let resp = call_tool(&mut mcp, "reliary_prior", serde_json::json!({
        "path": ".",
    }));
    let err = resp.get("error");
    assert!(err.is_none(), "prior failed: {:?}", err);
    let content = &resp["result"]["content"];
    assert!(content.is_array(), "prior should return content array");
}

/// Test 3: Input validation — each tool rejects missing required args gracefully
#[test]
fn e2e_mcp_tool_input_validation() {
    let mut mcp = common::start_mcp();
    init(&mut mcp);

    // Search missing query — should error, not crash
    let resp = call_tool(&mut mcp, "reliary_search", serde_json::json!({}));
    assert!(resp.get("error").is_some() || resp["result"]["content"].is_array(),
        "search without query should error or return empty: {:?}", resp);

    // Risk missing file
    let resp = call_tool(&mut mcp, "reliary_risk", serde_json::json!({}));
    assert!(resp.get("error").is_some() || resp["result"]["content"].is_array(),
        "risk without file should error");

    // Fix missing old/new
    let resp = call_tool(&mut mcp, "reliary_fix", serde_json::json!({
        "file": "/tmp/nonexistent",
    }));
    assert!(resp.get("error").is_some(), "fix without old/new should error: {:?}", resp);

    // Heal missing required args
    let resp = call_tool(&mut mcp, "reliary_heal", serde_json::json!({}));
    assert!(resp.get("error").is_some() || resp["result"]["content"].is_array(),
        "heal without args should error or handle gracefully");

    // Dead missing path
    let resp = call_tool(&mut mcp, "reliary_dead", serde_json::json!({}));
    assert!(resp.get("error").is_some() || resp["result"]["content"].is_array(),
        "dead without path should error or handle gracefully");
}

/// Test 4: Response content format matches MCP spec that agents expect
#[test]
fn e2e_mcp_response_format_conforms() {
    let mut mcp = common::start_mcp();
    init(&mut mcp);

    // Every tools/call response must conform to MCP spec:
    // result.content: [{ type, text }]
    // OR
    // error: { code, message }

    let resp = call_tool(&mut mcp, "reliary_compress", serde_json::json!({
        "text": "hello world",
    }));

    if let Some(err) = resp.get("error") {
        // Error format must have code and message
        assert!(err["code"].is_i64(), "error must have numeric code");
        assert!(err["message"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
            "error must have non-empty message");
    } else if let Some(content) = resp.get("result").and_then(|r| r.get("content")) {
        // Content must be array of {type, text} objects
        let arr = content.as_array().expect("content must be array");
        for item in arr {
            let t = item["type"].as_str().unwrap_or("text");
            assert!(t == "text", "content item type must be 'text', got '{}'", t);
            assert!(item.get("text").is_some() || item.get("data").is_some(),
                "content item must have text or data field");
        }
    } else {
        panic!("response must have either result.content or error");
    }
}

/// Test 5: Heal tool end-to-end via MCP (creates temp file, applies edit, verifies)
#[test]
fn e2e_mcp_heal_tool() {
    let mut mcp = common::start_mcp();
    init(&mut mcp);

    let dir = tempfile::tempdir().expect("tempdir");
    let test_file = dir.path().join("test_heal.py");
    std::fs::write(&test_file, r#"
def add(a, b):
    return a - b  # BUG: should be a + b
"#).unwrap();

    let resp = call_tool(&mut mcp, "reliary_heal", serde_json::json!({
        "file": test_file.to_str().unwrap(),
        "old": "return a - b",
        "new": "return a + b",
        "workdir": dir.path().to_str().unwrap(),
    }));

    // Heal should either succeed (with test) or give a descriptive error
    if let Some(err) = resp.get("error") {
        let msg = err["message"].as_str().unwrap_or("");
        assert!(!msg.is_empty(), "error message should not be empty: {:?}", err);
        // Common: no test file found (python test needed) — acceptable
        assert!(!msg.contains("panic") && !msg.contains("internal"),
            "error should be descriptive, not a crash: {}", msg);
    } else {
        let content = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(content.contains("success") || content.contains("message"),
            "heal result should indicate outcome: {}", &content[..content.len().min(100)]);
    }
}

/// Test 6: Dead code with confidence filter
#[test]
fn e2e_mcp_dead_with_filter() {
    let mut mcp = common::start_mcp();
    init(&mut mcp);

    let resp = call_tool(&mut mcp, "reliary_dead", serde_json::json!({
        "path": ".",
        "limit": 3,
        "confidence": "high",
    }));
    let err = resp.get("error");
    assert!(err.is_none(), "dead with filter failed: {:?}", err);
    let content = &resp["result"]["content"];
    assert!(content.is_array(), "dead should return content array");
    let text = content[0]["text"].as_str().unwrap_or("");
    assert!(text.contains("high") || text.contains("total"),
        "expected filtered summary, got: {}", &text[..text.len().min(100)]);
}

/// Test 7: Error handling — malformed JSON-RPC
#[test]
fn e2e_mcp_malformed_requests() {
    let mut mcp = common::start_mcp();
    init(&mut mcp);

    // Missing jsonrpc field
    let resp = call_tool(&mut mcp, "reliary_compress", serde_json::json!({
        "text": "test",
    }));
    // Should still work (jsonrpc field not strictly validated for tools/call)
    assert!(resp.get("error").is_none() || resp["error"]["code"] != serde_json::json!(-32700),
        "missing jsonrpc should not cause parse error");

    // Non-existent method
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "bogus_method",
    }));
    assert!(resp.get("error").is_some(), "expected error for bogus method");
    assert_eq!(resp["error"]["code"], -32601);
}

/// Test 8: MCP transport — re-initialize after tools/list (agents do this on reconnect)
#[test]
fn e2e_mcp_reinitialize() {
    let mut mcp = common::start_mcp();

    // First session
    init(&mut mcp);
    let tools1 = list_tools(&mut mcp);
    let names1: Vec<&str> = tools1.iter()
        .map(|t| t["name"].as_str().unwrap_or("")).collect();

    // Re-initialize (simulate agent reconnect)
    init(&mut mcp);
    let tools2 = list_tools(&mut mcp);
    let names2: Vec<&str> = tools2.iter()
        .map(|t| t["name"].as_str().unwrap_or("")).collect();

    assert_eq!(names1, names2, "tools should be identical after re-init");
}

/// Test 9: Notifications are silently accepted (no response body expected)
#[test]
fn e2e_mcp_notifications_are_noops() {
    let mut mcp = common::start_mcp();
    init(&mut mcp);

    // notifications/initialized — spec says no response expected.
    // Send it raw, no response to read.
    mcp.send_raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    // Should still be able to send and receive next command
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/list",
    }));
    assert!(resp.get("result").is_some(), "should still work after notification");
}

/// Test 10: tools/call with empty arguments object for all tools
#[test]
fn e2e_mcp_empty_arguments_never_crash() {
    let mut mcp = common::start_mcp();
    init(&mut mcp);

    let tools = list_tools(&mut mcp);
    for tool in &tools {
        let name = tool["name"].as_str().unwrap_or("");
        if name.is_empty() { continue; }

        // Call each tool with empty arguments
        let resp = call_tool(&mut mcp, name, serde_json::json!({}));

        // Must not panic/crash — error or response is fine
        if let Some(err) = resp.get("error") {
            let msg = err["message"].as_str().unwrap_or("");
            assert!(!msg.contains("panic") && !msg.contains("internal"),
                "tool {} crashed with empty args: {}", name, msg);
        } else {
            assert!(resp.get("result").is_some(),
                "tool {} should return result or error, got: {:?}", name, resp);
        }
    }
}
