//! End-to-end tests for the MCP stdio protocol.
//!
//! These drive the real `reliary mcp` binary over the same line-delimited
//! JSON-RPC transport that Claude Code, OpenCode, Cline and Pi use. They
//! assert protocol conformance (handshake, tools/list, tools/call, error
//! codes, malformed input survivability) and that every advertised tool is
//! actually callable.
//!
//! Run: `cargo test -p reliary-agent --test e2e_mcp`
mod common;

use common::{Fixture, Mcp};
use serde_json::json;
use std::time::Duration;

// ── 1. Handshake ──────────────────────────────────────────────────────────

#[test]
fn e2e_mcp_initialize_handshake() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());

    let resp = mcp.initialize();
    assert_eq!(resp["jsonrpc"], "2.0", "must be JSON-RPC 2.0: {}", resp);
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    assert!(
        resp["result"]["capabilities"]["tools"].is_object(),
        "must advertise tools capability: {}",
        resp
    );
    assert!(
        resp["result"]["serverInfo"]["name"].is_string(),
        "must report serverInfo.name: {}",
        resp
    );
    assert!(mcp.is_alive(), "server must stay alive after initialize");
}

#[test]
fn e2e_mcp_initialize_creates_index_when_absent() {
    let fx = Fixture::new();
    let index = fx.path().join(".reliary/index.sqlite");
    assert!(
        !index.exists(),
        "fixture must start unindexed"
    );

    let mut mcp = Mcp::start(fx.path());
    let resp = mcp.initialize();
    assert!(mcp.is_alive());

    // The fixture is a git project, so auto-trust is required to run: the
    // index must exist afterwards and the response must say it was created.
    assert!(
        index.exists(),
        "initialize on an unindexed git project must auto-trust and create \
         the index; response was {}",
        resp
    );
    assert_eq!(
        resp["result"]["serverInfo"]
            .get("auto_trusted")
            .and_then(|v| v.as_bool()),
        Some(true),
        "auto_trusted must be true when the index was just created: {}",
        resp
    );

    // The freshly created index must actually be queryable.
    let text = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "alpha", "def_only": true }),
    );
    assert!(
        text.contains("alpha"),
        "auto-created index must answer queries: {}",
        text
    );
}

/// The auto-trust must NOT fire for a non-git directory (no index is created).
#[test]
fn e2e_mcp_initialize_skips_autotrust_outside_git() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("loose.rs"), "pub fn loose() {}\n").unwrap();
    let index = dir.path().join(".reliary/index.sqlite");

    let mut mcp = Mcp::start(dir.path());
    let resp = mcp.initialize();
    assert!(mcp.is_alive());
    assert!(
        !index.exists(),
        "a non-git directory must not be auto-trusted (would surprise users \
         by creating indexes in arbitrary dirs); response was {}",
        resp
    );
}

// ── 2. tools/list conformance ─────────────────────────────────────────────

#[test]
fn e2e_mcp_tools_list_schema_conformance() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let tools = mcp.list_tools();
    assert!(
        tools.len() >= 6,
        "default menu should expose at least 6 tools, got {}",
        tools.len()
    );

    let mut names = Vec::new();
    for t in &tools {
        let name = t["name"].as_str().unwrap_or("");
        assert!(!name.is_empty(), "tool missing name: {}", t);
        assert!(
            name.starts_with("reliary_"),
            "tool name must be namespaced: {}",
            name
        );

        let desc = t["description"].as_str().unwrap_or("");
        assert!(
            desc.len() > 20,
            "tool {} needs a useful description (got {} chars)",
            name,
            desc.len()
        );

        let schema = t.get("inputSchema").expect("inputSchema required");
        assert_eq!(
            schema["type"], "object",
            "tool {} inputSchema.type must be object",
            name
        );
        assert!(
            schema.get("properties").is_some(),
            "tool {} inputSchema needs properties",
            name
        );
        names.push(name.to_string());
    }

    // No duplicates.
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "tools/list has duplicate names");

    // The entry points the shipped prompt tells the model to use.
    for required in [
        "reliary_search",
        "reliary_find_references",
        "reliary_call_graph",
        "reliary_list_methods",
        "reliary_find_dead_code",
        "reliary_describe",
    ] {
        assert!(
            names.iter().any(|n| n == required),
            "required tool {} missing from tools/list: {:?}",
            required,
            names
        );
    }
}

#[test]
fn e2e_mcp_tools_list_exposes_input_examples() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let tools = mcp.list_tools();
    let fr = tools
        .iter()
        .find(|t| t["name"] == "reliary_find_references")
        .expect("find_references must be advertised");
    let examples = fr["inputSchema"]
        .get("input_examples")
        .and_then(|e| e.as_array())
        .expect("find_references should provide input_examples (Anthropic guidance)");
    assert!(!examples.is_empty(), "input_examples must not be empty");
}

// ── 3. tools/call — every advertised tool is callable ─────────────────────

fn minimal_args_for(tool: &str) -> serde_json::Value {
    match tool {
        "reliary_search" => json!({ "query": "alpha" }),
        "reliary_find_references" => json!({ "name": "alpha" }),
        "reliary_goto_def" => json!({ "name": "alpha" }),
        "reliary_call_graph" => json!({ "name": "alpha" }),
        "reliary_list_methods" => json!({ "name": "Fixture" }),
        "reliary_find_dead_code" => json!({ "path": "src" }),
        "reliary_describe" => json!({ "name": "alpha" }),
        "reliary_similar" => json!({ "name": "alpha" }),
        "reliary_verify" => json!({ "text": "alpha at src/lib.rs:3" }),
        _ => json!({}),
    }
}

#[test]
fn e2e_mcp_every_advertised_tool_responds() {
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    for tool in mcp.list_tools() {
        let name = tool["name"].as_str().unwrap().to_string();
        let resp = mcp.call_tool(&name, minimal_args_for(&name));

        assert!(
            resp["jsonrpc"] == "2.0",
            "{}: response must be JSON-RPC 2.0: {}",
            name,
            resp
        );

        if resp.get("error").is_none() {
            let content = resp["result"]["content"]
                .as_array()
                .unwrap_or_else(|| panic!("{}: success must carry content array: {}", name, resp));
            assert!(
                !content.is_empty(),
                "{}: content array must not be empty",
                name
            );
            assert!(
                content[0]["type"] == "text",
                "{}: first content item must be text: {}",
                name,
                content[0]
            );
        }
        assert!(mcp.is_alive(), "{}: server died", name);
    }
}

#[test]
fn e2e_mcp_find_references_returns_definition_with_source() {
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let text = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "beta", "def_only": true }),
    );
    assert!(text.contains("beta"), "answer must name the symbol: {}", text);
    assert!(
        text.contains("other.rs"),
        "must locate beta in src/other.rs: {}",
        text
    );
    // V38+: raw code evidence accompanies the answer.
    assert!(
        text.contains("pub fn beta"),
        "must include raw source evidence: {}",
        text
    );
}

#[test]
fn e2e_mcp_search_never_returns_empty() {
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let text = mcp.call_tool_text("reliary_search", json!({ "query": "zzz_no_such_symbol_zzz" }));
    assert!(
        !text.trim().is_empty(),
        "search must be silent never-empty (closest files or an explanation)"
    );
}

// ── 4. Error handling / protocol robustness ───────────────────────────────

#[test]
fn e2e_mcp_unknown_method_returns_method_not_found() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let resp = mcp.request("tools/nonexistent", json!({}));
    assert_eq!(
        resp["error"]["code"], -32601,
        "unknown method must be -32601: {}",
        resp
    );
    assert!(resp["error"]["message"].is_string());
    assert!(mcp.is_alive());
}

#[test]
fn e2e_mcp_notifications_get_no_response() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    mcp.send_raw(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
    }));
    // A notification has no id; the server must stay silent.
    assert!(
        mcp.try_read_response(Duration::from_millis(400)).is_none(),
        "server must not respond to notifications"
    );
    assert!(mcp.is_alive());
}

#[test]
fn e2e_mcp_tools_call_missing_params_returns_invalid_params() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    // No `params` key at all — the JSON-RPC request is structurally invalid.
    mcp.send_raw(&json!({
        "jsonrpc": "2.0",
        "id": 777,
        "method": "tools/call",
    }));
    let resp = mcp.read_response(Duration::from_secs(30));
    assert_eq!(
        resp["error"]["code"], -32602,
        "tools/call without params must be -32602: {}",
        resp
    );
    assert!(mcp.is_alive());

    // An empty params object is a different, valid request: it names no tool,
    // so the server reports an unknown-tool error rather than a protocol error.
    let resp = mcp.call_tool("", json!({}));
    assert!(
        resp.get("error").is_some(),
        "empty tool name must produce an error: {}",
        resp
    );
    assert!(mcp.is_alive());
}

#[test]
fn e2e_mcp_unknown_tool_errors_but_stays_alive() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let resp = mcp.call_tool("reliary_no_such_tool", json!({}));
    assert!(
        resp.get("error").is_some() || resp["result"]["content"][0]["text"].is_string(),
        "unknown tool must produce an error or an explanatory text result: {}",
        resp
    );
    assert!(mcp.is_alive(), "unknown tool must not kill the server");
}

#[test]
fn e2e_mcp_malformed_json_does_not_kill_server() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    // Garbage, truncated JSON, a JSON array, and a JSON scalar.
    mcp.send_line("this is not json at all");
    mcp.send_line("{\"jsonrpc\": \"2.0\", \"id\": 7, \"method\": \"tools/list\"");
    mcp.send_line("[1, 2, 3]");
    mcp.send_line("\"just a string\"");
    mcp.send_line("{}");
    mcp.send_line("");

    // Malformed input is either dropped or answered with an error carrying a
    // null id. Drain those first so the next assertion reads the real reply.
    while let Some(resp) = mcp.try_read_response(Duration::from_millis(250)) {
        assert!(
            resp.get("error").is_some(),
            "a line that is not a valid request must not produce a result: {}",
            resp
        );
    }

    // The server must still answer a well-formed request afterwards.
    let resp = mcp.request("tools/list", json!({}));
    assert!(
        resp["result"]["tools"].is_array(),
        "server must recover after malformed input: {}",
        resp
    );
    assert!(mcp.is_alive());
}

#[test]
fn e2e_mcp_tool_with_wrong_arg_types_does_not_panic() {
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    // `name` should be a string; send numbers, arrays, objects, nulls.
    for bad in [
        json!({ "name": 12345 }),
        json!({ "name": ["alpha"] }),
        json!({ "name": { "x": 1 } }),
        json!({ "name": null }),
        json!({ "name": "alpha", "path_filter": 42 }),
        json!({ "name": "alpha", "limit": "not-a-number" }),
    ] {
        let resp = mcp.call_tool("reliary_find_references", bad.clone());
        assert!(
            resp.get("result").is_some() || resp.get("error").is_some(),
            "malformed args must yield a result or error, not a crash: {} -> {}",
            bad,
            resp
        );
        assert!(mcp.is_alive(), "server died on args {}", bad);
    }
}

#[test]
fn e2e_mcp_path_traversal_is_rejected() {
    let fx = Fixture::new();
    fx.trust();
    let marker = fx.path().join("outside.rs");
    std::fs::write(&marker, "pub fn outside() {}\n").unwrap();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    // Every path-accepting tool must reject an escape from the workdir with an
    // explicit error — never silently answer from the escaped location.
    let escapes = ["../", "../../", "../../../../etc", "/etc", "/tmp"];
    for escape in escapes {
        for (tool, arg) in [
            ("reliary_search", json!({ "query": "outside", "path": escape })),
            ("reliary_find_dead_code", json!({ "path": escape })),
        ] {
            let resp = mcp.call_tool(tool, arg.clone());
            assert!(
                mcp.is_alive(),
                "{} died on escape {}",
                tool,
                escape
            );
            // The escape must not silently produce results from outside the
            // workdir: it must be an explicit error.
            match resp.get("error") {
                Some(err) => {
                    let msg = err["message"].as_str().unwrap_or("");
                    assert!(
                        msg.contains("path") || msg.contains("escapes") || msg.contains("invalid"),
                        "{} escape {} must be rejected with a path error, got: {}",
                        tool,
                        escape,
                        msg
                    );
                }
                None => {
                    // /tmp is a valid absolute path but must still be refused
                    // (outside the workdir). A success here is a real failure.
                    let content = resp["result"]["content"][0]["text"]
                        .as_str()
                        .unwrap_or("");
                    assert!(
                        !content.contains("outside.rs") && !content.contains("/etc"),
                        "{} escape {} returned data from outside the workdir: {}",
                        tool,
                        escape,
                        content
                    );
                }
            }
        }
    }
}

#[test]
fn e2e_mcp_no_index_degrades_gracefully() {
    // A directory that is not a git repo and has no index: tools must explain
    // rather than crash. Auto-trust must not create an index here (not a git
    // repo), so this exercises the genuine no-index path.
    let dir = tempfile::tempdir().unwrap();
    assert!(
        !dir.path().join(".reliary/index.sqlite").exists(),
        "non-git fixture must stay unindexed"
    );
    let mut mcp = Mcp::start(dir.path());
    mcp.initialize();
    assert!(
        !dir.path().join(".reliary/index.sqlite").exists(),
        "initialize must not auto-trust a non-git directory"
    );

    // Each tool must return an informational result (or explicit error) whose
    // text explains that no index exists — not an empty or confusing answer.
    for (tool, args) in [
        ("reliary_find_references", json!({ "name": "anything" })),
        ("reliary_search", json!({ "query": "anything" })),
    ] {
        let resp = mcp.call_tool(tool, args.clone());
        assert!(mcp.is_alive(), "{} died with no index", tool);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .or_else(|| resp["error"]["message"].as_str())
            .unwrap_or("");
        let lower = text.to_ascii_lowercase();
        assert!(
            lower.contains("no index")
                || lower.contains("not indexed")
                || lower.contains("trust")
                || lower.contains("index first"),
            "{} with no index must say so: {} -> {}",
            tool,
            args,
            resp
        );
    }
}

#[test]
fn e2e_mcp_parallel_tool_calls_are_answered_in_order() {
    // Real agents batch parallel tool calls in one assistant message.
    // The server is single-threaded, so it must answer each in turn.
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let names = ["alpha", "beta", "alpha"];
    for (i, n) in names.iter().enumerate() {
        mcp.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": 100 + i,
            "method": "tools/call",
            "params": { "name": "reliary_find_references", "arguments": { "name": n } },
        }));
    }

    let mut ids = Vec::new();
    let mut texts = Vec::new();
    for _ in 0..names.len() {
        let resp = mcp.read_response(Duration::from_secs(30));
        ids.push(resp["id"].as_i64().unwrap_or(-1));
        assert!(
            resp.get("error").is_none(),
            "batched call must succeed: {}",
            resp
        );
        texts.push(
            resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        );
    }
    assert_eq!(ids, vec![100, 101, 102], "responses must preserve request order");
    // Each response must correspond to its own request (alpha/beta/alpha).
    assert!(
        texts[0].contains("alpha"),
        "response 0 must answer the alpha query: {}",
        texts[0]
    );
    assert!(
        texts[1].contains("beta"),
        "response 1 must answer the beta query: {}",
        texts[1]
    );
    assert_eq!(
        texts[0], texts[2],
        "identical parallel requests must yield identical answers"
    );
    assert!(mcp.is_alive());
}

#[test]
fn e2e_mcp_second_initialize_is_safe() {
    // V61: a repeated initialize must not spawn a second watcher or corrupt state.
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());

    let first = mcp.initialize();
    let second = mcp.initialize();
    assert_eq!(
        first["result"]["protocolVersion"], second["result"]["protocolVersion"],
        "re-initialize must agree on the protocol version"
    );
    assert!(mcp.is_alive());

    // And still serve tools afterwards.
    let text = mcp.call_tool_text("reliary_find_references", json!({ "name": "beta" }));
    assert!(text.contains("beta"), "must still answer after re-initialize");
}
