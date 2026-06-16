mod common;

#[test]
fn e2e_mcp_tools() {
    let mut mcp = common::start_mcp();

    // Initialize
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2024-11-05" },
    }));
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    assert!(resp["result"]["capabilities"]["tools"].is_object());

    // tools/list
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {},
    }));
    let tools = resp["result"]["tools"].as_array().expect("expected tools array");
    assert!(tools.len() >= 6, "expected at least 6 tools, got {}", tools.len());
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap_or("")).collect();
    assert!(names.contains(&"reliary_search"), "missing reliary_search");
    assert!(names.contains(&"reliary_risk"), "missing reliary_risk");
    assert!(names.contains(&"reliary_dead"), "missing reliary_dead");
    assert!(names.contains(&"reliary_compress"), "missing reliary_compress");
    assert!(names.contains(&"reliary_heal"), "missing reliary_heal");
    assert!(names.contains(&"reliary_prior"), "missing reliary_prior");

    // tools/call reliary_compress — proper MCP protocol
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "reliary_compress",
            "arguments": { "text": "Let me think about this carefully. First, I need to analyze the problem." }
        },
    }));
    assert!(resp.get("error").is_none(), "compress failed: {:?}", resp["error"]);
    let content = resp["result"]["content"].as_array().expect("expected content array");
    assert!(content.len() >= 1, "expected at least 1 content item");
    let text = content[0]["text"].as_str().unwrap_or("");
    assert!(text.contains("compressed") || text.contains("original_len"),
        "expected compression result, got: {}", text);

    // tools/call reliary_risk
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "reliary_risk",
            "arguments": { "file": "Cargo.toml" }
        },
    }));
    assert!(resp.get("error").is_none(), "risk failed: {:?}", resp["error"]);
    let content = resp["result"]["content"].as_array().expect("expected content array");
    let text = content[0]["text"].as_str().unwrap_or("");
    assert!(text.contains("risk"), "expected risk in result, got: {}", text);

    // tools/call reliary_search — may have no index, should not crash
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "reliary_search",
            "arguments": { "query": "search", "path": "." }
        },
    }));
    // Should respond without error (index may or may not exist)
    assert!(resp.get("error").is_none() || resp["error"]["code"] != serde_json::json!(-32601),
        "search should be handled: {:?}", resp);

    // tools/call reliary_prior
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": {
            "name": "reliary_prior",
            "arguments": { "path": "." }
        },
    }));
    assert!(resp.get("error").is_none(), "prior failed: {:?}", resp["error"]);
}

#[test]
fn e2e_mcp_error_handling() {
    let mut mcp = common::start_mcp();

    // Unknown method
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "nonexistent",
        "params": {},
    }));
    assert!(resp.get("error").is_some(), "expected error for unknown method");
    assert_eq!(resp["error"]["code"], -32601, "expected method not found code");

    // Unknown tool via tools/call
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "nonexistent_tool",
            "arguments": {}
        },
    }));
    assert!(resp.get("error").is_some(), "expected error for unknown tool");
    assert_eq!(resp["error"]["code"], -32601, "expected unknown tool code");

    // tools/call missing params
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {},
    }));
    assert!(resp.get("error").is_some(), "expected error for missing name");
}

#[test]
fn e2e_mcp_old_dispatch_still_works() {
    // The old-style dispatch (tools/search, tools/compress) should still
    // return method-not-found to avoid surprising any misconfigured clients
    let mut mcp = common::start_mcp();

    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/search",
        "params": { "query": "test" },
    }));
    assert!(resp.get("error").is_some(), "old dispatch should fail");
    assert_eq!(resp["error"]["code"], -32601, "expected method not found");
}
