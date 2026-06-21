//! Integration tests for hologram rendering.
//!
//! These tests require an indexed repo to exist. They use the public Reliary
//! clone at /tmp/opencode/bench-autocall/repo when available, otherwise skip.

use std::path::PathBuf;
use std::process::Command;

fn repo_path() -> Option<PathBuf> {
    let p = PathBuf::from("/tmp/opencode/bench-autocall/repo");
    if p.join(".reliary/index.sqlite").exists() {
        Some(p)
    } else {
        None
    }
}

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_reliary-agent"))
}

#[test]
fn renders_repo_hologram_with_prompt() {
    let Some(repo) = repo_path() else {
        eprintln!("skipping: bench repo not indexed");
        return;
    };

    let out = Command::new(binary_path())
        .arg("hologram")
        .arg("compression strategy")
        .arg(repo.to_str().unwrap())
        .arg("--top-k")
        .arg("5")
        .output()
        .expect("failed to run reliary-agent hologram");

    assert!(out.status.success(), "hologram failed: {:?}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("# hologram:"), "missing header in output");
    assert!(stdout.contains("defs:"), "expected defs line in entries");
    assert!(stdout.contains("[body:") || stdout.contains("(file not found"), "expected body markers or fallback");
}

#[test]
fn hologram_is_deterministic() {
    let Some(repo) = repo_path() else {
        eprintln!("skipping: bench repo not indexed");
        return;
    };

    let run = || -> String {
        let out = Command::new(binary_path())
            .arg("hologram")
            .arg(repo.to_str().unwrap())
            .arg("compression strategy")
            .arg("--top-k")
            .arg("3")
            .output()
            .expect("failed to run reliary-agent hologram");
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    let a = run();
    let b = run();
    assert_eq!(a, b, "hologram output is non-deterministic across runs");
}

#[test]
fn hologram_respects_top_k() {
    let Some(repo) = repo_path() else {
        eprintln!("skipping: bench repo not indexed");
        return;
    };

    let out = Command::new(binary_path())
        .arg("hologram")
        .arg(repo.to_str().unwrap())
        .arg("compression")
        .arg("--top-k")
        .arg("2")
        .output()
        .expect("failed to run");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Count ## file headers (each rendered entry)
    let entry_count = stdout.matches("## /").count();
    assert!(entry_count <= 2, "expected at most 2 entries with --top-k 2, got {}", entry_count);
}

#[test]
fn hologram_without_prompt_returns_index_overview() {
    let Some(repo) = repo_path() else {
        eprintln!("skipping: bench repo not indexed");
        return;
    };

    let out = Command::new(binary_path())
        .arg("hologram")
        .arg(repo.to_str().unwrap())
        .output()
        .expect("failed to run");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("# hologram:"));
    assert!(stdout.contains("files indexed"));
}

#[test]
fn hologram_json_output_is_valid() {
    let Some(repo) = repo_path() else {
        eprintln!("skipping: bench repo not indexed");
        return;
    };

    let out = Command::new(binary_path())
        .arg("hologram")
        .arg("compression")
        .arg(repo.to_str().unwrap())
        .arg("--json")
        .arg("--top-k")
        .arg("3")
        .output()
        .expect("failed to run");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .expect("hologram --json output must be valid JSON");
    assert!(parsed.get("entries").is_some(), "missing 'entries' field");
    assert!(parsed.get("indexed_files").is_some(), "missing 'indexed_files' field");
}