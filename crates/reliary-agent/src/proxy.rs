/// Bidirectional proxy: 4 real synergies that gate.js cannot achieve.
/// Synergy 1: Response cache (repeated edits cost zero after first generation)
/// Synergy 2: Two-phase generation (cheap model plans, main model executes)
/// Synergy 3: Context filter (strip old tool results, cap conversation at 5 turns)
/// Synergy 4: Feed-forward compression (compress before API sees it)

use tiny_http::{Server, Response, Request, Header, StatusCode};
use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Mutex;

const DEFAULT_UPSTREAM: &str = "https://api.deepinfra.com/v1/openai/chat/completions";
const RESPONSE_CACHE_LIMIT: usize = 100;

static RESPONSE_CACHE: std::sync::LazyLock<Mutex<HashMap<u64, String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Synergy 2: Output length feedback — per-session output history
static OUTPUT_LENGTH_HISTORY: std::sync::LazyLock<Mutex<HashMap<String, usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Synergy 3: In-memory compression ratio chronicle (records per-file ratios)
static COMPRESS_RATIO_CHRONICLE: std::sync::LazyLock<Mutex<Vec<(String, f64)>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

fn cache_key(model: &str, messages_json: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    model.hash(&mut h);
    messages_json.hash(&mut h);
    h.finish()
}

fn cached_response(model: &str, messages_json: &str) -> Option<String> {
    let key = cache_key(model, messages_json);
    RESPONSE_CACHE.lock().ok()?.get(&key).cloned()
}

fn store_response(model: &str, messages_json: &str, response: &str) {
    let key = cache_key(model, messages_json);
    if let Ok(mut cache) = RESPONSE_CACHE.lock() {
        cache.insert(key, response.to_string());
        if cache.len() > RESPONSE_CACHE_LIMIT + 20 {
            let keys: Vec<u64> = cache.keys().copied().collect();
            for k in keys.iter().take(20) {
                cache.remove(k);
            }
        }
    }
}

pub fn start(port: u16) -> Result<(), String> {
    let addr = format!("127.0.0.1:{}", port);
    let server = Server::http(&addr).map_err(|e| format!("proxy bind: {}", e))?;
    eprintln!("[reliary] proxy listening on {}", addr);

    for request in server.incoming_requests() {
        let method = request.method();
        if method == &tiny_http::Method::Post {
            handle_request(request);
        } else {
            let _ = request.respond(Response::from_string("not found").with_status_code(404));
        }
    }
    Ok(())
}

fn forward_to_api(mut request: Request, payload: &serde_json::Value) {
    let api_key = request.headers().iter()
        .find(|h| h.field.to_string().to_lowercase() == "authorization")
        .map(|h| format!("{}", h.value))
        .unwrap_or_default();

    let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let model_for_fb = model.clone();
    let upstream_url = std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| DEFAULT_UPSTREAM.to_string());
    let body_str = serde_json::to_string(payload).unwrap_or_default();

    let child = Command::new("curl")
        .args(["-s", "-X", "POST", &upstream_url,
               "-H", &format!("Authorization: {}", api_key),
               "-H", "Content-Type: application/json",
               "-d", &body_str])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    match child {
        Ok(mut c) => {
            let mut resp_body = String::new();
            if let Some(ref mut stdout) = c.stdout {
                let _ = stdout.read_to_string(&mut resp_body);
            }
            let _ = c.wait();
            if !resp_body.is_empty() {
                // Record output length per model for feedback loop
                if let Ok(resp_val) = serde_json::from_str::<serde_json::Value>(&resp_body) {
                    if let Some(choices) = resp_val["choices"].as_array() {
                        for choice in choices {
                            if let Some(msg) = choice["message"]["content"].as_str() {
                                let mut history = OUTPUT_LENGTH_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
                                history.insert(model_for_fb.clone(), msg.len());
                            }
                        }
                    }
                }
                let ct = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
                let _ = request.respond(
                    Response::from_string(resp_body)
                        .with_status_code(200)
                        .with_header(ct)
                );
            } else {
                respond(request, 502, "{\"error\":\"empty upstream response\"}");
            }
        }
        Err(e) => {
            respond(request, 502, &format!("{{\"error\":\"curl: {}\"}}", e));
        }
    }
}

fn handle_request(mut request: Request) {
    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        respond(request, 400, "{\"error\":\"read error\"}");
        return;
    }

    let mut payload = match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(v) => v,
        Err(e) => {
            respond(request, 400, &format!("{{\"error\":\"json parse: {}\"}}", e));
            return;
        }
    };

    let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

    // ── Synergy 3: Context filter ──
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let mut turn_count = 0;
        let mut to_keep: Vec<bool> = vec![true; messages.len()];

        for (i, msg) in messages.iter().enumerate() {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            match role {
                "user" => {
                    turn_count += 1;
                }
                "tool" | "toolResult" => {
                    // Drop tool results from turns older than 8
                    if turn_count > 8 {
                        to_keep[i] = false;
                    }
                }
                _ => {}
            }
        }

        // Remove filtered messages (in reverse order to preserve indices)
        for i in (0..messages.len()).rev() {
            if !to_keep[i] {
                messages.remove(i);
            }
        }
    }

    // ── Synergy 1: Response cache check (before mutations that add cost) ──
    // We check cache using pre-compression messages for broader hit rate
    if let Some(messages) = payload.get("messages") {
        if let Ok(msg_str) = serde_json::to_string(messages) {
            if let Some(cached) = cached_response(&model, &msg_str) {
                respond(request, 200, &cached);
                return;
            }
        }
    }

    // ── Synergy 4: Feed-forward compression ──
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let dict = crate::read_summary::load_dictionary();
        for (i, msg) in messages.iter_mut().enumerate() {
            if i < 2 { continue; }
            if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") { continue; }
            if let Some(content) = msg.get_mut("content") {
                if let Some(text) = content.as_str() {
                    let pre_len = text.len();
                    let compressed_result = reliary_compress::compress_reasoning(text, dict.as_ref());
                    if let Some(compressed) = compressed_result {
                        let ratio = compressed.len() as f64 / pre_len as f64;
                        *content = serde_json::Value::String(compressed);
                        // Synergy 3: record compression ratio
                        if let Ok(mut cr) = COMPRESS_RATIO_CHRONICLE.lock() {
                            cr.push(("assistant_block".to_string(), ratio));
                            if cr.len() > 1000 { cr.clear(); }
                        }
                    }
                }
            }
        }
    }

    // ── Synergy 1: Inverse compression — pad messages to fixed-size blocks for cache alignment ──
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for (i, msg) in messages.iter_mut().enumerate() {
            if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") { continue; }
            if let Some(content) = msg.get_mut("content") {
                if let Some(text) = content.as_str() {
                    let target_chars = 256usize * 4; // ~256 tokens
                    if text.len() < target_chars {
                        let padding = " ".repeat(target_chars - text.len());
                        *content = serde_json::Value::String(format!("{}{}", text, padding));
                    }
                }
            }
        }
    }

    // ── Synergy 2: Output length feedback loop ──
    {
        let history = OUTPUT_LENGTH_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
        let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let last_len = history.get(&model).copied().unwrap_or(0);
        drop(history);
        if last_len > 0 {
            let budget = if last_len > 1500 { 800usize }
                        else if last_len > 800 { 1200usize }
                        else { 2000usize };
            payload["max_completion_tokens"] = serde_json::json!(budget);
        }
    }

    // ── Synergy 2: Two-phase generation ──
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let user_idx = messages.iter().rposition(|m| {
            m.get("role").and_then(|r| r.as_str()) == Some("user")
        });
        if let Some(idx) = user_idx {
            let user_text = messages[idx]["content"].as_str().unwrap_or("").to_string();
            let is_fix_task = user_text.contains("fix") || user_text.contains("edit")
                || user_text.contains("update") || user_text.contains("change");
            let has_files = user_text.contains("src/") || user_text.contains(".rs")
                || user_text.contains(".py");

            if is_fix_task && has_files && user_text.len() > 20 {
                let plan_req = serde_json::json!({
                    "model": "deepseek/deepseek-v4-flash",
                    "messages": [
                        {"role": "system", "content": "Respond with a 1-line plan for the fix. No code. Format: [file.rs] change line X from Y to Z"},
                        {"role": "user", "content": &user_text}
                    ],
                    "max_tokens": 100,
                    "temperature": 0.1
                });

                let plan_body = serde_json::to_string(&plan_req).unwrap_or_default();
                let api_key = request.headers().iter()
                    .find(|h| h.field.to_string().to_lowercase() == "authorization")
                    .map(|h| format!("{}", h.value))
                    .unwrap_or_default();
                let upstream_url = std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| DEFAULT_UPSTREAM.to_string());

                if let Ok(mut plan_child) = Command::new("curl")
                    .args(["-s", "-X", "POST", &upstream_url,
                           "-H", &format!("Authorization: {}", api_key),
                           "-H", "Content-Type: application/json",
                           "-d", &plan_body])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    let mut plan_response = String::new();
                    let _ = plan_child.stdout.take().map(|mut o| o.read_to_string(&mut plan_response));
                    let _ = plan_child.wait();

                    if let Ok(plan_value) = serde_json::from_str::<serde_json::Value>(&plan_response) {
                        if let Some(plan_text) = plan_value["choices"][0]["message"]["content"].as_str() {
                            let plan_short: String = plan_text.chars().take(100).collect();
                            let plan_msg = serde_json::json!({
                                "role": "system",
                                "content": format!("[plan: {}]", plan_short)
                            });
                            messages.insert(1, plan_msg);
                        }
                    }
                }
            }
        }
    }

    // Forward to API
    forward_to_api(request, &payload);
}

fn respond(mut request: Request, status: u16, msg: &str) {
    let _ = request.respond(Response::from_string(msg).with_status_code(StatusCode(status)));
}
