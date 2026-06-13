/// Provider-agnostic proxy. Routes by Authorization header.
/// No model lists, no provider detection. Reads ~/.reliary/proxy-routes.json.

use tiny_http::{Server, Response, Request, Header, StatusCode};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Mutex, LazyLock, Arc};
use crate::session_state::SessionState;

const DEFAULT_UPSTREAM: &str = "https://api.deepinfra.com/v1/openai/chat/completions";

static RESPONSE_CACHE: LazyLock<Mutex<HashMap<u64, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static DAEMON_STATE: LazyLock<Mutex<Option<Arc<SessionState>>>> =
    LazyLock::new(|| Mutex::new(None));

fn get_state() -> Arc<SessionState> {
    let guard = DAEMON_STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard.clone().unwrap_or_else(|| Arc::new(SessionState::new(".")))
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

pub fn start(port: u16, daemon_state: Option<Arc<SessionState>>) -> Result<(), String> {
    if let Some(s) = daemon_state {
        if let Ok(mut guard) = DAEMON_STATE.lock() {
            *guard = Some(s);
        }
    }
    // Start scavenger thread with panic recovery
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

    // File descriptor check
    #[cfg(unix)] {
        if let Ok(limit) = rlimit::getrlimit(rlimit::Resource::NOFILE) {
            if limit.0 < 1024 {
                eprintln!("[reliary] WARNING: file descriptor limit is {} (recommended >= 1024)", limit.0);
            }
        }
    }

    let addr = format!("127.0.0.1:{}", port);
    let server = Server::http(&addr).map_err(|e| format!("proxy bind: {}", e))?;
    eprintln!("[reliary] listening on {}", addr);

    for request in server.incoming_requests() {
        let method = request.method();
        let url = request.url().to_string();

        // ── Health / Ping (no auth needed) ──
        if method == &tiny_http::Method::Get && url == "/health" {
            let ct = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
            let _ = request.respond(
                Response::from_string("{\"status\":\"ok\"}")
                    .with_status_code(200)
                    .with_header(ct)
            );
            continue;
        }
        if method == &tiny_http::Method::Get && url == "/ping" {
            let _ = request.respond(Response::from_string("pong").with_status_code(200));
            continue;
        }

        // ── Daemon GET routes ──
        if method == &tiny_http::Method::Get {
            let path = url.split('?').next().unwrap_or("");
            let params = parse_query(&url);
            let s = get_state();

            match path {
                "/search" => {
                    let q = params.get("q").map(|s| s.as_str()).unwrap_or("");
                    let p = params.get("path").map(|s| s.as_str()).unwrap_or(".");
                    respond_text(request, 200, &crate::daemon::daemon_handle_cmd_str(&format!("search {} {}", q, p), &s));
                }
                "/risk" => {
                    let f = params.get("file").map(|s| s.as_str()).unwrap_or("");
                    respond_text(request, 200, &crate::daemon::daemon_handle_cmd_str(&format!("risk {}", f), &s));
                }
                "/compress" => {
                    let t = params.get("text").map(|s| s.as_str()).unwrap_or("");
                    respond_text(request, 200, &crate::daemon::daemon_handle_cmd_str(&format!("compress {}", t), &s));
                }
                "/veto" => {
                    let f = params.get("file").map(|s| s.as_str()).unwrap_or("");
                    let t = params.get("text").map(|s| s.as_str()).unwrap_or("");
                    respond_text(request, 200, &crate::daemon::daemon_handle_cmd_str(&format!("veto {} {}", f, t), &s));
                }
                "/cache-read" => {
                    let p = params.get("path").map(|s| s.as_str()).unwrap_or("");
                    let h = params.get("hash").map(|s| s.as_str()).unwrap_or("");
                    let l = params.get("len").map(|s| s.as_str()).unwrap_or("0");
                    respond_text(request, 200, &crate::daemon::daemon_handle_cmd_str(&format!("cache-read {} {} {}", p, h, l), &s));
                }
                "/check-read" => {
                    let p = params.get("path").map(|s| s.as_str()).unwrap_or("");
                    let h = params.get("hash").map(|s| s.as_str()).unwrap_or("");
                    respond_text(request, 200, &crate::daemon::daemon_handle_cmd_str(&format!("check-read {} {}", p, h), &s));
                }
                "/muzzle" => {
                    let st = params.get("state").map(|s| s.as_str()).unwrap_or("");
                    if st == "on" { s.set_muzzle(true); respond_text(request, 200, "muzzled\n"); }
                    else if st == "off" { s.set_muzzle(false); respond_text(request, 200, "unmuzzled\n"); }
                    else { respond_text(request, 400, "ERROR: state must be on|off\n"); }
                }
                "/prior" => {
                    let p = params.get("path").map(|s| s.as_str()).unwrap_or(".");
                    respond_text(request, 200, &crate::daemon::daemon_handle_cmd_str(&format!("prior {}", p), &s));
                }
                "/read-summary" => {
                    let f = params.get("file").map(|s| s.as_str()).unwrap_or("");
                    respond_text(request, 200, &crate::daemon::daemon_handle_cmd_str(&format!("read-summary {}", f), &s));
                }
                "/status" => respond_text(request, 200, "ok\n"),
                _ => { let _ = request.respond(Response::from_string("not found").with_status_code(404)); }
            }
            continue;
        }

        // ── Proxy POST route ──
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

    let upstream_url = std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| DEFAULT_UPSTREAM.to_string());
    let body_str = serde_json::to_string(payload).unwrap_or_default();

    let child = Command::new("curl")
        .args(["-s", "-X", "POST", &upstream_url,
               "-H", &format!("Authorization: {}", api_key),
               "-H", "Content-Type: application/json",
               "--data-binary", "@-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    match child {
        Ok(mut c) => {
            // Write body via stdin (avoids OS argument length limit)
            if let Some(ref mut stdin) = c.stdin {
                let _ = stdin.write_all(body_str.as_bytes());
            }
            drop(c.stdin.take());
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
                respond_json(request, 502, "{\"error\":\"empty upstream response\"}");
            }
        }
        Err(e) => respond_json(request, 502, &format!("{{\"error\":\"curl: {}\"}}", e)),
    }
}

fn handle_request(mut request: Request) {
    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        respond_json(request, 400, "{\"error\":\"read error\"}");
        return;
    }

    let mut payload = match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(v) => v,
        Err(e) => { respond_json(request, 400, &format!("{{\"error\":\"json parse: {}\"}}", e)); return; }
    };

    let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

    // ── Synergy 3: Context filter ──
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let mut turn_count = 0;
        let mut to_keep: Vec<bool> = vec![true; messages.len()];

        for (i, msg) in messages.iter().enumerate() {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            match role {
                "user" => { turn_count += 1; }
                "tool" | "toolResult" if turn_count > 8 => { to_keep[i] = false; }
                _ => {}
            }
        }

        for i in (0..messages.len()).rev() {
            if !to_keep[i] { messages.remove(i); }
        }
    }

    // ── Synergy 1: Response cache ──
    if let Some(messages) = payload.get("messages") {
        if let Ok(msg_str) = serde_json::to_string(messages) {
            if let Some(cached) = cached_response(&model, &msg_str) {
                respond_json(request, 200, &cached);
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
                    if let Some(compressed) = reliary_compress::compress_reasoning(text, dict.as_ref()) {
                        *content = serde_json::Value::String(compressed);
                    }
                }
            }
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

// ── Helpers ──

fn parse_query(url: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(query) = url.split('?').nth(1) {
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next().unwrap_or("").to_string();
            let val = parts.next().unwrap_or("").to_string();
            map.insert(key, val);
        }
    }
    map
}

fn respond_text(mut request: Request, status: u16, msg: &str) {
    let _ = request.respond(Response::from_string(msg).with_status_code(StatusCode(status)));
}

fn respond_json(mut request: Request, status: u16, msg: &str) {
    let ct = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    let _ = request.respond(
        Response::from_string(msg)
            .with_status_code(StatusCode(status))
            .with_header(ct)
    );
}
