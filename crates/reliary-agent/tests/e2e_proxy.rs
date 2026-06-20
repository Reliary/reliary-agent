mod common;

#[test]
fn e2e_proxy_round_trip() {
    if common::skip_without_live() {
        eprintln!("skipping e2e_proxy_round_trip: set RELIARY_E2E_LIVE=1");
        return;
    }
    let key = common::load_api_key().expect("no API key found");
    let _guard = common::start_daemon();
    let client = common::http_client();

    let resp = client
        .post("http://127.0.0.1:9090/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", key))
        .json(&serde_json::json!({
            "model": "deepseek/deepseek-v4-flash",
            "messages": [{"role": "user", "content": "say ok"}],
            "stream": false,
        }))
        .send()
        .expect("request failed");

    assert_eq!(resp.status(), 200, "expected 200 OK");

    let savings = resp.headers().get("x-reliaty-savings-pct");
    assert!(savings.is_some(), "expected x-reliaty-savings-pct header");

    let body: serde_json::Value = resp.json().expect("invalid JSON");
    let content = body["choices"][0]["message"]["content"].as_str();
    assert!(content.is_some(), "expected content in response");
    assert!(!content.unwrap().is_empty(), "expected non-empty content");
}

#[test]
fn e2e_proxy_streaming() {
    if common::skip_without_live() {
        eprintln!("skipping e2e_proxy_streaming: set RELIARY_E2E_LIVE=1");
        return;
    }
    let key = common::load_api_key().expect("no API key found");
    let _guard = common::start_daemon();
    let client = common::http_client();

    let mut resp = client
        .post("http://127.0.0.1:9090/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", key))
        .json(&serde_json::json!({
            "model": "deepseek/deepseek-v4-flash",
            "messages": [{"role": "user", "content": "say ok"}],
            "stream": true,
        }))
        .send()
        .expect("request failed");

    use std::io::Read;

    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("content-type").map(|v| v.to_str().unwrap_or("")).unwrap_or("");
    assert!(ct.contains("text/event-stream"), "expected SSE content-type, got: {}", ct);

    let mut buf = [0u8; 4096];
    let mut chunks = 0;
    let mut saw_done = false;
    loop {
        let n = resp.read(&mut buf).expect("read error");
        if n == 0 { break; }
        let text = String::from_utf8_lossy(&buf[..n]);
        if text.contains("[DONE]") {
            saw_done = true;
        }
        chunks += 1;
    }
    assert!(chunks > 1, "expected multiple SSE chunks, got {}", chunks);
    assert!(saw_done, "expected [DONE] sentinel");
}

#[test]
fn e2e_auth_routing() {
    let _guard = common::start_daemon();
    let client = common::http_client();

    // Unknown key -> 403
    // NOTE: The daemon is spawned with RELIARY_UPSTREAM_URL removed (see common/mod.rs)
    // so unknown keys are rejected even if the developer has the env var set.
    let resp = client
        .post("http://127.0.0.1:9090/v1/chat/completions")
        .header("Authorization", "Bearer sk-fake-key-xxxxx")
        .json(&serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false,
        }))
        .send()
        .expect("request failed");
    assert_eq!(resp.status(), 403, "expected 403 for unknown key");
    let body: serde_json::Value = resp.json().unwrap();
    let err = body["error"].as_str().unwrap_or("");
    assert!(err.contains("unknown") || err.contains("api key"), "expected unknown-key error, got: {}", err);

    // No key -> 403
    let resp = client
        .post("http://127.0.0.1:9090/v1/chat/completions")
        .json(&serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false,
        }))
        .send()
        .expect("request failed");
    assert_eq!(resp.status(), 403, "expected 403 for missing key");
}
