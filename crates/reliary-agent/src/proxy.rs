//! Provider-agnostic proxy with axum — true SSE streaming support.
// Auth-based routing via routes.rs. No model lists, no provider detection.

use axum::{
    Router, extract::Query, http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use bytes::Bytes;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, LazyLock};
use serde_json::Value;
use tracing::{info, warn, error};

// Shared HTTP client with connection pooling — eliminates per-request TCP+TLS handshake.
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .pool_max_idle_per_host(10)
        .build()
        .expect("reqwest::Client")
});

// Compression dictionary loaded once from FTS5 index — known project symbols
// survive compression while unknown fluff gets stripped.
static COMPRESSION_DICT: LazyLock<Option<reliary_compress::CompressionDict>> =
    LazyLock::new(crate::read_summary::load_dictionary);

// Synchronization for JSONL logging — prevents interleaved lines from concurrent requests.
static JSONL_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

static RESPONSE_CACHE: LazyLock<Mutex<HashMap<u64, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static DAEMON_STATE: LazyLock<Mutex<Option<Arc<crate::session_state::SessionState>>>> =
    LazyLock::new(|| Mutex::new(None));

fn get_state() -> Arc<crate::session_state::SessionState> {
    let guard = DAEMON_STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard.clone().unwrap_or_else(|| Arc::new(crate::session_state::SessionState::new(".")))
}

fn cache_key(auth: &str, body: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    auth.hash(&mut h);
    body.hash(&mut h);
    h.finish()
}

fn cached_response(auth: &str, body: &str) -> Option<String> {
    let key = cache_key(auth, body);
    RESPONSE_CACHE.lock().ok().and_then(|c| c.get(&key).cloned())
}

fn store_response(auth: &str, body: &str, response: &str) {
    let key = cache_key(auth, body);
    if let Ok(mut cache) = RESPONSE_CACHE.lock() {
        cache.insert(key, response.to_string());
        if cache.len() > 120 {
            let keys: Vec<u64> = cache.keys().copied().collect();
            for k in keys.iter().take(20) { cache.remove(k); }
        }
    }
}

fn resolve_upstream(auth_key: &str) -> Option<String> {
    if let Some(url) = crate::routes::discover_upstream(auth_key) {
        return Some(url);
    }
    if let Ok(url) = std::env::var("RELIARY_UPSTREAM_URL") {
        return Some(url);
    }
    None
}

fn extract_auth_key(headers: &HeaderMap) -> String {
    headers.get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.strip_prefix("Bearer ").unwrap_or(v).to_string())
        .unwrap_or_default()
}

fn daemon_cmd_str(cmd: &str) -> String {
    crate::daemon::daemon_handle_cmd_str(cmd, &get_state())
}

fn jsonl_log(entry: &serde_json::Value) {
    let _lock = JSONL_LOCK.lock().ok();  // GUARDED: intentional
    if let Ok(mut fh) = std::fs::OpenOptions::new()
        .create(true).append(true).open("/tmp/reliary_proxy.jsonl")
    {
        use std::io::Write;
        let _ = writeln!(fh, "{}", serde_json::to_string(entry).unwrap_or_default());  // GUARDED: intentional
    }
}

// ── History Compression Components ──

// Per-auth-key state — first-appearance freeze cache.
// `content_cache`: maps content hash → compressed version.
struct PerKeyState {
    content_cache: HashMap<u64, String>,
}

impl PerKeyState {
    fn new() -> Self {
        Self { content_cache: HashMap::new() }
    }

    /// Content hash for cache lookup.
    fn content_hash(content: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut h);
        h.finish()
    }
}

// Global per-auth-key state store
static PER_KEY_STATE: LazyLock<Mutex<HashMap<String, PerKeyState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn get_or_create_state(auth_key: &str) -> std::sync::MutexGuard<'static, HashMap<String, PerKeyState>> {
    let mut guard = PER_KEY_STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard.entry(auth_key.to_string()).or_insert_with(PerKeyState::new);
    guard
}

// Compress old assistant reasoning — strip verbose explanations, keep code blocks intact.
// Splits message into code blocks (```...```) and prose sections.
// Compresses prose, leaves code verbatim.
fn compress_assistant_text(text: &str, dict: Option<&reliary_compress::CompressionDict>) -> Option<String> {
    // First try full-text compress (works for prose-only with no code blocks)
    if let Some(compressed) = reliary_compress::compress_reasoning(text, dict) {
        return Some(compressed);
    }

    // Split on code blocks
    let mut parts: Vec<String> = Vec::new();
    let mut in_code = false;
    let mut code_buf = String::new();
    let mut prose_buf = String::new();

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_code {
                parts.push(code_buf.clone());
                code_buf.clear();
                in_code = false;
            } else {
                if !prose_buf.is_empty() {
                    parts.push(prose_buf.clone());
                    prose_buf.clear();
                }
                in_code = true;
                code_buf.push_str(line);
                code_buf.push('\n');
            }
        } else if in_code {
            code_buf.push_str(line);
            code_buf.push('\n');
        } else {
            prose_buf.push_str(line);
            prose_buf.push('\n');
        }
    }
    if in_code && !code_buf.is_empty() {
        parts.push(code_buf);
    } else if !prose_buf.is_empty() {
        parts.push(prose_buf);
    }

    // Compress each section: keep code verbatim, compress prose
    let mut result = String::new();
    let mut total_original = 0usize;
    let mut total_compressed = 0usize;

    for part in &parts {
        total_original += part.len();
        if part.contains("```") || part.len() < 50 {
            result.push_str(part);
            total_compressed += part.len();
        } else {
            let compressed = reliary_compress::compress_reasoning(part, dict);
            match compressed {
                Some(c) if c.len() < part.len() => {
                    result.push_str(&c);
                    result.push('\n');
                    total_compressed += c.len();
                }
                _ => {
                    result.push_str(part);
                    total_compressed += part.len();
                }
            }
        }
    }
    if total_original > 0 && total_compressed < total_original {
        Some(result)
    } else {
        None
    }
}

// Full sift pipeline: zone truncate → command output collapse → content compress → Maxwell gate.
// Handles any length, any content type (command output, file reads, search results, logs).
fn sift_compress_tool_result(content: &str) -> String {
    if content.len() < 200 { return content.to_string(); }

    // Step 1: Very large content — zone truncate first (keep head + tail, drop middle)
    let working = if content.lines().count() > 200 {
        reliary_sift::zone_truncate(content, 30, 15)
    } else {
        content.to_string()
    };

    // Step 2: Command output (cargo/test/npm) — collapse repeated runs
    let collapsed = reliary_output::compress_output(&working);
    if collapsed.len() < working.len() {
        return collapsed;
    }

    // Step 3: File content — classify + compress (grammar-free byte DFA)
    let lines = reliary_sift::classify_content(&working);
    if reliary_sift::looks_like_content(&lines) {
        let compressed = reliary_sift::compress_content(lines, true);
        let result = compressed.join("\n");
        if result.len() < working.len() {
            return result;
        }
    }

    // Step 4: MaxwellGate — if information-dense, don't force compression
    let gate = reliary_sift::MaxwellGate::default();
    if gate.score(&working).is_none() {
        return working;
    }

    working
}

// Compress the assistant message in an API response body before returning to agent.
// Parses JSON, finds choices[0].message.content, compresses, re-serializes.
// For SSE: scans final data chunk for content field, compresses in-place.
fn compress_response_body(body: &str, is_sse: bool) -> String {
    if body.len() < 500 { return body.to_string(); }

    if is_sse {
        // SSE: find the last data: line with content, compress it
        let mut lines: Vec<String> = body.lines().map(|s| s.to_string()).collect();
        for i in (0..lines.len()).rev() {
            let line = &lines[i];
            if !line.starts_with("data: ") { continue; }
            let json_str = &line[6..];
            if json_str == "[DONE]" { continue; }
            if let Ok(mut v) = serde_json::from_str::<Value>(json_str) {
                if let Some(choices) = v.get_mut("choices").and_then(|c| c.as_array_mut()) {
                    if let Some(choice) = choices.first_mut() {
                        // Try delta first, then message (streaming vs non-streaming format)
                        let compressed_content: Option<String> = {
                            let content = choice.get("delta")
                                .and_then(|d| d.get("content"))
                                .or_else(|| choice.get("message").and_then(|m| m.get("content")))
                                .and_then(|c| c.as_str());
                            if let Some(c) = content {
                                if c.len() > 300 {
                                    compress_assistant_text(c, COMPRESSION_DICT.as_ref())
                                } else { None }
                            } else { None }
                        };
                        if let Some(compressed) = compressed_content {
                            if let Some(delta) = choice.get_mut("delta") {
                                delta["content"] = Value::String(compressed);
                            } else if let Some(msg) = choice.get_mut("message") {
                                msg["content"] = Value::String(compressed);
                            }
                        }
                    }
                }
                lines[i] = format!("data: {}", serde_json::to_string(&v).unwrap_or_else(|_| json_str.to_string()));
                break;
            }
        }
        return lines.join("\n");
    }

    // Non-streaming JSON response
    if let Ok(mut v) = serde_json::from_str::<Value>(body) {
        if let Some(choices) = v.get_mut("choices").and_then(|c| c.as_array_mut()) {
            if let Some(choice) = choices.first_mut() {
                if let Some(msg) = choice.get_mut("message") {
                    if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                        if content.len() > 300 {
                            if let Some(compressed) = compress_assistant_text(content, COMPRESSION_DICT.as_ref()) {
                                msg["content"] = Value::String(compressed);
                                return serde_json::to_string(&v).unwrap_or_else(|_| body.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    body.to_string()
}

// First-appearance freeze compression: compress every message on first occurrence,
// cache the compressed version, and use the cached version forever after.
// This preserves KV cache stability — the compressed version is what the API/SDK
// has cached from the start.
fn compress_messages(messages: &mut [Value], state: &mut PerKeyState) -> (usize, usize) {
    let mut history_saved: usize = 0;
    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

        // Never compress user or system messages
        if role == "user" || role == "system" { continue; }

        let content = match msg.get("content") {
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };
        if content.len() < 100 { continue; }

        let hash = PerKeyState::content_hash(&content);

        // If we already have a cached version, use it
        if let Some(cached) = state.content_cache.get(&hash) {
            history_saved += content.len().saturating_sub(cached.len());
            msg["content"] = Value::String(cached.clone());
            continue;
        }

        // First occurrence: compress and cache
        let compressed = match role {
            "assistant" => compress_assistant_text(&content, COMPRESSION_DICT.as_ref()),
            "tool" | "toolResult" => {
                let sifted = sift_compress_tool_result(&content);
                if sifted.len() < content.len() { Some(sifted) } else { None }
            }
            _ => None,
        };

        if let Some(c) = compressed {
            if c.len() < content.len() && state.content_cache.len() < 200 {
                history_saved += content.len().saturating_sub(c.len());
                state.content_cache.insert(hash, c.clone());
                msg["content"] = Value::String(c);
            }
            // Don't cache uncompressed content — it grows without bound
        }
    }

    (history_saved, 0)
}

// ── Health / Ping ──

async fn health() -> impl IntoResponse {
    (StatusCode::OK, [("content-type", "application/json")], "{\"status\":\"ok\"}")
}

async fn ping() -> &'static str { "pong" }

// ── Daemon GET routes ──

async fn search_handler(Query(params): Query<HashMap<String, String>>) -> String {
    let q = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let p = params.get("path").map(|s| s.as_str()).unwrap_or(".");
    daemon_cmd_str(&format!("search {} {}", q, p))
}

async fn risk_handler(Query(params): Query<HashMap<String, String>>) -> String {
    let f = params.get("file").map(|s| s.as_str()).unwrap_or("");
    daemon_cmd_str(&format!("risk {}", f))
}

async fn compress_handler(Query(params): Query<HashMap<String, String>>) -> String {
    let t = params.get("text").map(|s| s.as_str()).unwrap_or("");
    daemon_cmd_str(&format!("compress {}", t))
}

async fn veto_handler(Query(params): Query<HashMap<String, String>>) -> String {
    let f = params.get("file").map(|s| s.as_str()).unwrap_or("");
    let t = params.get("text").map(|s| s.as_str()).unwrap_or("");
    daemon_cmd_str(&format!("veto {} {}", f, t))
}

async fn muzzle_handler(Query(params): Query<HashMap<String, String>>) -> String {
    let st = params.get("state").map(|s| s.as_str()).unwrap_or("");
    let s = get_state();
    match st {
        "on" => { s.set_muzzle(true); "muzzled\n".to_string() }
        "off" => { s.set_muzzle(false); "unmuzzled\n".to_string() }
        _ => "ERROR: state must be on|off\n".to_string()
    }
}

async fn prior_handler(Query(params): Query<HashMap<String, String>>) -> String {
    let p = params.get("path").map(|s| s.as_str()).unwrap_or(".");
    daemon_cmd_str(&format!("prior {}", p))
}

async fn read_summary_handler(Query(params): Query<HashMap<String, String>>) -> String {
    let f = params.get("file").map(|s| s.as_str()).unwrap_or("");
    daemon_cmd_str(&format!("read-summary {}", f))
}

async fn status_handler() -> &'static str { "ok\n" }

async fn who_calls_handler(Query(params): Query<HashMap<String, String>>) -> String {
    let file = params.get("file").map(|s| s.as_str()).unwrap_or("");
    let identifier = params.get("identifier").map(|s| s.as_str()).unwrap_or("");
    if file.is_empty() || identifier.is_empty() {
        return "[]".to_string();
    }
    // Resolve project root from file path
    let root = if let Some((r, _, _)) = crate::daemon::find_reliary_root(file) {
        r
    } else {
        return "[]".to_string();
    };
    let index_path = format!("{}/.reliary/index.sqlite", root);
    let db = match rusqlite::Connection::open(&index_path) {
        Ok(d) => d,
        Err(_) => return "[]".to_string(),
    };
    // Normalize paths: index stores relative paths
    let rel_file = file.trim_start_matches('/').trim_start_matches(root.trim_end_matches('/')).trim_start_matches('/');
    let callers = reliary_search::search::who_calls(&db, identifier, rel_file);
    if !callers.is_empty() {
        tracing::info!("who_calls: {} referenced by {} files for {}", identifier, callers.len(), rel_file);
    }
    serde_json::to_string(&callers).unwrap_or_else(|_| "[]".to_string())
}

// ── Proxy POST handler ──

async fn proxy_post(
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let auth_key = extract_auth_key(&headers);
    let upstream_url = match resolve_upstream(&auth_key) {
        Some(url) => url,
        None => return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error":"unknown api key"}))).into_response(),
    };

    let mut payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": format!("json parse: {}", e)}))).into_response(),
    };

    let is_streaming = payload.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // Normalize roles: translate provider-specific roles to API-compatible.
    // Harmless for Anthropic /v1/messages (their roles are user/assistant, not developer).
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages.iter_mut() {
            if let Some(role) = msg.get_mut("role") {
                if let Some("developer" | "latest_reminder") = role.as_str() {
                    *role = Value::String("system".to_string());
                }
            }
        }
    }

    // Context filter: collapse old tool results to 1-line summaries (preserves message sequence for KV cache)
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let mut turn_count = 0;
        for msg in messages.iter_mut() {
            match msg.get("role").and_then(|r| r.as_str()).unwrap_or("") {
                "user" => { turn_count += 1; }
                "tool" | "toolResult" if turn_count > 8 => {
                    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    let len = content.len();
                    if len > 100 {
                        msg["content"] = Value::String(format!("[tool result: {} chars — collapsed]", len));
                    }
                }
                _ => {}
            }
        }
    }

    // Guard: check edit tool calls for orphaned references (ON by default, disable via RELIARY_PROXY_GUARD_DISABLE=1)
    if !std::env::var("RELIARY_PROXY_GUARD_DISABLE").is_ok_and(|v| v == "1") {
        if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
            if let Some(last) = messages.last() {
                if last.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                    let content = last.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    // Scan for edit tool calls in JSON content
                    if content.contains("\"edit\"") || content.contains("\"apply-edit\"") {
                        if let Some((file_path, new_text)) = extract_edit_from_assistant(content) {
                            if let Some((root, index_path, _)) = crate::daemon::find_reliary_root(&file_path) {
                                let rel_paths = resolve_index_paths(&file_path, &root);
                                for rp in &rel_paths {
                                    let guard_result = crate::guard::check_diff(&index_path, rp, &new_text);
                                    if guard_result.get("status").and_then(|s| s.as_str()) != Some("clean") {
                                        // Inject guard warning as user message
                                        let n_warnings = guard_result.get("warnings").and_then(|w| w.as_array()).map(|a| a.len()).unwrap_or(0);
                                        messages.push(serde_json::json!({
                                            "role": "user",
                                            "content": format!("[guard: {} potential issues in {} - verify cross-file references]", n_warnings, rp)
                                        }));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Anti-decision: record outcomes from tool results and annotate (off by default) ──
    if std::env::var("RELIARY_PROXY_FEATURE_ANTI").is_ok_and(|v| v == "1") {
        let workdir = get_state().workdir.to_string_lossy().to_string();
        if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
            for msg in messages.iter() {
                if let Some((file, identifier, operation, success)) =
                    crate::antidecision::extract_tool_call(msg)
                {
                    crate::antidecision::record(&workdir, &file, &identifier, &operation, success);
                }
            }
            for msg in messages.iter_mut() {
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
                let content = match msg.get("content").and_then(|c| c.as_str()).map(|s| s.to_string()) { Some(s) => s, None => continue };
                if role != "user" && role != "tool" && role != "toolResult" { continue; }
                let known_anti: Vec<(String, String, String)> = {
                    let mut list = Vec::new();
                    if let Ok(db) = crate::antidecision::ANTI_DB.lock() {
                        if let Some(counters) = db.get(&workdir) {
                            for key in counters.keys() {
                                if let Some(rest) = key.strip_prefix(&format!("{}::", workdir)) {
                                    if let Some((file, rest2)) = rest.split_once("::") {
                                        let identifier = rest2.to_string();
                                        if content.contains(file) && content.contains(&identifier) {
                                            let ann = format!(" -{}", identifier);
                                            list.push((file.to_string(), ann, identifier));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    list
                };
                for (file, ann, _identifier) in &known_anti {
                    let new_ref = format!("{} /*{}/**/", file, ann);
                    let annotated = content.replacen(file.as_str(), &new_ref, 1);
                    if annotated != content {
                        msg["content"] = Value::String(annotated);
                        break;
                    }
                }
            }
        }
    }

    // System prompt stripping: on turn 2+, replace system prompt with cached marker.
    // Providers KV-cache the system prompt after turn 1. Stripping saves ~1000+ tokens/turn.
    // Default ON. Disable via RELIARY_PROXY_STRIP_SYSTEM_PROMPT=0
    if !std::env::var("RELIARY_PROXY_STRIP_SYSTEM_PROMPT").is_ok_and(|v| v == "0") {
        if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
            let turn_count = messages.iter().filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user")).count();
            if turn_count >= 2 {
                if let Some(first) = messages.first_mut() {
                    if first.get("role").and_then(|r| r.as_str()) == Some("system") {
                        first["content"] = Value::String("[system prompt cached]".to_string());
                    }
                }
            }
        }
    }

    // First-appearance freeze: compress every message on first occurrence
    let (history_saved, _aggressiveness) = {
        let mut guard = get_or_create_state(&auth_key);
        if let Some(state) = guard.get_mut(&auth_key) {
            if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
                compress_messages(messages, state)
            } else {
                (0, 0)
            }
        } else {
            tracing::error!("state missing after insert for auth_key {}", &auth_key[..8.min(auth_key.len())]);
            (0, 0)
        }
    };

    // Response cache (streaming and non-streaming)
    if let Some(messages) = payload.get("messages") {
        if let Ok(msg_str) = serde_json::to_string(messages) {
            if let Some(cached) = cached_response(&auth_key, &msg_str) {
                let content_type = if is_streaming { "text/event-stream" } else { "application/json" };
                return (StatusCode::OK, [("content-type", content_type)], cached).into_response();
            }
        }
    }

    let body_bytes = serde_json::to_vec(&payload).unwrap_or_default();

    let mut req_builder = HTTP_CLIENT.post(&upstream_url)
        .header("Content-Type", "application/json")
        .body(body_bytes.clone());

    if let Some(auth_val) = headers.get("authorization") {
        req_builder = req_builder.header("authorization", auth_val);
    }

    let hdr_history_saved = history_saved.to_string();

    match req_builder.send().await {
        Ok(upstream_resp) => {
            if is_streaming {
                // SSE streaming: buffer the full upstream response, emit as a single chunk.
                // SSE is line-delimited text — parsers are chunk-agnostic, so sending the
                // entire body at once works identically to true streaming.
                // The mpsc channel approach lost the final [DONE] chunk under TCP timing.
                let ak = auth_key.clone();
                match upstream_resp.bytes().await {
                    Ok(body) => {
                        // Parse usage from buffered body
                        let text = String::from_utf8_lossy(&body);
                        if text.contains("\"usage\"") {
                            let pt = text.split("\"prompt_tokens\":").nth(1)
                                .and_then(|s| s.trim_start().split(|c: char| !c.is_ascii_digit()).next())
                                .and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                            let ct = text.split("\"completion_tokens\":").nth(1)
                                .and_then(|s| s.trim_start().split(|c: char| !c.is_ascii_digit()).next())
                                .and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                            jsonl_log(&serde_json::json!({
                                "event": "stream_usage",
                                "auth_prefix": &ak[..ak.len().min(12)],
                                "prompt_tokens": pt,
                                "completion_tokens": ct,
                            }));
                        }

                        if std::env::var("RELIARY_PROXY_FEATURE_COOCCUR").is_ok_and(|v| v == "1") {
                            let payload_clone = payload.clone();
                            tokio::spawn(async move { preload_next_file(&payload_clone); });
                        }

                        // Compress response body (SSE) before returning to agent
                        let compressed_body = compress_response_body(&String::from_utf8_lossy(&body), true);
                        // Store in response cache for future identical requests
                        if let Ok(msg_str) = serde_json::to_string(&payload.get("messages").unwrap_or(&Value::Null)) {
                            store_response(&auth_key, &msg_str, &compressed_body);
                        }
                        let body_bytes_resp = compressed_body.into_bytes();
                        let body_len = body_bytes_resp.len();
                        let body_stream = futures_util::stream::once(
                            async move { Ok::<_, std::convert::Infallible>(body_bytes_resp) }
                        );
                        let mut resp = axum::response::Response::new(
                            axum::body::Body::from_stream(body_stream)
                        );
                        resp.headers_mut().insert("content-type", header::HeaderValue::from_static("text/event-stream"));
                        resp.headers_mut().insert("cache-control", header::HeaderValue::from_static("no-cache"));
                        if let Ok(hv) = header::HeaderValue::from_str(&hdr_history_saved) {
                            resp.headers_mut().insert("x-reliaty-history-saved", hv);
                        }
                        resp.headers_mut().insert(
                            "x-reliaty-body-bytes",
                            header::HeaderValue::from_str(&body_len.to_string()).unwrap(),
                        );
                        resp
                    }
                    Err(e) => {
                        tracing::error!("upstream body error: {}", e);
                        (StatusCode::BAD_GATEWAY, "upstream body error").into_response()
                    }
                }
            } else {
                match upstream_resp.bytes().await {
                    Ok(bytes) => {
                        let raw_str = String::from_utf8_lossy(&bytes).to_string();
                        // Compress response body before returning to agent
                        let body_str = compress_response_body(&raw_str, false);
                        store_response(&auth_key, &String::from_utf8_lossy(&body_bytes), &body_str);

                        jsonl_log(&serde_json::json!({
                            "event": "proxy_response",
                            "auth_prefix": &auth_key[..auth_key.len().min(12)],
                            "history_saved": history_saved,
                            "response_compressed": body_str.len() < raw_str.len(),
                            "timestamp": std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs()).unwrap_or(0),
                        }));

                        let mut resp = (StatusCode::OK, [("content-type", "application/json")], body_str.clone()).into_response();
                        if std::env::var("RELIARY_PROXY_FEATURE_COOCCUR").is_ok_and(|v| v == "1") {
                            let payload_clone = payload.clone();
                            tokio::spawn(async move { preload_next_file(&payload_clone); });
                        }
                        if let Ok(hv) = header::HeaderValue::from_str(&hdr_history_saved) {
                            resp.headers_mut().insert("x-reliaty-history-saved", hv);
                        }
                        resp
                    }
                    Err(_) => (StatusCode::BAD_GATEWAY, "empty upstream response").into_response(),
                }
            }
        }
        Err(e) => {
            (StatusCode::BAD_GATEWAY, format!("upstream error: {}", e)).into_response()
        }
    }
}

// ── Co-occurrence prediction and pre-load ──
// After each response, extract file references from the message history,
// record them in session state, predict the next file, and pre-warm the cache.
fn preload_next_file(payload: &Value) {
    let state = get_state();
    let messages = match payload.get("messages").and_then(|m| m.as_array()) {
        Some(a) => a,
        None => return,
    };
    // Extract file paths from tool calls and tool results
    for msg in messages.iter().rev().take(2) {
        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
        // Look for .rs .py .js .md file paths
        for word in content.split_whitespace() {
            if word.ends_with(".rs") || word.ends_with(".py") || word.ends_with(".js")
                || word.ends_with(".md") || word.ends_with(".ts") || word.ends_with(".go")
            {
                let clean = word.trim_matches(|c: char| !c.is_ascii() || c.is_ascii_punctuation());
                if clean.contains('/') || clean.contains('\\') {
                    state.record_file_read(clean);
                }
            }
        }
    }
    // Predict next files and pre-load their content into read cache
    let predictions = state.predict_files(3);
    if !predictions.is_empty() {
        let names: Vec<&str> = predictions.iter().map(|(f, _)| f.as_str()).collect();
        tracing::info!("cooccur-predict: pre-loading {} predicted files: {:?}", predictions.len(), names);
    }
    for (file, _score) in &predictions {
        if state.read_cache_get(file).is_some() { continue; } // already cached
        if let Ok(content) = std::fs::read_to_string(file) {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&content, &mut h);
            state.read_cache_set(file.to_string(), crate::session_state::ReadCacheEntry {
                hash: h.finish(),
                len: content.len(),
            });
        }
    }
}

// Extract file path and new content from an edit tool call embedded in assistant JSON.
fn extract_edit_from_assistant(text: &str) -> Option<(String, String)> {
    // Try to find edit/apply-edit tool call patterns in the assistant's response.
    // Pattern 1: "edit" -> "filePath": "..." "newText": "..."
    if let Some(file_start) = text.find("\"filePath\"") {
        let after_file = &text[file_start + 10..];
        let file_path = after_file.split('"').nth(1).map(|s| s.to_string())?;
        if let Some(text_start) = text.find("\"newText\"") {
            let after_text = &text[text_start + 9..];
            let new_text = after_text.split('"').nth(1).map(|s| s.to_string())?;
            return Some((file_path, new_text));
        }
    }
    // Pattern 2: try term-encoded "write" -> "path": "..."
    if let Some(file_start) = text.find("\"path\":") {
        let after_file = &text[file_start + 6..];
        let file_path = after_file.split('"').nth(1).map(|s| s.to_string())?;
        if let Some(text_start) = text.find("\"content\":") {
            let after_text = &text[text_start + 9..];
            let new_text = after_text.split('"').nth(1).map(|s| s.to_string())?;
            return Some((file_path, new_text));
        }
    }
    None
}

// Try multiple relative path forms to match the index's stored paths.
fn resolve_index_paths(file_path: &str, root: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    if file_path.starts_with(root) {
        let rel = file_path[root.len() + 1..].trim_start_matches('/').to_string();
        candidates.push(rel.clone());
        if let Some(stripped) = rel.strip_prefix("crates/") {
            candidates.push(stripped.to_string());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = cwd.to_string_lossy().to_string();
        if file_path.starts_with(&cwd_str) {
            let rel = file_path[cwd_str.len() + 1..].trim_start_matches('/').to_string();
            candidates.push(rel.clone());
            if let Some(stripped) = rel.strip_prefix("crates/") {
                candidates.push(stripped.to_string());
            }
        }
    }
    candidates.push(file_path.to_string());
    candidates
}

// GET /check-diff — check a proposed edit for structural issues.
async fn check_diff_handler(Query(params): Query<HashMap<String, String>>) -> String {
    let file_path = params.get("file").map(|s| s.as_str()).unwrap_or("");
    let new_content = params.get("content").map(|s| s.as_str()).unwrap_or("");
    if file_path.is_empty() || new_content.is_empty() {
        return "{\"error\": \"missing file or content param\"}".to_string();
    }
    if let Some((root, index_path, _)) = crate::daemon::find_reliary_root(file_path) {
        // Try multiple relative path forms to match index
        let rel_paths = resolve_index_paths(file_path, &root);
        // Try each, return first that produces warnings
        for rp in &rel_paths {
            let result = crate::guard::check_diff(&index_path, rp, new_content);
            if result.get("status").and_then(|s| s.as_str()) != Some("clean") {
                return serde_json::to_string(&result).unwrap_or_else(|_| "{\"error\": \"serialization failed\"}".to_string());
            }
        }
        // All returned clean — return the first
        let result = crate::guard::check_diff(&index_path, &rel_paths[0], new_content);
        serde_json::to_string(&result).unwrap_or_else(|_| "{\"error\": \"serialization failed\"}".to_string())
    } else {
        "{\"error\": \"no .reliary index\"}".to_string()
    }
}

// GET /read-validated — warn about externally-referenced identifiers before editing.
async fn read_validated_handler(Query(params): Query<HashMap<String, String>>) -> String {
    let file_path = params.get("file").map(|s| s.as_str()).unwrap_or("");
    if file_path.is_empty() {
        return "{\"error\": \"missing file param\"}".to_string();
    }
    if let Some((root, index_path, _)) = crate::daemon::find_reliary_root(file_path) {
        use std::io::Read;
        let rel_paths = resolve_index_paths(file_path, &root);
        let rel_path = rel_paths.first().map(|s| s.as_str()).unwrap_or(file_path);
        let full_path = std::path::Path::new(&root).join(rel_path);
        let mut content = String::new();
        if let Ok(mut f) = std::fs::File::open(&full_path) {
            if let Ok(meta) = f.metadata() {
                if meta.len() > 10_000_000 {
                    return serde_json::json!({"error": "file too large"}).to_string();
                }
            }
            if f.read_to_string(&mut content).is_err() {  // GUARDED: intentional
                return serde_json::json!({"error": "cannot read file"}).to_string();
            }
        }
        // Try each path form
        for rp in &rel_paths {
            let result = crate::guard::read_validated(&index_path, rp, &content);
            if result.get("status").and_then(|s| s.as_str()) != Some("clean") {
                return serde_json::to_string(&result).unwrap_or_else(|_| "{\"error\": \"serialization failed\"}".to_string());
            }
        }
        let result = crate::guard::read_validated(&index_path, &rel_paths[0], &content);
        serde_json::to_string(&result).unwrap_or_else(|_| "{\"error\": \"serialization failed\"}".to_string())
    } else {
        "{\"error\": \"no .reliary index\"}".to_string()
    }
}

// ── Startup ──

pub async fn start(port: u16, daemon_state: Option<Arc<crate::session_state::SessionState>>) -> Result<(), String> {
    if let Some(s) = daemon_state {
        if let Ok(mut guard) = DAEMON_STATE.lock() {
            *guard = Some(s);
        }
    }

    // Scavenger thread
    let state = get_state();
    std::thread::Builder::new()
        .name("scavenger".into())
        .spawn(move || {
            loop {
                let sc = Arc::clone(&state);
                if let Err(e) = std::panic::catch_unwind(|| {
                    crate::scavenger::scavenger_loop(sc);
                }) {
                    error!("scavenger crashed: {:?}", e);
                }
                std::thread::sleep(std::time::Duration::from_secs(120));
            }
        })
        .ok();  // GUARDED: intentional

    #[cfg(unix)] {
        if let Ok(limit) = rlimit::getrlimit(rlimit::Resource::NOFILE) {
            if limit.0 < 1024 {
                warn!("file descriptor limit is {} (recommended >= 1024)", limit.0);
            }
        }
    }

    let addr = format!("127.0.0.1:{}", port);

    let app = Router::new()
        .route("/health", get(health))
        .route("/ping", get(ping))
        .route("/search", get(search_handler))
        .route("/risk", get(risk_handler))
        .route("/compress", get(compress_handler))
        .route("/veto", get(veto_handler))
        .route("/muzzle", get(muzzle_handler))
        .route("/prior", get(prior_handler))
        .route("/read-summary", get(read_summary_handler))
        .route("/who-calls", get(who_calls_handler))
        .route("/status", get(status_handler))
        .route("/check-diff", get(check_diff_handler))
        .route("/read-validated", get(read_validated_handler))
        .route("/v1/chat/completions", post(proxy_post))
        .route("/v1/messages", post(proxy_post))  // Anthropic/Claude Code compatibility
        .route("/mcp/sse", get(crate::mcp_sse::sse_handler))
        .route("/mcp/messages", post(crate::mcp_sse::messages_handler))
        .layer(tower_http::cors::CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("bind: {}", e))?;

    info!(target: "reliary", "v{} ready — daemon + proxy on :{}", env!("CARGO_PKG_VERSION"), port);
    info!(target: "reliary", "Routes: /health /ping /search /risk /compress /veto /muzzle /prior");
    info!(target: "reliary", "Proxy features: compression=on guard={} anti={} (set RELIARY_PROXY_GUARD_DISABLE=1 to disable guard)",
        if std::env::var("RELIARY_PROXY_GUARD_DISABLE").is_ok_and(|v| v == "1") { "off" } else { "on" },
        if std::env::var("RELIARY_PROXY_FEATURE_ANTI").is_ok_and(|v| v == "1") { "on" } else { "off" });

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("serve: {}", e))
}
