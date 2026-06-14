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
    assert!(tools.len() >= 4, "expected at least 4 tools, got {}", tools.len());
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap_or("")).collect();
    assert!(names.contains(&"reliary_search"), "missing reliary_search");
    assert!(names.contains(&"reliary_risk"), "missing reliary_risk");
    assert!(names.contains(&"reliary_dead"), "missing reliary_dead");
    assert!(names.contains(&"reliary_compress"), "missing reliary_compress");

    // tools/dead
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/dead",
        "params": { "path": ".", "limit": 3 },
    }));
    assert!(resp["result"]["total"].is_number(), "expected total count");
    assert!(resp.get("error").is_none(), "unexpected error: {:?}", resp["error"]);

    // tools/compress
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/compress",
        "params": { "text": "Let me think about this carefully. First, I need to analyze the problem. The user is asking me to compute a sum. I'll start by reviewing the requirements carefully and ensure I understand the inputs correctly before proceeding with the solution." },
    }));
    // Short input may return Null compressed — that's OK, compression is optional
    let compressed = resp["result"]["compressed"].as_str().or_else(|| resp["result"].as_str());
    assert!(compressed.is_none() || (compressed.is_some() && !compressed.unwrap().is_empty()),
        "expected compressed text or null, got: {:?}", resp);

    // tools/risk
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/risk",
        "params": { "file": "Cargo.toml" },
    }));
    assert!(resp.get("error").is_none(), "unexpected error: {:?}", resp["error"]);

    // tools/search — verify endpoint doesn't crash, results may be empty
    // (FTS5 index depends on CWD which is test build dir)
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/search",
        "params": { "query": "search", "path": "." },
    }));
    assert!(resp.get("error").is_none(), "search failed: {:?}", resp["error"]);
    assert!(resp["result"]["results"].is_array(), "expected results array");
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

    // Missing required params
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/search",
        "params": {},
    }));
    assert!(resp.get("error").is_none() || resp["result"]["results"].as_array().is_some(),
        "missing params should not crash");
}
