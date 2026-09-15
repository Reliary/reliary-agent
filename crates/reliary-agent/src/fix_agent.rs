// reliary fix — autonomous bug-fix agent.
// Self-contained: LLM (DeepSeek via reqwest) drives reliary's code-intel
// library functions to locate code, applies edits via the grammar-free edit
// primitive, and runs a verifier. Single binary, no external agent.

use serde_json::{json, Value};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// DeepSeek chat client (Phase 1)
// ---------------------------------------------------------------------------

/// Minimal chat-completions request/response over reqwest::blocking.
pub struct DeepSeekClient {
    client: reqwest::blocking::Client,
    api_key: String,
    model: String,
}

#[derive(Clone)]
pub struct ChatMsg {
    pub role: String, // "system" | "user" | "assistant"
    pub content: String,
}

impl DeepSeekClient {
    pub fn new(model: &str) -> Option<Self> {
        let api_key = std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .or_else(load_deepseek_key_from_auth)
            .unwrap_or_default();
        if api_key.is_empty() {
            return None;
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .ok()?;
        Some(Self {
            client,
            api_key,
            model: model.to_string(),
        })
    }

    /// Non-streaming chat completion. Returns (text, usage_dict).
    pub fn complete(&self, msgs: &[ChatMsg]) -> Result<(String, Value), String> {
        let body = json!({
            "model": self.model,
            "messages": msgs.iter().map(|m| json!({"role": m.role, "content": m.content})).collect::<Vec<_>>(),
            "temperature": 0.2,
            "max_tokens": 4096,
        });
        // V74: retry transient failures (connection reset, 429, 5xx) three
        // times with backoff — a single blip previously aborted the whole run.
        let mut attempt = 0u32;
        let resp = loop {
            attempt += 1;
            match self
                .client
                .post("https://api.deepseek.com/chat/completions")
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
            {
                Ok(r) if (r.status().is_server_error() || r.status().as_u16() == 429) && attempt < 3 => {
                    std::thread::sleep(std::time::Duration::from_millis(500 * attempt as u64));
                    continue;
                }
                Ok(r) => break r,
                Err(e) if attempt < 3 => {
                    let _ = e;
                    std::thread::sleep(std::time::Duration::from_millis(500 * attempt as u64));
                }
                Err(e) => return Err(format!("reqwest: {}", e)),
            }
        };
        let status = resp.status();
        let text = resp.text().map_err(|e| format!("read body: {}", e))?;
        if !status.is_success() {
            let snippet: String = text.chars().take(300).collect();
            return Err(format!("HTTP {}: {}", status, snippet));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("parse: {}", e))?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let usage = v.get("usage").cloned().unwrap_or(Value::Null);
        Ok((content, usage))
    }
}

/// Fall back to the opencode auth.json key store (bench convention).
fn load_deepseek_key_from_auth() -> Option<String> {
    let mut p = PathBuf::from(std::env::var("HOME").ok()?);
    p.push(".local/share/opencode/auth.json");
    let raw = std::fs::read_to_string(p).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v["deepseek"]["key"].as_str().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Tool dispatch to the search crate's library functions
// ---------------------------------------------------------------------------

/// A single tool result fed back into the LLM.
#[allow(dead_code)]
pub struct ToolResult {
    pub name: String,
    pub output: String,
}

/// Locate the index connection + root for a project dir.
fn open_index(path: &str) -> rusqlite::Result<rusqlite::Connection> {
    // trust/build lazily via lazy_occurrence if needed; the index lives at path/.reliary/index.sqlite
    rusqlite::Connection::open(format!("{}/.reliary/index.sqlite", path))
}

/// Dispatch one tool call by name to the search library. Grammar-free; mirrors
/// the MCP handlers but returns plain text (one-line style) for the LLM.
pub fn dispatch(path: &str, name: &str, args: &Value) -> String {
    let sym = args["name"].as_str().unwrap_or("").to_string();
    match name {
        "search" => {
            let q = args["query"].as_str().unwrap_or(&sym);
            let db = match open_index(path) {
                Ok(d) => d,
                Err(e) => return format!("(search error: {})", e),
            };
            let hits = reliary_search::search::search_fts5(&db, q, 8);
            if hits.is_empty() {
                "(no search results)".to_string()
            } else {
                let mut out = String::new();
                for h in hits.iter().take(8) {
                    let f = std::path::Path::new(&h.file)
                        .file_name().map(|x| x.to_string_lossy().to_string())
                        .unwrap_or_else(|| h.file.clone());
                    out.push_str(&format!("{} ({:.2})\n", f, h.score));
                }
                out
            }
        }
        "find_references" => {
            let def_only = args["def_only"].as_bool().unwrap_or(false);
            let usage_only = args["usage_only"].as_bool().unwrap_or(false);
            if def_only {
                definition_lookup(path, &sym)
            } else if usage_only {
                callers_output(path, &sym)
            } else {
                references_output(path, &sym)
            }
        }
        "call_graph" => {
            let direction = args["direction"].as_str().unwrap_or("outbound");
            call_graph_output(path, &sym, direction)
        }
        "methods" | "list_methods" => methods_output(path, &sym),
        "dead_code" => dead_code_output(path, args["path"].as_str().unwrap_or(".")),
        "describe" => describe_output(path, &sym),
        "similar" => similar_output(path, &sym),
        // V74: the edit tool is handled by the caller; report success so the
        // model isn't told "(unknown tool: edit)" after a successful edit.
        "edit" => "(edit applied)".to_string(),
        other => format!("(unknown tool: {})", other),
    }
}

fn definition_output_common(path: &str, sym: &str) -> Option<(String, i32, String)> {
    // Reuse type_flow candidate definitions: first is_def hit wins.
    let db = open_index(path).ok()?;
    let c = reliary_search::type_flow::top_candidate_definitions(&db, sym)
        .into_iter().next()?;
    let (fp, ln, _) = c;
    let short = std::path::Path::new(&fp)
        .file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or(fp.clone());
    Some((short, ln + 1, fp))
}

fn definition_lookup(path: &str, sym: &str) -> String {
    match definition_output_common(path, sym) {
        Some((f, line, _)) => format!("{} is defined at {}:{}", sym, f, line),
        None => format!("No definition found for {}", sym),
    }
}

fn callers_output(path: &str, sym: &str) -> String {
    // usage_only: non-def occurrences (call sites).
    let db = match open_index(path) { Ok(d) => d, Err(_) => return format!("No callers found for {}", sym) };
    let pid = match reliary_search::symbol::phrase_id_for(&db, sym) {
        Ok(Some(p)) => p,
        _ => return format!("No callers found for {}", sym),
    };
    let Ok(mut stmt) = db.prepare(
        "SELECT f.file_path, o.line FROM occurrence o JOIN file_map f ON f.id=o.file_id WHERE o.phrase_id=?1 AND o.is_def=0 LIMIT 12",
    ) else { return format!("No callers found for {}", sym) };
    let Ok(rows) = stmt.query_map(rusqlite::params![pid], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?))
    }) else { return format!("No callers found for {}", sym) };
    let mut out = String::new();
    let mut n = 0;
    for row in rows.flatten() {
        let (fp, line) = row;
        let f = std::path::Path::new(&fp).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or(fp);
        out.push_str(&format!("{}:{} ", f, line + 1));
        n += 1;
    }
    if n == 0 {
        format!("No callers found for {}", sym)
    } else {
        format!("{} is called from: {}", sym, out.trim())
    }
}

fn references_output(path: &str, sym: &str) -> String {
    let db = match open_index(path) { Ok(d) => d, Err(_) => return format!("No references found for {}", sym) };
    let Some(pid) = reliary_search::symbol::phrase_id_for(&db, sym).ok().flatten() else {
        return format!("No references found for {}", sym);
    };
    let Ok(mut stmt) = db.prepare(
        "SELECT f.file_path, o.line FROM occurrence o JOIN file_map f ON f.id=o.file_id WHERE o.phrase_id=?1 LIMIT 12",
    ) else { return format!("No references found for {}", sym) };
    let Ok(rows) = stmt.query_map(rusqlite::params![pid], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?))
    }) else { return format!("No references found for {}", sym) };
    let mut out = String::new();
    let mut n = 0;
    for row in rows.flatten() {
        let (fp, line) = row;
        let f = std::path::Path::new(&fp).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or(fp);
        out.push_str(&format!("{}:{} ", f, line + 1));
        n += 1;
    }
    if n == 0 {
        format!("No references found for {}", sym)
    } else {
        format!("References to {}: {}", sym, out.trim())
    }
}

fn call_graph_output(path: &str, sym: &str, direction: &str) -> String {
    // Use callgraph_v2::build_call_graph for outbound callees.
    if direction == "outbound" {
        match open_index(path).and_then(|db| {
            reliary_search::callgraph_v2::build_call_graph(&db, sym, "", None, 1)
        }) {
            Ok(cg) if !cg.callees.is_empty() => {
                let names: Vec<String> = cg.callees.iter().take(10).map(|c| c.name.clone()).collect();
                format!("{} calls: {}", sym, names.join(", "))
            }
            _ => format!("No callees found for {}", sym),
        }
    } else {
        callers_output(path, sym)
    }
}

fn methods_output(path: &str, sym: &str) -> String {
    match open_index(path).and_then(|db| {
        reliary_search::callgraph_v2::find_methods_on(&db, sym)
    }) {
        Ok(res) if !res.methods.is_empty() => {
            let names: Vec<String> = res.methods.iter().take(15).map(|m| m.name.clone()).collect();
            format!("Methods on {}: {}", sym, names.join(", "))
        }
        _ => format!("No methods found on {}", sym),
    }
}

fn dead_code_output(path: &str, scope: &str) -> String {
    // Wrap dead_symbols. Simple: scan index for is_def=1 with no is_def=0 refs.
    let db = match open_index(path) { Ok(d) => d, Err(e) => return format!("(dead code error: {})", e) };
    let Ok(mut stmt) = db.prepare(
        "SELECT p.phrase FROM occurrence o JOIN phrases p ON p.id=o.phrase_id WHERE o.is_def=1 AND o.phrase_id NOT IN (SELECT phrase_id FROM occurrence WHERE is_def=0) LIMIT 20",
    ) else { return format!("No dead code found in {}", scope) };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return format!("No dead code found in {}", scope);
    };
    let dead: Vec<String> = rows.flatten().collect();
    if dead.is_empty() {
        format!("No dead code found in {}", scope)
    } else {
        format!("Dead code in {}: {}", scope, dead.join(", "))
    }
}

fn describe_output(path: &str, sym: &str) -> String {
    // Fall back to definition + methods for a compact describe.
    let def = definition_lookup(path, sym);
    let methods = methods_output(path, sym);
    format!("{} | {}", def, methods)
}

fn similar_output(path: &str, sym: &str) -> String {
    match open_index(path).map(|db| reliary_search::similar::find_similar(&db, sym, 5)) {
        Ok(list) if !list.is_empty() => {
            let names: Vec<String> = list.iter().map(|f| f.name.clone()).collect();
            format!("Similar to {}: {}", sym, names.join(", "))
        }
        _ => format!("No similar symbols to {}", sym),
    }
}

// ---------------------------------------------------------------------------
// Agent loop
// ---------------------------------------------------------------------------

pub fn run(path: &str, task: &str, max_iters: usize, dry_run: bool, json_out: bool, verify: Option<&str>) -> Result<i32, String> {
    let Some(client) = DeepSeekClient::new("deepseek-chat") else {
        return Err("DeepSeek API key not found (set DEEPSEEK_API_KEY or opencode auth.json)".into());
    };
    let mut msgs: Vec<ChatMsg> = vec![
        ChatMsg {
            role: "system".into(),
            content: SYSTEM_PROMPT.to_string(),
        },
        ChatMsg {
            role: "user".into(),
            content: format!("Task: {}\nProject: {}\n", task, path),
        },
    ];

    let mut applied: Vec<String> = Vec::new();
    for _it in 0..max_iters {
        let (reply, _usage) = client.complete(&msgs)?;
        // Detect a tool_call request from the assistant (any format).
        // The table format uses full-width ｜ separators — parse it on the
        // RAW reply before the ｜-strip makes it unreachable.
        let parsed = parse_tool_call_table(&reply)
            .or_else(|| {
                let cleaned = reply.replace('｜', "");
                parse_tool_call(&cleaned)
                    .or_else(|| parse_tool_call_json(&cleaned))
                    .or_else(|| parse_tool_call_named(&cleaned))
                    .or_else(|| parse_tool_call_invoke(&cleaned))
                    .or_else(|| parse_tool_call_bare(&cleaned))
            });
        if let Some((tool, args)) = parsed {
            let out = dispatch(path, &tool, &args);
            if json_out { println!("{{\"tool\":\"{}\",\"output\":\"{}\"}}", tool, out.escape_default()); }
            else { eprintln!("[tool:{}]\n{}", tool, out); }
            // Apply edits if the tool is an edit request.
            if tool == "edit" {
                let file = args["file"].as_str().unwrap_or("");
                let old = args["old"].as_str().unwrap_or("");
                let new = args["new"].as_str().unwrap_or("");
                let edit_result = if dry_run {
                    Ok(true)
                } else {
                    reliary_edit::apply_edit(path, file, old, new)
                };
                match edit_result {
                    Ok(true) => {
                        let old_t = old.chars().take(30).collect::<String>();
                        let new_t = new.chars().take(30).collect::<String>();
                        applied.push(format!("{}: {} -> {}", file, old_t, new_t));
                    }
                    Ok(false) => {
                        // Feed the failure back so the model can retry with
                        // corrected old-text instead of silently "succeeding".
                        msgs.push(ChatMsg { role: "assistant".into(), content: reply });
                        msgs.push(ChatMsg { role: "user".into(), content: format!(
                            "<tool_result>edit FAILED: old text not found in {}. Provide the exact current text (or a whitespace-insensitive match).</tool_result>", file) });
                        continue;
                    }
                    Err(e) => {
                        msgs.push(ChatMsg { role: "assistant".into(), content: reply });
                        msgs.push(ChatMsg { role: "user".into(), content: format!(
                            "<tool_result>edit ERROR: {}</tool_result>", e) });
                        continue;
                    }
                }
            }
            msgs.push(ChatMsg { role: "assistant".into(), content: reply });
            msgs.push(ChatMsg { role: "user".into(), content: format!("<tool_result>{}</tool_result>", out) });
        } else if reply.trim().is_empty() {
            return Err("LLM returned empty response".into());
        } else {
            // Final answer.
            if json_out { println!("{{\"final\":\"{}\"}}", reply.escape_default()); }
            else { println!("{}", reply); }
            if !applied.is_empty() {
                eprintln!("Edits applied: {}", applied.join("; "));
            }
            // V61: run the user's verify template after edits and report
            // pass/fail. `{file}` is substituted with the first edited file.
            if let Some(tpl) = verify {
                if !applied.is_empty() && !dry_run {
                    let first_file = applied[0].split(':').next().unwrap_or("");
                    let cmd = tpl.replace("{file}", first_file);
                    let status = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(&cmd)
                        .current_dir(path)
                        .status();
                    match status {
                        Ok(s) if s.success() => {
                            eprintln!("Verify PASSED: {}", cmd);
                        }
                        Ok(s) => {
                            eprintln!("Verify FAILED (exit {}): {}", s.code().unwrap_or(-1), cmd);
                            return Err(format!("verify failed: {}", cmd));
                        }
                        Err(e) => {
                            eprintln!("Verify ERROR: {} — {}", cmd, e);
                        }
                    }
                } else if applied.is_empty() && !dry_run {
                    eprintln!("No edits applied — skipping verify");
                }
            }
            return Ok(0);
        }
    }
    Err("Iteration limit reached without a final answer".into())
}

/// Parse an assistant reply that requests a tool call, if any.
/// Accepts both `<tool_call><name>X</name><args>{...}</args></tool_call>`
/// and the markdown-table separator style the model tends to emit:
///   ｜tool name="X"｜ ... ｜parameter name="k"｜v｜
fn parse_tool_call(reply: &str) -> Option<(String, Value)> {
    // Try XML first.
    let name = reply.split("<name>").nth(1)?.split("</name>").next()?.trim();
    let args_part = reply.split("<args>").nth(1)?.split("</args>").next()?;
    let args: Value = serde_json::from_str(args_part).unwrap_or(Value::Object(Default::default()));
    Some((name.to_string(), args))
}

/// Fallback parser: extract a JSON tool request from the reply if it appears
/// as `{"name":"search","arguments":{...}}` or `tool:search({...})`.
pub fn parse_tool_call_json(reply: &str) -> Option<(String, Value)> {
    // `tool:name({"query":"x"})` or `name(args={...})`
    let body = reply.trim();
    // Match `{...}` that contains a "name" and "arguments"/"args".
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    // V74: a stray '}' before the first '{' made start > end and the slice
    // panicked. Guard the range; nothing to parse in that case.
    if start > end { return None; }
    let json_part = &body[start..=end];
    if let Ok(v) = serde_json::from_str::<Value>(json_part) {
        let name = v["name"].as_str()
            .or_else(|| v["tool"].as_str())?;
        let args = v["arguments"].as_object()
            .or_else(|| v["args"].as_object())
            .cloned()
            .unwrap_or_default();
        return Some((name.to_string(), Value::Object(args)));
    }
    None
}

/// Parse the markdown-table separator format the model emits:
///   ｜tool name="search"｜
///   ｜parameter name="query"｜classify_structural｜
fn parse_tool_call_table(reply: &str) -> Option<(String, Value)> {
    if !reply.contains('｜') {
        return None;
    }
    let mut name = String::new();
    let mut args = serde_json::Map::new();
    for line in reply.lines() {
        let line = line.trim();
        if line.contains("tool name=\"") {
            name = line.split("name=\"").nth(1)?.split('"').next()?.to_string();
        } else if line.contains("parameter name=\"") {
            let key = line.split("name=\"").nth(1)?.split('"').next()?.to_string();
            // V74: value is the text after the closing `>` of the opening tag
            // (or the remainder of the line after name="..."). The old split on
            // "param" yielded `eter name="query"｜…`.
            let val = line.split('>').nth(1).unwrap_or("")
                .split("</parameter>").next().unwrap_or("")
                .trim_matches('|').trim().to_string();
            args.insert(key, Value::String(val));
        }
    }
    if name.is_empty() {
        None
    } else {
        Some((name, Value::Object(args)))
    }
}

/// Parse `<tool_call name="search"><args>{...}</args></tool_call>`.
fn parse_tool_call_named(reply: &str) -> Option<(String, Value)> {
    let body = reply.trim();
    if !body.starts_with("<tool_call") {
        return None;
    }
    let name = body.split("name=\"").nth(1)?.split('"').next()?.to_string();
    let args_start = body.find("<args>")? + "<args>".len();
    let args_end = body[args_start..].find("</args>")? + args_start;
    let args_json = &body[args_start..args_end];
    let args: Value = serde_json::from_str(args_json).ok()?;
    Some((name, args))
}

/// Parse the Anthropic-concise format: `<invoke name="search"><parameter
/// name="query">classify_structural</parameter></invoke>`
fn parse_tool_call_invoke(reply: &str) -> Option<(String, Value)> {
    let body = reply.trim();
    let open = body.find("<invoke name=\"")?;
    let rest = &body[open..];
    let name = rest.split("name=\"").nth(1)?.split('"').next()?.to_string();
    let mut args = serde_json::Map::new();
    let mut remainder = rest;
    while let Some(p) = remainder.find("<parameter name=\"") {
        let key = remainder[p..].split("name=\"").nth(1)?.split('"').next()?.to_string();
        let val_start = remainder[p..].find('>')? + p + 1;
        let close_tag = remainder[val_start..].find("</parameter>")?;
        let val = &remainder[val_start..val_start + close_tag];
        args.insert(key, Value::String(val.to_string()));
        remainder = &remainder[val_start + close_tag + "</parameter>".len()..];
    }
    if name.is_empty() {
        None
    } else {
        Some((name, Value::Object(args)))
    }
}

/// Parse the bare-tag format: `<search><query>classify_structural</query></search>`
/// or `<tool_name><param_name>value</param_name></tool_name>`.
fn parse_tool_call_bare(reply: &str) -> Option<(String, Value)> {
    let body = reply.trim();
    // First line is the tool name inside <...>
    let first = body.lines().next()?.trim();
    if !first.starts_with('<') || !first.ends_with('>') {
        return None;
    }
    let name = &first[1..first.len() - 1];
    if name.is_empty() {
        return None;
    }
    // Collect <key>value</key> params from the remainder.
    let mut args = serde_json::Map::new();
    let inner = &body[first.len()..];
    let mut rest = inner;
    while let Some(open) = rest.find('<') {
        let close = rest[open..].find('>')?;
        let key = &rest[open + 1..open + close];
        if key.starts_with('/') {
            break; // closing tag
        }
        let val_start = open + close + 1;
        let close_tag = rest[val_start..].find(&format!("</{}>", key))?;
        let val = &rest[val_start..val_start + close_tag];
        args.insert(key.to_string(), Value::String(val.to_string()));
        rest = &rest[val_start + close_tag + key.len() + 3..];
    }
    if args.is_empty() {
        None
    } else {
        Some((name.to_string(), Value::Object(args)))
    }
}

const SYSTEM_PROMPT: &str = r#"You are reliary-fix, an autonomous code repair agent.
You have these tools (respond with a tool_call block when you need info):
  <name>search</name><args>{"query":"..."}</args>
  <name>find_references</name><args>{"name":"X","def_only":true}</args>
  <name>find_references</name><args>{"name":"X","usage_only":true}</args>
  <name>call_graph</name><args>{"name":"X","direction":"outbound"}</args>
  <name>methods</name><args>{"name":"X"}</args>
  <name>dead_code</name><args>{"path":"..."}</args>
  <name>describe</name><args>{"name":"X"}</args>
  <name>edit</name><args>{"file":"src/x.rs","old":"exact text","new":"replacement"}</args>

Rules:
1. Investigate the task with the read tools, then apply an edit.
2. After editing, run the verifier yourself (cargo check) via the tool result.
3. When satisfied, reply in plain prose with your final answer (no tool block).
Do NOT invent file:line numbers. Base everything on tool output."#;
