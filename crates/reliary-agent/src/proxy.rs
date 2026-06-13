/// Provider-agnostic proxy. Routes by Authorization header.
/// No model lists, no provider detection. Reads ~/.reliary/proxy-routes.json.

use tiny_http::{Server, Response, Request, Header, StatusCode};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Mutex, LazyLock};

static ROUTES: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(|| {
    let path = std::env::var("HOME").unwrap_or_default()
        + "/.reliary/proxy-routes.json";
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

fn get_upstream(auth_key: &str) -> Option<String> {
    let routes = ROUTES.lock().ok()?;
    if let Some(url) = routes.get(auth_key) {
        return Some(url.clone());
    }
    // Auto-discovery: scan agent configs
    drop(routes);
    let discovered = crate::routes::discover_upstream(auth_key);
    if let Some(ref url) = discovered {
        if let Ok(mut routes) = ROUTES.lock() {
            routes.insert(auth_key.to_string(), url.clone());
        }
    }
    discovered
}

pub fn start(port: u16) -> Result<(), String> {
    let addr = format!("127.0.0.1:{}", port);
    let server = Server::http(&addr).map_err(|e| format!("proxy bind: {}", e))?;
    eprintln!("[reliary] proxy listening on {}", addr);
    for request in server.incoming_requests() {
        if request.method() == &tiny_http::Method::Post {
            handle_request(request);
        } else {
            let _ = request.respond(Response::from_string("not found").with_status_code(404));
        }
    }
    Ok(())
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
                let ct = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
                let _ = request.respond(
                    tiny_http::Response::from_string(resp_body)
                        .with_status_code(200)
                        .with_header(ct)
                );
            } else {
                respond(request, 502, "{\"error\":\"empty upstream response\"}");
            }
        }
        Err(e) => respond(request, 502, &format!("{{\"error\":\"curl: {}\"}}", e)),
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
        Err(e) => { respond(request, 400, &format!("{{\"error\":\"json parse: {}\"}}", e)); return; }
    };

    // Extract auth key for routing
    let auth_key = request.headers().iter()
        .find(|h| h.field.to_string().to_lowercase() == "authorization")
        .map(|h| format!("{}", h.value))
        .unwrap_or_default();

    // Determine upstream URL from auth key
    let upstream_url = match get_upstream(&auth_key) {
        Some(url) => url,
        None => {
            respond(request, 403, &format!("{{\"error\":\"unknown api key. run 'rel init' to configure\"}}"));
            return;
        }
    };

    // ── Synergy 3: Context filter ──
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

    // ── Synergy 1: Response cache ──
    if let Some(messages) = payload.get("messages") {
        if let Ok(msg_str) = serde_json::to_string(messages) {
            if let Some(cached) = cached_response(&auth_key, &msg_str) {
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
                    if let Some(compressed) = reliary_compress::compress_reasoning(text, dict.as_ref()) {
                        *content = serde_json::Value::String(compressed);
                    }
                }
            }
        }
    }

    // Forward to the upstream determined by auth key
    forward_to_api(request, &payload, &upstream_url);
}

fn respond(mut request: Request, status: u16, msg: &str) {
    let _ = request.respond(Response::from_string(msg).with_status_code(StatusCode(status)));
}
