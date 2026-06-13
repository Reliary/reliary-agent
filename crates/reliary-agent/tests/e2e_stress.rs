use std::time::Instant;

mod common;

#[test]
fn e2e_concurrent_load() {
    let _guard = common::start_daemon();
    let client = common::http_client();

    let start = Instant::now();
    let mut handles = vec![];

    for i in 0..50 {
        handles.push(std::thread::spawn(|| {
            let c = common::http_client();
            let resp = c
                .get("http://127.0.0.1:9090/ping")
                .send()
                .expect("request failed");
            assert_eq!(resp.status(), 200, "expected 200");
        }));
    }

    for (i, h) in handles.into_iter().enumerate() {
        h.join().expect(&format!("thread {} panicked", i));
    }

    let elapsed = start.elapsed();
    eprintln!("50 concurrent ping requests: {:?}", elapsed);

    // Verify daemon still responsive after load
    let resp = client
        .get("http://127.0.0.1:9090/health")
        .send()
        .expect("health check failed after load");
    assert_eq!(resp.status(), 200, "daemon not responsive after concurrent load");
}

#[test]
fn e2e_parallel_mcp_sessions() {
    let mut mcp1 = common::start_mcp();
    let mut mcp2 = common::start_mcp();

    // Both sessions should work independently
    let r1 = mcp1.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2024-11-05" },
    }));
    let r2 = mcp2.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2024-11-05" },
    }));

    assert_eq!(r1["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(r2["result"]["protocolVersion"], "2024-11-05");
}
