/// Deterministic replay proxy: records API responses, replays them identically.
/// Two modes via RELIARY_REPLAY env var:
///   "record" — forward requests, save responses to /tmp/reliary-replay.jsonl
///   "replay" — return saved responses, zero API calls, zero LLM variance
///
/// Uses ureq for inline HTTP (no subprocess, ~0.5ms per call vs curl's 3-15ms).

use tiny_http::{Server, Response, Header};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;

const DEFAULT_UPSTREAM: &str = "https://api.deepinfra.com/v1/openai/chat/completions";
const REPLAY_FILE: &str = "/tmp/reliary-replay.jsonl";

/// Replay cache: messages_json_hash → response_body
static REPLAY_CACHE: std::sync::LazyLock<Mutex<HashMap<u64, String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn replay_key(model: &str, messages_json: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    model.hash(&mut h);
    messages_json.hash(&mut h);
    h.finish()
}

fn ok_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .unwrap_or_else(|_| Header::from_bytes(&b"Content-Type"[..], &b"text/plain"[..]).unwrap())
}

fn load_replay_cache() {
    let path = std::path::Path::new(REPLAY_FILE);
    if !path.exists() { return; }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut cache = REPLAY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    for line in content.lines() {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(key) = val["key"].as_u64() {
                if let Some(resp) = val["response"].as_str() {
                    cache.insert(key, resp.to_string());
                }
            }
        }
    }
    eprintln!("[replay] loaded {} cached responses from {}", cache.len(), REPLAY_FILE);
}

/// Safe record: O(1) append, no API key in stored messages, no full file rewrite.
fn record_response(model: &str, messages_json: &str, response: &str, api_key: &str) {
    let mut sanitized = messages_json.to_string();
    if !api_key.is_empty() && sanitized.contains(api_key) {
        sanitized = sanitized.replace(api_key, "[REDACTED]");
    }
    let key = replay_key(model, &sanitized);
    let line = serde_json::json!({"key": key, "model": model, "response": response});
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(REPLAY_FILE)
    {
        let _ = writeln!(file, "{}", serde_json::to_string(&line).unwrap_or_default());
    }
}

pub fn start(port: u16) -> Result<(), String> {
    let is_replay = std::env::var("RELIARY_REPLAY").unwrap_or_default() == "replay";
    let is_record = std::env::var("RELIARY_REPLAY").unwrap_or_default() == "record";
    let replay_mode = is_replay || std::env::var("RELIARY_REPLAY").unwrap_or_default() == "dual";

    if is_replay { load_replay_cache(); }

    let addr = format!("127.0.0.1:{}", port);
    let server = Server::http(&addr).map_err(|e| format!("proxy bind: {}", e))?;
    eprintln!("[reliary] proxy listening on {} (replay: {}, record: {})", addr, is_replay, is_record);

    for mut request in server.incoming_requests() {
        let method = request.method();
        if method != &tiny_http::Method::Post {
            let _ = request.respond(Response::from_string("not found").with_status_code(404));
            continue;
        }

        let mut body = String::new();
        if request.as_reader().read_to_string(&mut body).is_err() {
            let _ = request.respond(Response::from_string("{\"error\":\"read error\"}").with_status_code(400));
            continue;
        }

        let payload = match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => v,
            Err(e) => {
                let _ = request.respond(Response::from_string(
                    format!("{{\"error\":\"json: {}\"}}", e)).with_status_code(400));
                continue;
            }
        };

        let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
        let msg_str = match payload.get("messages") {
            Some(m) => serde_json::to_string(m).unwrap_or_default(),
            None => String::new(),
        };

        // REPLAY mode: serve from cache
        if is_replay || replay_mode {
            let key = replay_key(&model, &msg_str);
            let cached_result = {
                let cache = REPLAY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
                cache.get(&key).cloned()
            };
            if let Some(cached) = cached_result {
                let _ = request.respond(
                    Response::from_string(cached)
                        .with_status_code(200)
                        .with_header(ok_header())
                );
                continue;
            }
            if !replay_mode {
                let _ = request.respond(Response::from_string(
                    "{\"error\":\"cache miss\"}").with_status_code(404));
                continue;
            }
        }

        // Forward to API using ureq (inline HTTP, no subprocess)
        let api_key = request.headers().iter()
            .find(|h| h.field.to_string().to_lowercase() == "authorization")
            .map(|h| format!("{}", h.value))
            .unwrap_or_default();

        let upstream_url = std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| DEFAULT_UPSTREAM.to_string());
        let body_str = serde_json::to_string(&payload).unwrap_or_default();

        let resp_body = match ureq::post(&upstream_url)
            .header("Authorization", &api_key)
            .header("Content-Type", "application/json")
            .send(body_str.as_bytes())
        {
            Ok(resp) => resp.into_body().read_to_string().unwrap_or_default(),
            Err(e) => format!("{{\"error\":\"proxy: {}\"}}", e),
        };

        if !resp_body.is_empty() {
            if is_record || replay_mode {
                record_response(&model, &msg_str, &resp_body, &api_key);
            }
            if replay_mode {
                let key = replay_key(&model, &msg_str);
                if let Ok(mut cache) = REPLAY_CACHE.lock() {
                    cache.insert(key, resp_body.clone());
                }
            }
            let _ = request.respond(
                Response::from_string(resp_body)
                    .with_status_code(200)
                    .with_header(ok_header())
            );
        } else {
            let _ = request.respond(Response::from_string(
                "{\"error\":\"empty upstream\"}").with_status_code(502));
        }
    }
    Ok(())
}
