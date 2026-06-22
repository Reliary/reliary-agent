use std::io::{self, BufRead, Write};

fn respond(id: u64, result: serde_json::Value) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    let mut out = io::stdout();
    writeln!(out, "{}", serde_json::to_string(&response).unwrap_or_default()).unwrap_or_default();
    out.flush().unwrap_or_default();
}

fn respond_error(id: u64, code: i32, message: &str) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    });
    let mut out = io::stdout();
    writeln!(out, "{}", serde_json::to_string(&response).unwrap_or_default()).unwrap_or_default();
    out.flush().unwrap_or_default();
}

/// Public tool definitions for MCP tools/list — shared by stdio and SSE.
pub fn tool_definitions() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({ "name": "reliary_search", "description": "BM25 grammar-free code search", "inputSchema": { "type": "object", "properties": { "query": {"type": "string"}, "path": {"type": "string"} }, "required": ["query"] } }),
        serde_json::json!({ "name": "reliary_compress", "description": "IR reasoning compression", "inputSchema": { "type": "object", "properties": { "text": {"type": "string"} }, "required": ["text"] } }),
        serde_json::json!({ "name": "reliary_risk", "description": "Pre-edit risk analysis", "inputSchema": { "type": "object", "properties": { "file": {"type": "string"} }, "required": ["file"] } }),
        serde_json::json!({ "name": "reliary_fix", "description": "Pattern-based file fix", "inputSchema": { "type": "object", "properties": { "file": {"type": "string"}, "old": {"type": "string"}, "new": {"type": "string"} }, "required": ["file", "old", "new"] } }),
        serde_json::json!({ "name": "reliary_dead", "description": "Grammar-free dead code detection (compact summary + top-N)", "inputSchema": { "type": "object", "properties": { "path": {"type": "string"}, "limit": {"type": "integer"}, "confidence": {"type": "string"} }, "required": ["path"] } }),
        serde_json::json!({ "name": "reliary_heal", "description": "Apply edit with self-healing (test before commit)", "inputSchema": { "type": "object", "properties": { "file": {"type": "string"}, "old": {"type": "string"}, "new": {"type": "string"}, "workdir": {"type": "string"} }, "required": ["file", "old", "new"] } }),
        serde_json::json!({ "name": "reliary_prior", "description": "Chronicled project state and cross-session memory", "inputSchema": { "type": "object", "properties": { "path": {"type": "string"} }, "required": ["path"] } }),
    ]
}

/// Pure dispatch result — returned by dispatch_tool_call for shared use by stdio and SSE.
pub enum DispatchResult {
    Success(serde_json::Value),
    Error(i32, String),
}

/// Pure dispatch: maps tool name + args → result or error. No I/O.
pub fn dispatch_tool_call(name: &str, args: &serde_json::Map<String, serde_json::Value>) -> DispatchResult {
    match name {
        "reliary_search" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let db_path = format!("{}/.reliary/index.sqlite", path.trim_end_matches('/'));
            match rusqlite::Connection::open(&db_path) {
                Ok(db) => {
                    let _ = db.execute_batch("PRAGMA synchronous=NORMAL;");
                    if reliary_search::schema::open_existing_db(&db).is_ok() {
                        let results = reliary_search::search::search_fts5(&db, query, 10);
                        DispatchResult::Success(serde_json::json!({
                            "content": [{ "type": "text", "text": serde_json::to_string(&results.iter().map(|r| serde_json::json!({"file": r.file, "score": r.score})).collect::<Vec<_>>()).unwrap_or_default() }]
                        }))
                    } else {
                        let tokens = reliary_search::tokenize(query);
                        DispatchResult::Success(serde_json::json!({
                            "content": [{ "type": "text", "text": serde_json::json!({"results": [], "note": "no index — run index first", "stemmed": tokens.iter().map(|t| reliary_search::porter_stem(t)).collect::<Vec<_>>()}).to_string() }]
                        }))
                    }
                }
                Err(e) => DispatchResult::Error(-1, format!("cannot open index: {}", e)),
            }
        }
        "reliary_compress" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let compressed = reliary_compress::compress_reasoning(text, None);
            let result = serde_json::json!({
                "compressed": compressed,
                "original_len": text.len(),
                "compressed_len": compressed.as_ref().map(|c| c.len()).unwrap_or(0),
            });
            DispatchResult::Success(serde_json::json!({
                "content": [{ "type": "text", "text": serde_json::to_string(&result).unwrap_or_default() }]
            }))
        }
        "reliary_risk" => {
            let file = args.get("file").and_then(|v| v.as_str()).unwrap_or("");
            if let Ok(meta) = std::fs::metadata(file) {
                if meta.len() > 10_000_000 {
                    return DispatchResult::Error(-1, "file too large".into());
                }
            }
            match std::fs::read_to_string(file) {
                Ok(content) => {
                    let risk = reliary_risk::compute_file_risk(file, &content);
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::json!({"file": risk.file, "risk": format!("{:?}", risk.risk), "reason": risk.reason}).to_string() }]
                    }))
                }
                Err(e) => DispatchResult::Error(-1, format!("cannot read {}: {}", file, e)),
            }
        }
        "reliary_fix" => {
            let file = args.get("file").and_then(|v| v.as_str()).unwrap_or("");
            let old = args.get("old").and_then(|v| v.as_str()).unwrap_or("");
            let new = args.get("new").and_then(|v| v.as_str()).unwrap_or("");
            if let Ok(meta) = std::fs::metadata(file) {
                if meta.len() > 10_000_000 {
                    return DispatchResult::Error(-1, "file too large".into());
                }
            }
            match std::fs::read_to_string(file) {
                Ok(content) => {
                    let fixes = vec![(old.to_string(), new.to_string())];
                    let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);
                    if count > 0 {
                        if reliary_core::atomic_write(file, &modified).is_ok() {
                            DispatchResult::Success(serde_json::json!({
                                "content": [{ "type": "text", "text": serde_json::json!({"success": true, "replacements": count, "file": file}).to_string() }]
                            }))
                        } else {
                            DispatchResult::Error(-1, format!("cannot write: {}", std::io::Error::last_os_error()))
                        }
                    } else {
                        DispatchResult::Error(-1, "no matches found".into())
                    }
                }
                Err(e) => DispatchResult::Error(-1, format!("cannot read {}: {}", file, e)),
            }
        }
        "reliary_dead" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let min_confidence = args.get("confidence").and_then(|v| v.as_str()).unwrap_or("all");
            let config = reliary_dead::DeadConfig::default();
            let mut candidates = Vec::new();
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let fp = entry.path();
                    if fp.extension().map(|e| e == "py" || e == "rs" || e == "js").unwrap_or(false) {
                        if let Some(p) = fp.to_str() {
                            if let Ok(content) = std::fs::read_to_string(p) {
                                candidates.extend(reliary_dead::analyze_file(p, &content, &config));
                            }
                        }
                    }
                }
            }
            let filtered: Vec<_> = candidates.iter().filter(|c| {
                match min_confidence {
                    "high" => c.confidence == reliary_dead::Confidence::High,
                    "medium" => c.confidence == reliary_dead::Confidence::High || c.confidence == reliary_dead::Confidence::Medium,
                    _ => true,
                }
            }).collect();
            let high = filtered.iter().filter(|c| c.confidence == reliary_dead::Confidence::High).count();
            let medium = filtered.iter().filter(|c| c.confidence == reliary_dead::Confidence::Medium).count();
            let low = filtered.iter().filter(|c| c.confidence == reliary_dead::Confidence::Low).count();
            let top: Vec<_> = filtered.iter().take(limit).map(|c| {
                let conf_str = match c.confidence {
                    reliary_dead::Confidence::High => "high",
                    reliary_dead::Confidence::Medium => "medium",
                    reliary_dead::Confidence::Low => "low",
                };
                serde_json::json!({"name": c.name, "file": c.file, "line": c.line, "confidence": conf_str})
            }).collect();
            let mut response_obj = serde_json::json!({
                "total": filtered.len(),
                "high": high,
                "medium": medium,
                "low": low,
                "items": top,
            });
            if filtered.len() > limit {
                if let Some(obj) = response_obj.as_object_mut() {
                    obj.insert("truncated".to_string(), serde_json::json!(true));
                    obj.insert("limit".to_string(), serde_json::json!(limit));
                }
            }
            DispatchResult::Success(serde_json::json!({
                "content": [{ "type": "text", "text": serde_json::to_string(&response_obj).unwrap_or_default() }]
            }))
        }
        "reliary_heal" => {
            let file = args.get("file").and_then(|v| v.as_str()).unwrap_or("");
            let old = args.get("old").and_then(|v| v.as_str()).unwrap_or("");
            let new = args.get("new").and_then(|v| v.as_str()).unwrap_or("");
            let workdir = args.get("workdir").and_then(|v| v.as_str()).unwrap_or(".");
            match crate::heal::heal_fix(file, old, new, workdir) {
                Ok(msg) => DispatchResult::Success(serde_json::json!({
                    "content": [{ "type": "text", "text": serde_json::json!({"success": true, "message": msg}).to_string() }]
                })),
                Err(e) => DispatchResult::Error(-1, e),
            }
        }
        "reliary_prior" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let prior = match std::fs::read_to_string(format!("{}/.reliary/prior_block", path.trim_end_matches('/'))) {
                Ok(p) => p.trim().to_string(),
                Err(_) => String::new(),
            };
            DispatchResult::Success(serde_json::json!({
                "content": [{ "type": "text", "text": serde_json::json!({"prior": prior}).to_string() }]
            }))
        }
        _ => DispatchResult::Error(-32601, format!("unknown tool: {}", name)),
    }
}

// ── Stdio transport (fallback, always available) ──

fn handle_tool_call_stdio(id: u64, params: &serde_json::Value) {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").and_then(|v| v.as_object()).cloned().unwrap_or_default();

    match dispatch_tool_call(name, &args) {
        DispatchResult::Success(result) => respond(id, result),
        DispatchResult::Error(code, message) => respond_error(id, code, &message),
    }
}

pub fn serve_stdio() {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() { continue; }

        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let id = msg.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");

        match method {
            "initialize" => {
                respond(id, serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "reliary", "version": env!("CARGO_PKG_VERSION") }
                }));
            }
            "notifications/initialized" => {}
            "tools/list" => {
                respond(id, serde_json::json!({ "tools": tool_definitions() }));
            }
            "tools/call" => {
                let params = match msg.get("params") {
                    Some(p) => p,
                    None => { respond_error(id, -32602, "missing params"); continue; }
                };
                handle_tool_call_stdio(id, params);
            }
            _ => {
                if !method.starts_with("notifications/") {
                    respond_error(id, -32601, &format!("method not found: {}", method));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_definitions_have_required_fields() {
        let tools = tool_definitions();
        assert!(!tools.is_empty(), "should have at least one tool");
        for t in &tools {
            let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
            assert!(!name.is_empty(), "each tool needs a name");
            assert!(name.starts_with("reliary_"), "tool name should start with reliary_: {}", name);
            assert!(t.get("description").is_some(), "tool {} needs a description", name);
            assert!(t.get("inputSchema").is_some(), "tool {} needs inputSchema", name);
        }
    }

    #[test]
    fn test_tool_list_response_format() {
        let tools = tool_definitions();
        let response = serde_json::json!({ "tools": tools });
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("reliary_search"));
        assert!(json.contains("reliary_heal"));
        assert!(json.contains("reliary_prior"));
    }

    #[test]
    fn test_handle_tool_call_unknown_tool() {
        let params = serde_json::json!({
            "name": "nonexistent_tool",
            "arguments": {}
        });
        handle_tool_call_stdio(1, &params); // Should not panic
    }

    #[test]
    fn test_handle_tool_call_search_missing_args() {
        let params = serde_json::json!({
            "name": "reliary_search",
            "arguments": {}
        });
        handle_tool_call_stdio(1, &params); // Should not panic
    }

    #[test]
    fn test_handle_tool_call_compress() {
        let params = serde_json::json!({
            "name": "reliary_compress",
            "arguments": { "text": "hello world" }
        });
        handle_tool_call_stdio(1, &params); // Should not panic
    }

    #[test]
    fn test_dispatch_tool_call_pure() {
        // Test pure dispatch without I/O
        let result = dispatch_tool_call("reliary_compress", &Default::default());
        match result {
            DispatchResult::Success(_) => {},
            DispatchResult::Error(_, _) => panic!("expected success"),
        }
        let result = dispatch_tool_call("nonexistent", &Default::default());
        match result {
            DispatchResult::Success(_) => panic!("expected error"),
            DispatchResult::Error(code, _) => assert_eq!(code, -32601),
        }
    }
}
