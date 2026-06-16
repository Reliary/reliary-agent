/// Minimal MCP server for reliary-agent.
/// Exposes tools: search, compress, risk, fix, dead, heal, prior

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
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "reliary",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }));
            }
            "notifications/initialized" => {}  // noop
            "tools/list" => {
                respond(id, serde_json::json!({
                    "tools": [
                        { "name": "reliary_search", "description": "BM25 grammar-free code search", "inputSchema": { "type": "object", "properties": { "query": {"type": "string"}, "path": {"type": "string"} } } },
                        { "name": "reliary_compress", "description": "IR reasoning compression", "inputSchema": { "type": "object", "properties": { "text": {"type": "string"} } } },
                        { "name": "reliary_risk", "description": "Pre-edit risk analysis", "inputSchema": { "type": "object", "properties": { "file": {"type": "string"} } } },
                        { "name": "reliary_fix", "description": "Pattern-based file fix", "inputSchema": { "type": "object", "properties": { "file": {"type": "string"}, "old": {"type": "string"}, "new": {"type": "string"} } } },
                        { "name": "reliary_dead", "description": "Grammar-free dead code detection (compact summary + top-N)", "inputSchema": { "type": "object", "properties": { "path": {"type": "string"}, "limit": {"type": "integer", "default": 10}, "confidence": {"type": "string", "enum": ["all","high","medium","low"], "default": "all"} } } },
                        { "name": "reliary_heal", "description": "Apply edit with self-healing", "inputSchema": { "type": "object", "properties": { "file": {"type": "string"}, "old": {"type": "string"}, "new": {"type": "string"}, "workdir": {"type": "string"} } } },
                        { "name": "reliary_prior", "description": "Chronicled project state", "inputSchema": { "type": "object", "properties": { "path": {"type": "string"} } } },
                    ]
                }));
            }
            "tools/search" => {
                let params = msg.get("params").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let path = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let db_path = format!("{}/.reliary/index.sqlite", path.trim_end_matches('/'));
                if let Ok(db) = rusqlite::Connection::open(&db_path) {
                    if reliary_search::schema::open_existing_db(&db).is_ok() {
                        let results = reliary_search::search::search_fts5(&db, query, 10);
                        respond(id, serde_json::json!({
                            "results": results.iter().map(|r| serde_json::json!({"file": r.file, "score": r.score})).collect::<Vec<_>>()
                        }));
                    } else {
                        let tokens = reliary_search::tokenize(query);
                        respond(id, serde_json::json!({
                            "results": [],
                            "note": "no index at path — run index first",
                            "stemmed": tokens.iter().map(|t| reliary_search::porter_stem(t)).collect::<Vec<_>>()
                        }));
                    }
                } else {
                    respond_error(id, -1, &format!("cannot open index at {}", db_path));
                }
            }
            "tools/compress" => {
                let params = msg.get("params").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let compressed = reliary_compress::compress_reasoning(text, None);
                respond(id, serde_json::json!({
                    "compressed": compressed,
                    "original_len": text.len(),
                    "compressed_len": compressed.as_ref().map(|c| c.len()).unwrap_or(0),
                }));
            }
            "tools/risk" => {
                let params = msg.get("params").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                let file = params.get("file").and_then(|v| v.as_str()).unwrap_or("");
                match std::fs::read_to_string(file) {
                    Ok(content) => {
                        let risk = reliary_risk::compute_file_risk(file, &content);
                        respond(id, serde_json::json!({ "file": risk.file, "risk": format!("{:?}", risk.risk), "reason": risk.reason }));
                    }
                    Err(e) => respond_error(id, -1, &format!("cannot read {}: {}", file, e)),
                }
            }
            "tools/fix" => {
                let params = msg.get("params").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                let file = params.get("file").and_then(|v| v.as_str()).unwrap_or("");
                let old = params.get("old").and_then(|v| v.as_str()).unwrap_or("");
                let new = params.get("new").and_then(|v| v.as_str()).unwrap_or("");
                match std::fs::read_to_string(file) {
                    Ok(content) => {
                        let fixes = vec![(old.to_string(), new.to_string())];
                        let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);
                        if count > 0 {
                            if let Err(e) = std::fs::write(file, &modified) {
                                respond_error(id, -1, &format!("cannot write {}: {}", file, e));
                            } else {
                                respond(id, serde_json::json!({ "success": true, "replacements": count, "file": file }));
                            }
                        } else {
                            respond_error(id, -1, "no matches found");
                        }
                    }
                    Err(e) => respond_error(id, -1, &format!("cannot read {}: {}", file, e)),
                }
            }
            "tools/dead" => {
                let params = msg.get("params").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                let path = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                let min_confidence = params.get("confidence").and_then(|v| v.as_str()).unwrap_or("all");
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

                // Filter by confidence
                let filtered: Vec<_> = candidates.iter().filter(|c| {
                    match min_confidence {
                        "high" => c.confidence == reliary_dead::Confidence::High,
                        "medium" => c.confidence == reliary_dead::Confidence::High || c.confidence == reliary_dead::Confidence::Medium,
                        "low" => true,
                        _ => true,
                    }
                }).collect();

                // Count by confidence
                let high = filtered.iter().filter(|c| c.confidence == reliary_dead::Confidence::High).count();
                let medium = filtered.iter().filter(|c| c.confidence == reliary_dead::Confidence::Medium).count();
                let low = filtered.iter().filter(|c| c.confidence == reliary_dead::Confidence::Low).count();

                // Top-N items
                let top: Vec<_> = filtered.iter().take(limit).map(|c| {
                    let conf_str = match c.confidence {
                        reliary_dead::Confidence::High => "high",
                        reliary_dead::Confidence::Medium => "medium",
                        reliary_dead::Confidence::Low => "low",
                    };
                    serde_json::json!({"name": c.name, "file": c.file, "line": c.line, "confidence": conf_str})
                }).collect();

                let mut response = serde_json::json!({
                    "total": filtered.len(),
                    "high": high,
                    "medium": medium,
                    "low": low,
                    "items": top,
                });
                if filtered.len() > limit {
                    let obj = response.as_object_mut().unwrap();
                    obj.insert("truncated".to_string(), serde_json::json!(true));
                    obj.insert("limit".to_string(), serde_json::json!(limit));
                }
                respond(id, response);
            }
            "tools/heal" => {
                let params = msg.get("params").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                let file = params.get("file").and_then(|v| v.as_str()).unwrap_or("");
                let old = params.get("old").and_then(|v| v.as_str()).unwrap_or("");
                let new = params.get("new").and_then(|v| v.as_str()).unwrap_or("");
                let workdir = params.get("workdir").and_then(|v| v.as_str()).unwrap_or(".");
                match crate::heal::heal_fix(file, old, new, workdir) {
                    Ok(msg) => respond(id, serde_json::json!({ "success": true, "message": msg })),
                    Err(e) => respond_error(id, -1, &e),
                }
            }
            "tools/prior" => {
                let params = msg.get("params").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                let path = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let prior = match std::fs::read_to_string(format!("{}/.reliary/prior_block", path.trim_end_matches('/'))) {
                    Ok(p) => p.trim().to_string(),
                    Err(_) => String::new(),
                };
                respond(id, serde_json::json!({ "prior": prior }));
            }
            _ => {
                if !method.starts_with("notifications/") {
                    respond_error(id, -32601, &format!("method not found: {}", method));
                }
            }
        }
    }
}
