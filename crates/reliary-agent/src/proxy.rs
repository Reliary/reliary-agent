/// Unified HTTP server on :9090. Serves both daemon endpoints (GET) and proxy (POST /v1/...).
/// No separate TCP daemon on :9799. Everything through one port, one process.

use tiny_http::{Server, Response, Request, Header, StatusCode};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Mutex, LazyLock, Arc};
use crate::session_state::SessionState;

static ROUTES: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(|| {
    let path = std::env::var("HOME").unwrap_or_default() + "/.reliary/proxy-routes.json";
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(routes) = serde_json::from_str::<HashMap<String, String>>(&content) {
            return Mutex::new(routes);
        }
    }
    Mutex::new(HashMap::new())
});

static RESPONSE_CACHE: LazyLock<Mutex<HashMap<u64, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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

fn get_upstream(auth_key: &str) -> Option<String> {
    let routes = ROUTES.lock().ok()?;
    if let Some(url) = routes.get(auth_key) {
        return Some(url.clone());
    }
    drop(routes);
    let discovered = crate::routes::discover_upstream(auth_key);
    if let Some(ref url) = discovered {
        if let Ok(mut routes) = ROUTES.lock() {
            routes.insert(auth_key.to_string(), url.clone());
        }
    }
    discovered
}

pub fn start(port: u16, state: Arc<SessionState>) -> Result<(), String> {
    let scavenger_state = Arc::clone(&state);
    std::thread::Builder::new()
        .name("scavenger".into())
        .spawn(move || crate::scavenger::scavenger_loop(scavenger_state))
        .ok();

    let addr = format!("127.0.0.1:{}", port);
    let server = Server::http(&addr).map_err(|e| format!("http bind: {}", e))?;
    eprintln!("[reliary] listening on {} (workdir: {:?}, max connections: 50)", addr, state.workdir);

    for request in server.incoming_requests() {
        let method = request.method();
        let url = request.url().to_string();

        match *method {
            tiny_http::Method::Get => handle_daemon_route(request, &url, &state),
            tiny_http::Method::Post => handle_proxy(request),
            _ => { let _ = request.respond(Response::from_string("not found").with_status_code(404)); }
        }
    }
    Ok(())
}

// ── Daemon GET routes ──

fn handle_daemon_route(request: Request, url: &str, state: &SessionState) {
    let path = url.split('?').next().unwrap_or("");
    let params = parse_query(url);

    let files_root = state.workdir.to_string_lossy().to_string();

    match path {
        "/ping" => respond_text(request, 200, "pong\n"),
        "/health" => respond_json(request, 200, &format!("{{\"status\":\"ok\",\"workdir\":\"{}\"}}", files_root)),
        "/search" => {
            let q = params.get("q").map(|s| s.as_str()).unwrap_or("");
            let p = params.get("path").map(|s| s.as_str()).unwrap_or(".");
            let cmd = format!("search {} {}", q, p);
            let resp = crate::daemon::daemon_handle_cmd_str(&cmd, state);
            respond_text(request, 200, &resp);
        }
        "/risk" => {
            let file = params.get("file").map(|s| s.as_str()).unwrap_or("");
            let cmd = format!("risk {}", file);
            let resp = crate::daemon::daemon_handle_cmd_str(&cmd, state);
            respond_text(request, 200, &resp);
        }
        "/compress" => {
            let text = params.get("text").map(|s| s.as_str()).unwrap_or("");
            if text.is_empty() {
                respond_text(request, 400, "ERROR: need text param\n");
            } else {
                let cmd = format!("compress {}", text);
            let resp = crate::daemon::daemon_handle_cmd_str(&cmd, state);
                respond_text(request, 200, &resp);
            }
        }
        "/veto" => {
            let file = params.get("file").map(|s| s.as_str()).unwrap_or("");
            let text = params.get("text").map(|s| s.as_str()).unwrap_or("");
            let cmd = format!("veto {} {}", file, text);
            let resp = crate::daemon::daemon_handle_cmd_str(&cmd, state);
            respond_text(request, 200, &resp);
        }
        "/muzzle" => {
            let s = params.get("state").map(|s| s.as_str()).unwrap_or("");
            if s == "on" { state.set_muzzle(true); respond_text(request, 200, "muzzled\n"); }
            else if s == "off" { state.set_muzzle(false); respond_text(request, 200, "unmuzzled\n"); }
            else { respond_text(request, 400, "ERROR: state must be on|off\n"); }
        }
        "/prior" => {
            let p = params.get("path").map(|s| s.as_str()).unwrap_or(".");
            let cmd = format!("prior {}", p);
            let resp = crate::daemon::daemon_handle_cmd_str(&cmd, state);
            respond_text(request, 200, &resp);
        }
        "/read-summary" => {
            let file = params.get("file").map(|s| s.as_str()).unwrap_or("");
            let cmd = format!("read-summary {}", file);
            let resp = crate::daemon::daemon_handle_cmd_str(&cmd, state);
            respond_text(request, 200, &resp);
        }
        "/cache-read" => {
            let p = params.get("path").map(|s| s.as_str()).unwrap_or("");
            let h = params.get("hash").map(|s| s.as_str()).unwrap_or("");
            let len = params.get("len").map(|s| s.as_str()).unwrap_or("0");
            let cmd = format!("cache-read {} {} {}", p, h, len);
            let resp = crate::daemon::daemon_handle_cmd_str(&cmd, state);
            respond_text(request, 200, &resp);
        }
        "/check-read" => {
            let p = params.get("path").map(|s| s.as_str()).unwrap_or("");
            let h = params.get("hash").map(|s| s.as_str()).unwrap_or("");
            let cmd = format!("check-read {} {}", p, h);
            let resp = crate::daemon::daemon_handle_cmd_str(&cmd, state);
            respond_text(request, 200, &resp);
        }
        "/status" => respond_text(request, 200, "ok\n"),
        _ => respond_text(request, 404, "ERROR: unknown endpoint\n"),
    }
}

// ── Proxy POST route ──

fn handle_proxy(mut request: Request) {
    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        respond_json(request, 400, "{\"error\":\"read error\"}");
        return;
    }

    let mut payload = match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(v) => v,
        Err(e) => { respond_json(request, 400, &format!("{{\"error\":\"json parse: {}\"}}", e)); return; }
    };

    let auth_raw = request.headers().iter()
        .find(|h| h.field.to_string().to_lowercase() == "authorization")
        .map(|h| format!("{}", h.value))
        .unwrap_or_default();
    let auth_key = auth_raw.strip_prefix("Bearer ").unwrap_or(&auth_raw).to_string();

    let upstream_url = match get_upstream(&auth_key) {
        Some(url) => url,
        None => {
            respond_json(request, 403, "{\"error\":\"unknown api key. run 'rel init' to configure\"}");
            return;
        }
    };

    // Context filter
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let mut turn_count = 0;
        let mut to_keep: Vec<bool> = vec![true; messages.len()];
        for (i, msg) in messages.iter().enumerate() {
            match msg.get("role").and_then(|r| r.as_str()).unwrap_or("") {
                "user" => turn_count += 1,
                "tool" | "toolResult" if turn_count > 8 => to_keep[i] = false,
                _ => {}
            }
        }
        for i in (0..messages.len()).rev() {
            if !to_keep[i] { messages.remove(i); }
        }
    }

    // Response cache
    if let Some(messages) = payload.get("messages") {
        if let Ok(msg_str) = serde_json::to_string(messages) {
            if let Some(cached) = cached_response(&auth_key, &msg_str) {
                respond_json(request, 200, &cached);
                return;
            }
        }
    }

    // Feed-forward compression
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

    forward_to_api(request, &payload, &upstream_url);
}

fn forward_to_api(mut request: Request, payload: &serde_json::Value, upstream_url: &str) {
    let api_key = request.headers().iter()
        .find(|h| h.field.to_string().to_lowercase() == "authorization")
        .map(|h| format!("{}", h.value))
        .unwrap_or_default();

    let body_str = serde_json::to_string(payload).unwrap_or_default();

    let child = Command::new("curl")
        .args(["-s", "-X", "POST", upstream_url,
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
                respond_json(request, 502, "{\"error\":\"empty upstream response\"}");
            }
        }
        Err(e) => respond_json(request, 502, &format!("{{\"error\":\"curl: {}\"}}", e)),
    }
}

// ── Helpers ──

fn parse_query(url: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(query) = url.split('?').nth(1) {
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next().unwrap_or("").to_string();
            let val = urlencoding_decode(parts.next().unwrap_or(""));
            map.insert(key, val);
        }
    }
    map
}

fn urlencoding_decode(s: &str) -> String {
    // Simple URL decoder for ASCII paths
    let mut result = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            }
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    result
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
