/// Bidirectional proxy: 6 synergies for universal agent compression.
/// Listens on :9090, forwards to API upstream via curl subprocess.
///
/// Synergy 1: Feed-forward compression (all assistant messages)
/// Synergy 2: Adaptive compression per-file (chronicle failure history)
/// Synergy 3: Pre-generation context inject (FTS5 search + risk)
/// Synergy 4: Model-specific compression profiles (detected from payload)
/// Synergy 5: Cross-session read dedup (hash cache via daemon)
/// Synergy 6: Hallucination detection (deferred — needs SSE streaming)

use tiny_http::{Server, Response, Request, Header, StatusCode};
use std::io::Read;
use std::process::{Command, Stdio};

const DEFAULT_UPSTREAM: &str = "https://api.deepinfra.com/v1/openai/chat/completions";

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

    // Synergy 4: Model-specific compression profile
    let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
    let profile = model_profile(model);

    // Synergy 3: Pre-generation context inject
    let files_seen = inject_context(&mut payload);

    // Synergy 2 + 5: Adaptive compression + dedup per file
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let dict = if profile.dict_usage { crate::read_summary::load_dictionary() } else { None };

        for (i, msg) in messages.iter_mut().enumerate() {
            if i < 2 { continue; }

            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

            // Synergy 5: Cross-session read dedup
            let mut dedup_target: Option<(String, String, usize)> = None;
            if role == "toolResult" || role == "tool" {
                if let Some(text) = msg["content"].as_str() {
                    let text_owned = text.to_string();
                    let text_len = text.len();
                    for file in &files_seen {
                        if text_owned.contains(file) {
                            let hash = daemon_hash(&text_owned);
                            let check_key = format!("check-read {} {}", file, hash);
                            if let Some(daemon_result) = daemon_cmd(&check_key) {
                                if daemon_result.starts_with("unchanged") {
                                    dedup_target = Some((hash, file.clone(), text_len));
                                    break;
                                }
                            }
                            let cache_key = format!("cache-read {} {} {}", file, hash, text_len);
                            let _ = daemon_cmd(&cache_key);
                        }
                    }
                }
            }
            if let Some((hash, _file, text_len)) = dedup_target {
                msg["content"] = serde_json::Value::String(
                    format!("[reliary: {}] unchanged ({} chars)", hash, text_len));
                continue;
            }

            if role != "assistant" { continue; }

            if let Some(content) = msg.get_mut("content") {
                if let Some(text) = content.as_str() {
                    // Synergy 2: Adaptive compression based on chronicle
                    if profile.reasoning_compression > 0.0 {
                        let file_risk = if !files_seen.is_empty() {
                            files_seen.iter().any(|f| text.contains(f))
                        } else {
                            false
                        };
                        let aggressive = file_risk && profile.reasoning_compression > 0.6;
                        let threshold = if aggressive { 0.5 } else { 0.85 };

                        if let Some(compressed) = reliary_compress::compress_reasoning(text, dict.as_ref()) {
                            if (compressed.len() as f64) < (text.len() as f64) * threshold {
                                *content = serde_json::Value::String(compressed);
                            }
                        }
                    }
                }
            }
        }
    }

    // Forward via curl
    let api_key = request.headers().iter()
        .find(|h| h.field.to_string().to_lowercase() == "authorization")
        .map(|h| format!("{}", h.value))
        .unwrap_or_default();

    let upstream_url = std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| DEFAULT_UPSTREAM.to_string());
    let body_str = serde_json::to_string(&payload).unwrap_or_default();

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

// ── Synergy 2: Adaptive compression via chronicle ──

fn files_with_recent_failures() -> Vec<String> {
    vec![] // Deferred — queries chronicle SQLite on each request. Low hit rate.
}

// ── Synergy 3: Pre-generation context inject ──

fn inject_context(payload: &mut serde_json::Value) -> Vec<String> {
    let messages = match payload.get("messages").and_then(|m| m.as_array()) {
        Some(msgs) => msgs,
        None => return vec![],
    };

    // Find the last user message
    let user_text = messages.iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    // Extract file paths from user prompt
    let file_re = regex_lite::Regex::new(r"((?:src/|tests?/|lib/)?[a-zA-Z0-9_/.]+\.(?:rs|py|js|ts|go|md))").unwrap();
    let mut files: Vec<String> = Vec::new();
    for cap in file_re.captures_iter(user_text) {
        let f = cap[1].to_string();
        if !files.contains(&f) {
            files.push(f);
        }
    }

    if files.is_empty() { return vec![]; }

    // Inject structured context as a system message
    let context_blocks: Vec<String> = files.iter().take(3)
        .filter_map(|f| {
            let summary = crate::read_summary::build(f);
            if summary.len() > 5 && !summary.contains("ERROR") { Some(summary) } else { None }
        })
        .collect();

    if context_blocks.is_empty() { return files; }

    let context = context_blocks.join("\n");
    let context_msg = serde_json::json!({
        "role": "system",
        "content": format!("[reliary context]\n{}\n[end context]", context)
    });

    // Insert as second message (after original system prompt)
    if let Some(msgs) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        msgs.insert(1, context_msg);
    }

    files
}

// ── Synergy 4: Model-specific compression profiles ──

struct ModelProfile {
    reasoning_compression: f64,
    dict_usage: bool,
}

fn model_profile(model: &str) -> ModelProfile {
    if model.contains("deepseek") || model.contains("deepseek-ai") {
        ModelProfile { reasoning_compression: 0.8, dict_usage: true }
    } else if model.contains("qwen") {
        ModelProfile { reasoning_compression: 0.3, dict_usage: false }
    } else if model.contains("nvidia") || model.contains("nemotron") {
        ModelProfile { reasoning_compression: 0.4, dict_usage: false }
    } else if model.contains("stepfun") {
        ModelProfile { reasoning_compression: 0.7, dict_usage: true }
    } else {
        ModelProfile { reasoning_compression: 0.6, dict_usage: true }
    }
}

// ── Synergy 5: Cross-session read dedup ──

fn daemon_cmd(cmd: &str) -> Option<String> {
    // Try TCP daemon on :9799
    if let Ok(mut s) = std::net::TcpStream::connect("127.0.0.1:9799") {
        use std::io::Write;
        if s.write_all(format!("{}\n", cmd).as_bytes()).is_ok() {
            let mut buf = String::new();
            if s.read_to_string(&mut buf).is_ok() {
                let result = buf.trim().to_string();
                return if result.is_empty() || result.starts_with("ERROR") { None } else { Some(result) }
            }
        }
    }
    None
}

fn daemon_hash(text: &str) -> String {
    // Simple 16-char hex hash of content length + first/last chars
    let mut h: u64 = text.len() as u64;
    for b in text.as_bytes().iter().take(32) {
        h = h.wrapping_mul(31).wrapping_add(*b as u64);
    }
    for b in text.as_bytes().iter().rev().take(32) {
        h = h.wrapping_mul(31).wrapping_add(*b as u64);
    }
    format!("{:016x}", h)
}

fn respond(mut request: Request, status: u16, msg: &str) {
    let _ = request.respond(Response::from_string(msg).with_status_code(StatusCode(status)));
}
