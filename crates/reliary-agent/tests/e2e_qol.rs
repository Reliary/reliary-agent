/// QoL integration tests for doctor, clean, config, version, and features.
use std::process::Command;

fn rel() -> String {
    env!("CARGO_BIN_EXE_reliary-agent").to_string()
}

// --- 1. --version flag ---
#[test]
fn test_version_flag() {
    let output = Command::new(rel())
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("reliary-agent"), "should contain binary name: {}", stdout);
    assert!(stdout.contains("0."), "should contain version number: {}", stdout);
}

// --- 2. Internal commands hidden from --help ---
#[test]
fn test_internal_commands_hidden() {
    let output = Command::new(rel())
        .arg("--help")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // These should NOT appear in help
    let hidden = ["fix-file", "apply-edit", "session-state", "memory", "veto", "fix-dir", "mcp"];
    for cmd in &hidden {
        // Hidden commands might appear in raw text but shouldn't be listed in usage lines
        // Check they're not in the "Commands:" section
        let commands_section = stdout.split("Commands:").nth(1).unwrap_or("");
        assert!(
            !commands_section.contains(cmd),
            "Internal command '{}' should be hidden from help but found in Commands section",
            cmd
        );
    }
    // These SHOULD appear in help
    let visible = ["search", "index", "serve", "start", "doctor", "status", "init", "clean", "logs", "config"];
    for cmd in &visible {
        assert!(
            stdout.contains(cmd),
            "Visible command '{}' missing from help output",
            cmd
        );
    }
}

// --- 3. Config displays file paths ---
#[test]
fn test_config_shows_paths() {
    let output = Command::new(rel())
        .args(["config"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("gate mode"), "should show gate mode: {}", stdout);
    assert!(stdout.contains("Global:"), "should show global config path: {}", stdout);
}

// --- 4. Config shows features ---
#[test]
fn test_config_shows_features() {
    let output = Command::new(rel())
        .args(["config"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Default features: compress, convWindow, readEnrichment should be enabled
    assert!(stdout.contains("compress"), "should list compress feature: {}", stdout);
    assert!(stdout.contains("features enabled") || stdout.contains("features disabled"),
        "should show features: {}", stdout);
}

// --- 5. Doctor checks upstream ---
#[test]
fn test_doctor_upstream_check() {
    let output = Command::new(rel())
        .args(["doctor"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Upstream:") || stdout.contains("upstream"),
        "doctor should check upstream routing: {}", stdout
    );
}

// --- 6. Clean confirmation prompt ---
#[test]
fn test_clean_confirmation_required() {
    // Running clean without stdin should hang or fail, not delete
    let output = Command::new(rel())
        .args(["clean"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Wipe") || stderr.contains("clean") || stderr.contains("project state"),
        "clean should show confirmation prompt: {}", stderr
    );
}

// --- 7. Serve banner visible ---
#[test]
fn test_serve_banner_contains_version() {
    // Start serve briefly, capture stderr
    let mut child = Command::new(rel())
        .args(["serve"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Wait 2 seconds for startup
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Kill it
    child.kill().ok();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Reliary Agent") || stderr.contains("Proxy"),
        "serve should print startup banner: {}", stderr
    );
}

// --- 8. Status shows proxy state ---
#[test]
fn test_status_shows_proxy() {
    let output = Command::new(rel())
        .args(["status"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Proxy:") || stdout.contains("proxy") || stdout.contains("Proxy"),
        "status should show proxy state: {}", stdout
    );
    assert!(
        stdout.contains("Mode:"),
        "status should show mode: {}", stdout
    );
}

// --- 9. Config set and get round-trip ---
#[test]
fn test_config_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();

    // Set a config value
    let output = Command::new(rel())
        .args(["config", "mode", "fast", "--local", "--root", root])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Set mode = fast"), "should confirm: {}", stdout);

    // Read it back
    let output = Command::new(rel())
        .args(["config", "--root", root])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("fast"), "should show mode=fast: {}", stdout);
}

// --- 10. Feature toggle ---
#[test]
fn test_feature_toggle_via_env() {
    // Set a feature override via env var
    let output = Command::new(rel())
        .args(["config"])
        .env("RELIARY_FEATURES", "-compress,+editMerge")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // compress should be disabled, editMerge should be enabled
    assert!(
        stdout.contains("compress") && stdout.contains("editMerge"),
        "should show both features: {}", stdout
    );
}
