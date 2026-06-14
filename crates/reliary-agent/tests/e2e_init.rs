use std::path::PathBuf;

mod common;

fn setup_fake_home() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let home = dir.path();

    // Mock Claude Code config
    let claude = home.join(".claude.json");
    std::fs::write(&claude, r#"{"mcpServers":{}}"#).unwrap();

    // Mock OpenCode config
    let opencode_dir = home.join(".config/opencode");
    std::fs::create_dir_all(&opencode_dir).unwrap();
    let opencode = opencode_dir.join("opencode.json");
    std::fs::write(&opencode, r#"{"mcpServers":{}}"#).unwrap();

    // Mock Pi config (needed for init to detect Pi)
    let pi_dir = home.join(".local/bin");
    std::fs::create_dir_all(&pi_dir).unwrap();
    let pi_bin = pi_dir.join("pi");
    std::fs::write(&pi_bin, "#!/bin/sh\necho pi 0.78.0").unwrap();

    dir
}

#[test]
fn e2e_init_injects_mcp_config() {
    let fake_home = setup_fake_home();
    let home = fake_home.path().to_str().unwrap().to_string();

    let mut child = std::process::Command::new(common::binary_path())
        .env("HOME", &home)
        .env("USER", "test")
        .arg("init")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to start init");

    // Feed answers: Y for Pi (no pi binary in fake home, won't fire),
    // Y for Claude, Y for OpenCode, N for Cline (no config), N for daemon
    if let Some(stdin) = child.stdin.as_mut() {
        use std::io::Write;
        stdin.write_all(b"Y\nY\nY\nN\nN\n").unwrap();
        stdin.flush().unwrap();
    }

    let result = child.wait_with_output().expect("init failed");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    eprintln!("init stdout: {}", stdout);
    eprintln!("init stderr: {}", stderr);

    // Verify Claude config was modified
    let claude_path = PathBuf::from(&home).join(".claude.json");
    let claude_content = std::fs::read_to_string(&claude_path).unwrap_or_default();
    let claude_json: serde_json::Value = serde_json::from_str(&claude_content).unwrap_or_default();
    let reliary_mcp = &claude_json["mcpServers"]["reliary"];
    assert!(
        !reliary_mcp.is_null(),
        "expected reliary MCP entry in Claude config. Content: {}",
        claude_content
    );
    assert_eq!(
        reliary_mcp["command"].as_str().unwrap_or(""),
        common::binary_path().canonicalize().unwrap().to_str().unwrap(),
        "MCP command should be binary path"
    );
    assert_eq!(
        reliary_mcp["args"][0].as_str().unwrap_or(""),
        "mcp",
        "MCP args should be 'mcp', not 'serve'"
    );

    // Verify OpenCode config was modified
    let opencode_path = PathBuf::from(&home).join(".config/opencode/opencode.json");
    let opencode_content = std::fs::read_to_string(&opencode_path).unwrap_or_default();
    let opencode_json: serde_json::Value = serde_json::from_str(&opencode_content).unwrap_or_default();
    let oc_mcp = &opencode_json["mcpServers"]["reliary"];
    assert!(
        !oc_mcp.is_null(),
        "expected reliary MCP entry in OpenCode config. Content: {}",
        opencode_content
    );
}

#[test]
fn e2e_uninstall_removes_mcp_config() {
    let fake_home = setup_fake_home();
    let home = fake_home.path().to_str().unwrap().to_string();

    // First run init — answer Y for all prompts
    let mut child = std::process::Command::new(common::binary_path())
        .env("HOME", &home)
        .env("USER", "test")
        .arg("init")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("failed to start init");
    if let Some(stdin) = child.stdin.as_mut() {
        use std::io::Write;
        stdin.write_all(b"Y\nY\nY\nN\nN\n").unwrap();
        stdin.flush().unwrap();
    }
    child.wait().unwrap();

    // Verify MCP entry exists
    let claude_path = PathBuf::from(&home).join(".claude.json");
    let before: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&claude_path).unwrap()).unwrap();
    assert!(before["mcpServers"]["reliary"].is_object(), "MCP entry should exist before uninstall");

    // Run uninstall — answer N for global config deletion
    let mut child = std::process::Command::new(common::binary_path())
        .env("HOME", &home)
        .env("USER", "test")
        .arg("uninstall")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run uninstall");
    if let Some(stdin) = child.stdin.as_mut() {
        use std::io::Write;
        stdin.write_all(b"N\n").unwrap();  // Don't delete global config
        stdin.flush().unwrap();
    }
    let output = child.wait_with_output().expect("uninstall failed");
    assert!(output.status.success(), "uninstall failed: {}", String::from_utf8_lossy(&output.stderr));

    // Verify MCP entry is gone
    let after: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&claude_path).unwrap()).unwrap();
    assert!(
        after["mcpServers"]["reliary"].is_null(),
        "MCP entry should be removed after uninstall, got: {:?}",
        after["mcpServers"]
    );
}
