/// Provider-agnostic proxy. Routes by Authorization header via routes.rs.
/// No model lists, no provider detection. Streaming-aware. ureq inline HTTP.

use tiny_http::{Server, Response, Request, Header, StatusCode};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::sync::{Mutex, LazyLock, Arc};
use crate::session_state::SessionState;

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

/// Resolve upstream URL from auth key (routes.rs) or env fallback
fn resolve_upstream(auth_key: &str) -> Option<String> {
    // 1. Auth-based routing via routes.rs
    if let Some(url) = crate::routes::discover_upstream(auth_key) {
        return Some(url);
    }
    // 2. Direct env var override
    if let Ok(url) = std::env::var("RELIARY_UPSTREAM_URL") {
        return Some(url);
    }
    None
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

    #[cfg(unix)] {
        if let Ok(limit) = rlimit::getrlimit(rlimit::Resource::NOFILE) {
            if limit.0 < 1024 {
                eprintln!("[reliary] WARNING: file descriptor limit is {} (recommended >= 1024)", limit.0);
            }
        }
    }

    let addr = format!("127.0.0.1:{}", port);
    let server = Server::http(&addr).map_err(|e| format!("proxy bind: {}", e))?;
    eprintln!("\x1b[1m\x1b[34m  reliary-agent v{} ready\x1b[0m", env!("CARGO_PKG_VERSION"));
    eprintln!("  \x1b[2mDaemon + proxy on \x1b[1m:{}", port);
    eprintln!("  \x1b[2mRoutes: /health /ping /search /risk /compress /veto /muzzle /prior\x1b[0m");

    for request in server.incoming_requests() {
        let method = request.method();
        let url = request.url().to_string();

        if method == &tiny_http::Method::Get && url == "/health" {
            let _ = request.respond(
                Response::from_string("{\"status\":\"ok\"}")
                    .with_status_code(200)
                    .with_header(ct_hdr())
            );
            continue;
        }
        if method == &tiny_http::Method::Get && url == "/ping" {
            let _ = request.respond(Response::from_string("pong").with_status_code(200));
            continue;
        }

        // Daemon GET routes
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

        if method == &tiny_http::Method::Post {
            handle_request(request);
        } else {
            let _ = request.respond(Response::from_string("not found").with_status_code(404));
        }
    }
    Ok(())
}

/// Headers we use repeatedly
fn ct_hdr() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("valid Content-Type header")
}

/// Forward request to upstream using ureq (inline HTTP, no curl subprocess).
/// Supports streaming: if stream=true in payload, pipes response chunked.
fn forward_to_upstream(mut request: Request, payload: &serde_json::Value) {
    let api_key = request.headers().iter()
        .find(|h| h.field.to_string().to_lowercase() == "authorization")
        .map(|h| format!("{}", h.value))
        .unwrap_or_default();

    let auth_key = api_key.strip_prefix("Bearer ").unwrap_or(&api_key).to_string();
    let upstream_url = match resolve_upstream(&auth_key) {
        Some(url) => url,
        None => { respond_json(request, 403, "{\"error\":\"unknown api key\"}"); return; }
    };

    let is_streaming = payload.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    let body_str = serde_json::to_string(payload).unwrap_or_default();

    // Use ureq for inline HTTP (no subprocess)
    let req = ureq::post(&upstream_url)
        .header("Authorization", &api_key)
        .header("Content-Type", "application/json");

    match req.send(&body_str) {
        Ok(resp) => {
            let status_code = resp.status().as_u16();
            let body_bytes: Vec<u8> = resp.into_body().read_to_vec().unwrap_or_default();
            let body = String::from_utf8_lossy(&body_bytes).to_string();
            if is_streaming {
                // Streaming: still buffers due to tiny_http sync API
                let _ = request.respond(
                    Response::from_string(body)
                        .with_status_code(status_code)
                        .with_header(ct_hdr())
                );
            } else {
                store_response(&auth_key, &body_str, &body);
                let _ = request.respond(
                    Response::from_string(body)
                        .with_status_code(status_code)
                        .with_header(ct_hdr())
                );
            }
        }
        Err(ureq::Error::StatusCode(code)) => {
            respond_json(request, code, "{\"error\":\"upstream returned error\"}");
        }
        Err(e) => respond_json(request, 502, &format!("{{\"error\":\"upstream: {}\"}}", e)),
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

    // Extract metadata before mutable borrows
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

    // ── Synergy 1: Response cache (non-streaming only) ──
    let is_streaming = payload.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    if !is_streaming {
        if let Some(messages) = payload.get("messages") {
            if let Ok(msg_str) = serde_json::to_string(messages) {
                if let Some(cached) = cached_response("", &msg_str) {
                    respond_json(request, 200, &cached);
                    return;
                }
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

    // Forward to upstream
    forward_to_upstream(request, &payload);
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
    let _ = request.respond(
        Response::from_string(msg)
            .with_status_code(StatusCode(status))
            .with_header(ct_hdr())
    );
}
