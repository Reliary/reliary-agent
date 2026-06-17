/// QoL + config transparency + JSON output tests.
use std::process::Command;

fn rel() -> String {
    env!("CARGO_BIN_EXE_reliary-agent").to_string()
}

// --- Config transparency tests ---

#[test]
fn test_config_shows_source_default() {
    let output = Command::new(rel())
        .args(["config"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should show source for mode
    assert!(
        stdout.contains("from:") || stdout.contains("default"),
        "config should show source: {}", stdout
    );
}

#[test]
fn test_config_json_output() {
    let output = Command::new(rel())
        .args(["--format", "json", "config"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .expect(&format!("config --format json should produce valid JSON: {}", stdout));
    // Should contain mode and mode_source
    assert!(parsed.get("mode").is_some(), "JSON should have mode key");
    assert!(parsed.get("mode_source").is_some(), "JSON should have mode_source key");
    assert!(parsed.get("features").is_some(), "JSON should have features array");
}

#[test]
fn test_config_json_features_have_sources() {
    let output = Command::new(rel())
        .args(["--format", "json", "config"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let features = parsed.get("features").unwrap().as_array().unwrap();
    assert!(!features.is_empty(), "features array should not be empty");
    for f in features {
        assert!(f.get("name").is_some(), "feature should have name");
        assert!(f.get("enabled").is_some(), "feature should have enabled");
        assert!(f.get("source").is_some(), "feature should have source");
    }
}

#[test]
fn test_config_env_source_override() {
    let output = Command::new(rel())
        .args(["--format", "json", "config"])
        .env("RELIARY_MODE", "fast")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed.get("mode").unwrap().as_str(), Some("fast"));
    assert_eq!(parsed.get("mode_source").unwrap().as_str(), Some("env"));
}

#[test]
fn test_config_feature_env_source() {
    let output = Command::new(rel())
        .args(["--format", "json", "config"])
        .env("RELIARY_FEATURES", "-compress,+editMerge")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let features = parsed.get("features").unwrap().as_array().unwrap();
    let compress = features.iter().find(|f| f["name"] == "compress").unwrap();
    assert_eq!(compress["enabled"], false);
    assert_eq!(compress["source"], "env");
    let edit_merge = features.iter().find(|f| f["name"] == "editMerge").unwrap();
    assert_eq!(edit_merge["enabled"], true);
    assert_eq!(edit_merge["source"], "env");
}

// --- Doctor / Status tests ---

#[test]
fn test_doctor_output_format() {
    let output = Command::new(rel())
        .args(["doctor"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Doctor output uses lowercase names (daemon, upstream, etc.)
    assert!(stdout.contains("daemon"), "doctor should check daemon: {}", stdout);
    assert!(stdout.contains("upstream"), "doctor should check upstream: {}", stdout);
    assert!(stdout.contains("mode"), "doctor should check mode: {}", stdout);
}

#[test]
fn test_status_output_format() {
    let output = Command::new(rel())
        .args(["status"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Status output uses uppercase names with bullet points (Proxy:, Mode:, Routes:)
    assert!(stdout.contains("Proxy") || stdout.contains("proxy"), "status should show proxy: {}", stdout);
    assert!(stdout.contains("Mode") || stdout.contains("mode"), "status should show mode: {}", stdout);
    assert!(stdout.contains("Routes") || stdout.contains("routes"), "status should show routes: {}", stdout);
}

// --- CLI guardrails ---

#[test]
fn test_version_flag_works() {
    let output = Command::new(rel())
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("reliary-agent"));
}

#[test]
fn test_internal_commands_hidden() {
    let output = Command::new(rel())
        .arg("--help")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let hidden = ["fix-file", "apply-edit", "session-state", "memory", "veto", "fix-dir", "mcp"];
    for cmd in &hidden {
        let commands_section = stdout.split("Commands:").nth(1).unwrap_or("");
        assert!(
            !commands_section.contains(cmd),
            "Internal command '{}' should be hidden from help",
            cmd
        );
    }
}

#[test]
fn test_clean_requires_confirmation() {
    let output = Command::new(rel())
        .args(["clean"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Wipe") || stderr.contains("project state"),
        "clean should show confirmation prompt: {}", stderr
    );
}
