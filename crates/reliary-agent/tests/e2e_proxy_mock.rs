mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

/// Find a free port for the mock upstream server
fn free_port() -> u16 {
    static PORT: OnceLock<AtomicU16> = OnceLock::new();
    let p = PORT.get_or_init(|| AtomicU16::new(19000));
    p.fetch_add(1, Ordering::SeqCst)
}

/// SSE chunks that simulate a real streaming response
const SSE_CHUNKS: &[&[u8]] = &[
    b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1000000,\"model\":\"deepseek/deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
    b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1000000,\"model\":\"deepseek/deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
    b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1000000,\"model\":\"deepseek/deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
    b"data: [DONE]\n\n",
];

fn mock_upstream_handle(mut stream: TcpStream) {
    let mut buf = [0u8; 4096];
    // Read the HTTP request headers + body
    let n = stream.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]);
    let is_streaming = request.contains(r#""stream":true"#) || request.contains(r#""stream": true"#);

    if is_streaming {
        let headers = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
        let _ = stream.write_all(headers);
        for chunk in SSE_CHUNKS {
            let _ = stream.write_all(chunk);
            let _ = stream.flush();
            thread::sleep(Duration::from_millis(10));
        }
    } else {
        let body = r#"{"id":"chatcmpl-mock","object":"chat.completion","created":1000000,"model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"Hello world"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#;
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(headers.as_bytes());
        let _ = stream.write_all(body.as_bytes());
    }
}

fn start_mock_upstream() -> u16 {
    let port = free_port();
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).expect("mock upstream bind");
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    s.set_read_timeout(Some(Duration::from_secs(5))).ok();  // GUARDED: intentional
                    mock_upstream_handle(s);
                }
                Err(_) => break,
            }
        }
    });
    port
}

#[test]
fn e2e_proxy_mock_both() {
    let mock_port = start_mock_upstream();
    let upstream_url = format!("http://127.0.0.1:{}/v1/chat/completions", mock_port);
    let mock_key = "sk-mock-both-key";

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let routes_dir = std::path::Path::new(&home).join(".reliary");
    let routes_path = routes_dir.join("proxy-routes.json");
    let old_routes = std::fs::read_to_string(&routes_path).ok();  // GUARDED: intentional
    let _ = std::fs::create_dir_all(&routes_dir);
    std::fs::write(&routes_path, serde_json::json!({mock_key: upstream_url}).to_string()).ok();  // GUARDED: intentional

    let _guard = common::start_daemon();
    let client = common::http_client();

    // ── Streaming test ──
    let mut resp = client
        .post("http://127.0.0.1:9090/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", mock_key))
        .json(&serde_json::json!({
            "model": "mock-model",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true,
        }))
        .send()
        .expect("proxy request failed");

    use std::io::Read;

    assert_eq!(resp.status(), 200, "expected 200 OK");
    let ct = resp.headers().get("content-type").map(|v| v.to_str().unwrap_or("")).unwrap_or("");
    assert!(ct.contains("text/event-stream"), "expected SSE content-type, got: {}", ct);

    let mut buf = [0u8; 4096];
    let mut total = Vec::new();
    loop {
        match resp.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => total.extend_from_slice(&buf[..n]),
            Err(e) => {
                eprintln!("read error: {}", e);
                break;
            }
        }
    }

    let body = String::from_utf8_lossy(&total);
    assert!(body.contains("[DONE]"), "expected [DONE] sentinel");
    assert!(body.contains("Hello"), "expected content chunk");

    // ── Non-streaming test ──
    let resp = client
        .post("http://127.0.0.1:9090/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", mock_key))
        .json(&serde_json::json!({
            "model": "mock-model",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": false,
        }))
        .send()
        .expect("proxy request failed");

    assert_eq!(resp.status(), 200, "expected 200 OK");
    let json_body: serde_json::Value = resp.json().expect("invalid JSON");
    assert!(json_body.get("id").is_some() || json_body.get("choices").is_some(), "expected response body");

    // Restore proxy-routes.json
    if let Some(old) = old_routes {
        std::fs::write(&routes_path, old).ok();  // GUARDED: intentional
    } else {
        std::fs::remove_file(&routes_path).ok();  // GUARDED: intentional
    }
}
