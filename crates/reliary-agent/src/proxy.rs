/// Bidirectional proxy: compresses conversation history before forwarding to API.
/// Listens on :9090, forwards to API upstream via curl subprocess.

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

    // Compress assistant messages (keep first 2 for context)
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

    let api_key = request.headers().iter()
        .find(|h| h.field.to_string().to_lowercase() == "authorization")
        .map(|h| format!("{}", h.value))
        .unwrap_or_default();

    let upstream_url = std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| DEFAULT_UPSTREAM.to_string());
    let body_str = serde_json::to_string(&payload).unwrap_or_default();

    // Forward via curl
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

fn respond(mut request: Request, status: u16, msg: &str) {
    let _ = request.respond(Response::from_string(msg).with_status_code(StatusCode(status)));
}
