/// Provider-agnostic proxy with axum — true SSE streaming support.
/// Auth-based routing via routes.rs. No model lists, no provider detection.

use axum::{
    Router, extract::Query, http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use bytes::Bytes;
use futures_util::stream::StreamExt;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, LazyLock};
use serde_json::Value;
use std::time::Instant;

// ── Token counting (lightweight heuristic) ──

fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() { return 0; }
    let whitespace = text.split_whitespace().count();
    let avg_len = text.len().saturating_sub(whitespace.saturating_sub(1)) / whitespace.max(1);
    // Common heuristic: ~1.3 tokens per word for code, ~1.5 for prose
    let tokens_per_word = if avg_len > 5 { 1.3 } else { 1.5 };
    (whitespace as f64 * tokens_per_word).round() as usize
}

 // Alias to avoid name conflict

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

// ── History Compression Components ──

/// Per-auth-key state — first-appearance freeze cache.
/// `content_cache`: maps content hash → compressed version.
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

/// Global per-auth-key state store
static PER_KEY_STATE: LazyLock<Mutex<HashMap<String, PerKeyState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn get_or_create_state(auth_key: &str) -> std::sync::MutexGuard<'static, HashMap<String, PerKeyState>> {
    let mut guard = PER_KEY_STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard.entry(auth_key.to_string()).or_insert_with(PerKeyState::new);
    guard
}

/// Compress old assistant reasoning — strip verbose explanations, keep code blocks intact.
/// Splits message into code blocks (```...```) and prose sections.
/// Compresses prose, leaves code verbatim.
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

/// Sift-based tool result compression — uses reliary-output for structural collapse.
fn sift_compress_tool_result(content: &str) -> String {
    if content.len() <= 200 { return content.to_string(); }
    let compressed = reliary_output::compress_output(content);
    if compressed.len() < content.len() {
        compressed
    } else {
        content.to_string()
    }
}

/// First-appearance freeze compression: compress every message on first occurrence,
/// cache the compressed version, and use the cached version forever after.
/// This preserves KV cache stability — the compressed version is what the API/SDK
/// has cached from the start.
fn compress_messages(messages: &mut Vec<Value>, state: &mut PerKeyState) -> (usize, usize) {
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
            "assistant" => compress_assistant_text(&content, None),
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
            } else {
                state.content_cache.insert(hash, content);
            }
        } else {
            state.content_cache.insert(hash, content);
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

    // Normalize roles: translate provider-specific roles to API-compatible
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages.iter_mut() {
            if let Some(role) = msg.get_mut("role") {
                if let Some(r) = role.as_str() {
                    match r {
                        "developer" | "latest_reminder" => {
                            *role = Value::String("system".to_string());
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Context filter: drop old tool results
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let mut turn_count = 0;
        let mut to_keep: Vec<bool> = vec![true; messages.len()];
        for (i, msg) in messages.iter().enumerate() {
            match msg.get("role").and_then(|r| r.as_str()).unwrap_or("") {
                "user" => { turn_count += 1; }
                "tool" | "toolResult" if turn_count > 8 => { to_keep[i] = false; }
                _ => {}
            }
        }
        for i in (0..messages.len()).rev() {
            if !to_keep[i] { messages.remove(i); }
        }
    }

    // First-appearance freeze: compress every message on first occurrence
    let (history_saved, _aggressiveness) = {
        let mut guard = get_or_create_state(&auth_key);
        let state = guard.get_mut(&auth_key).unwrap();
        if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
            compress_messages(messages, state)
        } else {
            (0, 0)
        }
    };

    // Response cache (non-streaming only)
    if !is_streaming {
        if let Some(messages) = payload.get("messages") {
            if let Ok(msg_str) = serde_json::to_string(messages) {
                if let Some(cached) = cached_response(&auth_key, &msg_str) {
                    return (StatusCode::OK, [("content-type", "application/json")], cached).into_response();
                }
            }
        }
    }

    let body_bytes = serde_json::to_vec(&payload).unwrap_or_default();

    let client = reqwest::Client::new();
    let mut req_builder = client.post(&upstream_url)
        .header("Content-Type", "application/json")
        .body(body_bytes.clone());

    if let Some(auth_val) = headers.get("authorization") {
        req_builder = req_builder.header("authorization", auth_val);
    }

    let hdr_history_saved = history_saved.to_string();
    let hdr_aggr = String::new();

    match req_builder.send().await {
        Ok(upstream_resp) => {
            if is_streaming {
                let byte_stream = upstream_resp.bytes_stream();
                let body_stream = byte_stream.map({
                    let auth_key = auth_key.clone();
                    let _aggressiveness = 0;
                    move |chunk| {
                        if let Ok(bytes) = &chunk {
                            let text = String::from_utf8_lossy(bytes);
                            // Log token usage from final SSE chunk
                            if text.contains("\"usage\"") {
                                // Extract prompt_tokens and completion_tokens
                                let pt = text.split("\"prompt_tokens\":").nth(1)
                                    .and_then(|s| s.trim_start().split(|c: char| !c.is_ascii_digit()).next())
                                    .and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                                let ct = text.split("\"completion_tokens\":").nth(1)
                                    .and_then(|s| s.trim_start().split(|c: char| !c.is_ascii_digit()).next())
                                    .and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                                let log_entry = serde_json::json!({
                                    "event": "stream_usage",
                                    "auth_prefix": &auth_key[..auth_key.len().min(12)],
                                    "prompt_tokens": pt,
                                    "completion_tokens": ct,
                                });
                                if let Ok(mut lf) = std::fs::OpenOptions::new()
                                    .create(true).append(true).open("/tmp/reliary_proxy.jsonl")
                                {
                                    use std::io::Write;
                                    let _ = writeln!(lf, "{}", log_entry);
                                }
                            }
                        }
                        Ok::<Bytes, std::convert::Infallible>(chunk.unwrap_or_else(|_| Bytes::from("[error]\n")))
                    }
                });
                let mut resp = axum::response::Response::new(axum::body::Body::from_stream(body_stream));
                resp.headers_mut().insert("content-type", header::HeaderValue::from_static("text/event-stream"));
                resp.headers_mut().insert("cache-control", header::HeaderValue::from_static("no-cache"));
                if let Ok(hv) = header::HeaderValue::from_str(&hdr_history_saved) {
                    resp.headers_mut().insert("x-reliaty-history-saved", hv);
                }
                resp
            } else {
                match upstream_resp.bytes().await {
                    Ok(bytes) => {
                        let body_str = String::from_utf8_lossy(&bytes).to_string();
                        store_response(&auth_key, &String::from_utf8_lossy(&body_bytes), &body_str);

                        // Log per-request token data for benchmarking
                        if let Ok(mut log_fh) = std::fs::OpenOptions::new()
                            .create(true).append(true).open("/tmp/reliary_proxy.jsonl")
                        {
                            use std::io::Write;
                            let log_entry = serde_json::json!({
                                "event": "proxy_response",
                                "auth_prefix": &auth_key[..auth_key.len().min(12)],
                                "history_saved": history_saved,
                                "timestamp": std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs()).unwrap_or(0),
                            });
                            let _ = writeln!(log_fh, "{}", log_entry);
                        }

                        let mut resp = (StatusCode::OK, [("content-type", "application/json")], body_str.clone()).into_response();
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

/// Try multiple relative path forms to match the index's stored paths.
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

/// GET /check-diff — check a proposed edit for structural issues.
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

/// GET /read-validated — warn about externally-referenced identifiers before editing.
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
            let _ = f.read_to_string(&mut content);
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
                    eprintln!("[reliary] scavenger crashed: {:?}", e);
                }
                std::thread::sleep(std::time::Duration::from_secs(120));
            }
        })
        .ok();

    #[cfg(unix)] {
        if let Ok(limit) = rlimit::getrlimit(rlimit::Resource::NOFILE) {
            if limit.0 < 1024 {
                eprintln!("[reliary] WARNING: file descriptor limit is {} (recommended >= 1024)", limit.0);
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
        .route("/status", get(status_handler))
        .route("/check-diff", get(check_diff_handler))
        .route("/read-validated", get(read_validated_handler))
        .route("/v1/chat/completions", post(proxy_post))
        .route("/v1/messages", post(proxy_post));  // Anthropic/Claude Code compatibility

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("bind: {}", e))?;

    eprintln!("\x1b[1m\x1b[34m  reliary-agent v{} ready\x1b[0m", env!("CARGO_PKG_VERSION"));
    eprintln!("  \x1b[2mDaemon + proxy on \x1b[1m:{}", port);
    eprintln!("  \x1b[2mRoutes: /health /ping /search /risk /compress /veto /muzzle /prior\x1b[0m");

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("serve: {}", e))
}
