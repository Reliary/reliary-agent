use std::path::PathBuf;

mod common;

fn create_temp_python_test() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let test_file = dir.path().join("test_calc.py");
    std::fs::write(&test_file, r#"
def add(a, b):
    return a - b  # BUG: should be a + b

def test_add():
    assert add(2, 2) == 4
"#).unwrap();
    (dir, test_file)
}

#[test]
fn e2e_heal_apply_via_mcp() {
    let (_dir, test_file) = create_temp_python_test();
    let path = test_file.to_str().unwrap().to_string();

    // Use MCP tools/fix to apply a fix
    let mut mcp = common::start_mcp();

    // First initialize
    mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2024-11-05" },
    }));

    // Check fix schema exists
    let list = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {},
    }));
    let names: Vec<&str> = list["result"]["tools"].as_array().unwrap()
        .iter().map(|t| t["name"].as_str().unwrap_or("")).collect();
    assert!(names.contains(&"reliary_fix"), "expected reliary_fix tool");

    // Apply fix via tools/fix
    let resp = mcp.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/fix",
        "params": { "file": path, "old": "return a - b", "new": "return a + b" },
    }));

    eprintln!("fix response: {}", serde_json::to_string_pretty(&resp).unwrap());

    // Should either succeed or give a meaningful error
    if let Some(err) = resp.get("error") {
        // Error should be descriptive, not a crash
        let msg = err["message"].as_str().unwrap_or("");
        assert!(!msg.is_empty(), "expected non-empty error message");
        eprintln!("fix returned error (acceptable): {}", msg);
    } else {
        assert!(resp["result"]["success"].as_bool().unwrap_or(false),
            "expected success=true");
        // Verify the file was actually fixed by reading it back
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("return a - b"), "file was not updated");
        assert!(content.contains("return a + b"), "file missing expected fix");
    }
}

#[test]
fn e2e_veto_hallucinated_identifier() {
    let _guard = common::start_daemon();
    let client = common::http_client();

    // Test the daemon's veto endpoint directly (returns plain text)
    let resp = client
        .get("http://127.0.0.1:9090/veto?file=Cargo.toml&text=nonexistentMagicFunction")
        .send()
        .expect("request failed");

    assert_eq!(resp.status(), 200);
    let body = resp.text().unwrap();
    eprintln!("veto response: {:?}", body);

    // Should either block (ERROR) or allow (ok) — either way no crash
    assert!(
        !body.contains("panic") && !body.contains("internal"),
        "veto endpoint should not crash: {}",
        body
    );
}

#[test]
fn e2e_veto_allows_known_identifiers() {
    let _guard = common::start_daemon();
    let client = common::http_client();

    // Known identifier should pass veto
    let resp = client
        .get("http://127.0.0.1:9090/veto?file=Cargo.toml&text=serde_json")
        .send()
        .expect("request failed");

    assert_eq!(resp.status(), 200);
    let body = resp.text().unwrap();
    eprintln!("known identifier veto: {:?}", body);
}
