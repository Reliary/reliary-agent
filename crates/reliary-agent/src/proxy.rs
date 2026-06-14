/// Provider-agnostic proxy with axum — true SSE streaming support.
/// Auth-based routing via routes.rs. No model lists, no provider detection.

use axum::{
    Router, extract::Query, http::{HeaderMap, StatusCode, header},
    response::{sse::Sse, IntoResponse, Json, sse::Event},
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

/// Adaptive compression policy — adjusts aggressiveness based on last output length.
#[derive(Clone)]
struct AdaptivePolicy {
    last_output_len: usize,
    aggressiveness: f32,
    concise_turns: u32,
}

impl AdaptivePolicy {
    fn new() -> Self {
        Self { last_output_len: 0, aggressiveness: 0.7, concise_turns: 0 }
    }

    fn compute_aggressiveness(last_output_len: usize) -> f32 {
        match last_output_len {
            0..=500   => 0.3,
            501..=1500 => 0.5,
            1501..=3000 => 0.7,
            _          => 0.9,
        }
    }

    fn update(&mut self, output_len: usize) {
        self.last_output_len = output_len;
        let new = Self::compute_aggressiveness(output_len);
        // Decay aggressiveness when LLM is concise
        if output_len < 500 {
            self.concise_turns += 1;
            if self.concise_turns >= 2 {
                self.aggressiveness = self.aggressiveness.max(0.1) - 0.1;
            }
        } else {
            self.concise_turns = 0;
            self.aggressiveness = new;
        }
        self.aggressiveness = self.aggressiveness.clamp(0.1, 0.9);
    }
}

/// Per-auth-key state (policy + dedup cache).
struct PerKeyState {
    policy: AdaptivePolicy,
    dedup_cache: HashMap<u64, (String, Instant)>,  // hash -> (file_path, last_seen)
}

impl PerKeyState {
    fn new() -> Self {
        Self { policy: AdaptivePolicy::new(), dedup_cache: HashMap::new() }
    }

    fn check_dedup(&mut self, content: &str, path: &str) -> Option<String> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut h);
        let hash = h.finish();

        // Evict entries older than 5 minutes
        let now = Instant::now();
        self.dedup_cache.retain(|_, (_, t)| now.duration_since(*t).as_secs() < 300);

        if let Some((existing_path, _)) = self.dedup_cache.get(&hash) {
            return Some(format!("[already seen: {} — {} chars unchanged]", existing_path, content.len()));
        }

        if self.dedup_cache.len() < 50 {
            self.dedup_cache.insert(hash, (path.to_string(), now));
        }
        None
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
    // Split on code blocks
    let mut parts: Vec<String> = Vec::new();
    let mut in_code = false;
    let mut code_buf = String::new();
    let mut prose_buf = String::new();

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_code {
                // End code block
                parts.push(code_buf.clone());
                code_buf.clear();
                in_code = false;
            } else {
                // Flush prose buffer, compressed
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
    // Flush remaining buffers
    if in_code && !code_buf.is_empty() {
        parts.push(code_buf);
    } else if !prose_buf.is_empty() {
        parts.push(prose_buf);
    }

    // If no code blocks, use compress_reasoning directly
    if parts.len() <= 1 && !text.contains("```") {
        return reliary_compress::compress_reasoning(text, dict);
    }

    // Compress prose sections
    let mut result = String::new();
    let mut total_original = 0usize;
    let mut total_compressed = 0usize;

    for part in &parts {
        total_original += part.len();
        // Check if this part looks like prose (no code patterns)
        if part.contains("```") || part.len() < 100 {
            // Code block or too short — keep verbatim
            result.push_str(part);
            total_compressed += part.len();
        } else {
            // Prose section — attempt to compress
            match reliary_compress::compress_reasoning(part, dict) {
                Some(c) => {
                    result.push_str(&c);
                    result.push('\n');
                    total_compressed += c.len();
                }
                None => {
                    result.push_str(part);
                    total_compressed += part.len();
                }
            }
        }
    }

    // Require at least 15% savings
    if total_original > 0 && total_compressed < (total_original as f64 * 0.85) as usize {
        Some(result)
    } else if parts.len() <= 1 && total_compressed < total_original {
        Some(result)
    } else {
        None
    }
}

/// Truncate old tool results — keep first 200 + last 50 chars.
fn truncate_tool_result(content: &str) -> String {
    if content.len() <= 250 { return content.to_string(); }
    let prefix = &content[..200];
    let suffix = &content[content.len().saturating_sub(50)..];
    format!("{} …[truncated {} chars]… {}", prefix, content.len() - 250, suffix)
}

/// Compress all messages in the conversation history.
fn compress_messages(messages: &mut Vec<Value>, state: &mut PerKeyState) -> (usize, usize) {
    let total = messages.len();
    let mut history_saved: usize = 0;
    for i in (0..total).rev() {
        let age = total - i; // 1 = most recent
        let role = messages[i].get("role").and_then(|r| r.as_str()).unwrap_or("");

        match role {
            "assistant" if age > 2 && state.policy.aggressiveness >= 0.3 => {
                // Compress old assistant reasoning
                if let Some(content) = messages[i].get("content").and_then(|c| c.as_str()) {
                    if let Some(compressed) = compress_assistant_text(content, None) {
                        let saved = content.len().saturating_sub(compressed.len());
                        if saved > 10 {
                            history_saved += saved;
                            messages[i]["content"] = Value::String(compressed);
                        }
                    }
                }
            }
            "tool" | "toolResult" if age > 4 => {
                // Truncate old tool results
                if let Some(content) = messages[i].get("content").and_then(|c| c.as_str()) {
                    let truncated = truncate_tool_result(content);
                    if truncated.len() < content.len() {
                        let saved = content.len().saturating_sub(truncated.len());
                        history_saved += saved;
                        messages[i]["content"] = Value::String(truncated);
                    }
                }
            }
            "tool" | "toolResult" if age > 2 => {
                // Dedup repeated file reads
                if let Some(content) = messages[i].get("content").and_then(|c| c.as_str()) {
                    // Extract file path from content (heuristic: first line that looks like a path)
                    let path = content.lines().find(|l| l.contains(".rs") || l.contains(".py") || l.contains(".js") || l.contains(".ts"))
                        .unwrap_or("file");
                    if let Some(deduped) = state.check_dedup(content, path) {
                        let saved = content.len().saturating_sub(deduped.len());
                        history_saved += saved;
                        messages[i]["content"] = Value::String(deduped);
                    }
                }
            }
            _ => {}
        }
    }

    (history_saved, (state.policy.aggressiveness * 100.0) as usize)
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

    // History compression: compress old assistant reasoning + truncate old tool results
    let (history_saved, aggressiveness) = {
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

    // Feed-forward compression
    let input_body_str = serde_json::to_string(&payload).unwrap_or_default();
    let input_tokens = estimate_tokens(&input_body_str);

    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let dict = crate::read_summary::load_dictionary();
        for (i, msg) in messages.iter_mut().enumerate() {
            if i < 2 { continue; }
            if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") { continue; }
            if let Some(content) = msg.get_mut("content") {
                if let Some(text) = content.as_str() {
                    if let Some(compressed) = reliary_compress::compress_reasoning(text, dict.as_ref()) {
                        *content = Value::String(compressed);
                    }
                }
            }
        }
    }

    let compressed_body_str = serde_json::to_string(&payload).unwrap_or_default();
    let compressed_tokens = estimate_tokens(&compressed_body_str);
    let token_savings = if input_tokens > 0 {
        ((input_tokens.saturating_sub(compressed_tokens)) as f64 / input_tokens as f64 * 100.0) as usize
    } else { 0 };

    let body_bytes = serde_json::to_vec(&payload).unwrap_or_default();

    let client = reqwest::Client::new();
    let mut req_builder = client.post(&upstream_url)
        .header("Content-Type", "application/json")
        .body(body_bytes.clone());

    if let Some(auth_val) = headers.get("authorization") {
        req_builder = req_builder.header("authorization", auth_val);
    }

    let token_hdr_input = input_tokens.to_string();
    let token_hdr_compressed = compressed_tokens.to_string();
    let token_hdr_savings = token_savings.to_string();
    let hdr_history_saved = history_saved.to_string();
    let hdr_aggr = aggressiveness.to_string();

    match req_builder.send().await {
        Ok(upstream_resp) => {
            if is_streaming {
                let byte_stream = upstream_resp.bytes_stream();
                let event_stream = byte_stream.map(|chunk| {
                    let data = match chunk {
                        Ok(b) => String::from_utf8_lossy(&b).to_string(),
                        Err(_) => "[error]".to_string(),
                    };
                    Ok::<Event, std::convert::Infallible>(Event::default().data(data))
                });
                let mut resp = Sse::new(event_stream).into_response();
                resp.headers_mut().insert("content-type", header::HeaderValue::from_static("text/event-stream"));
                resp.headers_mut().insert("cache-control", header::HeaderValue::from_static("no-cache"));
                resp.headers_mut().insert("x-reliaty-input-tokens", header::HeaderValue::from_str(&token_hdr_input).unwrap());
                resp.headers_mut().insert("x-reliaty-compressed-tokens", header::HeaderValue::from_str(&token_hdr_compressed).unwrap());
                resp.headers_mut().insert("x-reliaty-savings-pct", header::HeaderValue::from_str(&token_hdr_savings).unwrap());
                resp.headers_mut().insert("x-reliaty-history-saved", header::HeaderValue::from_str(&hdr_history_saved).unwrap());
                resp.headers_mut().insert("x-reliaty-aggressiveness", header::HeaderValue::from_str(&hdr_aggr).unwrap());
                // Update adaptive policy
                if let Ok(mut guard) = PER_KEY_STATE.lock() {
                    if let Some(st) = guard.get_mut(&auth_key) {
                        st.policy.update(body_bytes.len());
                    }
                }
                resp.into_response()
            } else {
                match upstream_resp.bytes().await {
                    Ok(bytes) => {
                        let body_str = String::from_utf8_lossy(&bytes).to_string();
                        store_response(&auth_key, &String::from_utf8_lossy(&body_bytes), &body_str);
                        // Update adaptive policy with output length
                        if let Ok(mut guard) = PER_KEY_STATE.lock() {
                            if let Some(st) = guard.get_mut(&auth_key) {
                                st.policy.update(body_str.len());
                            }
                        }
                        let mut resp = (StatusCode::OK, [("content-type", "application/json")], body_str).into_response();
                        resp.headers_mut().insert("x-reliaty-input-tokens", header::HeaderValue::from_str(&token_hdr_input).unwrap());
                        resp.headers_mut().insert("x-reliaty-compressed-tokens", header::HeaderValue::from_str(&token_hdr_compressed).unwrap());
                        resp.headers_mut().insert("x-reliaty-savings-pct", header::HeaderValue::from_str(&token_hdr_savings).unwrap());
                        resp.headers_mut().insert("x-reliaty-history-saved", header::HeaderValue::from_str(&hdr_history_saved).unwrap());
                        resp.headers_mut().insert("x-reliaty-aggressiveness", header::HeaderValue::from_str(&hdr_aggr).unwrap());
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
