//! V85 — end-to-end tests for `reliary warrant`.
//!
//! These drive the shipped binary against real git repositories: no mocks.
//! Each test asserts the exit code contract (0 closed, 1 open, 3
//! indeterminate, 2 usage) plus the certificate's evidence.

mod common;

use common::{run_cli, Fixture};
use std::process::Command;

/// Commit the fixture as-is so HEAD is the clean base.
fn commit_all(root: &std::path::Path) {
    for args in [
        vec!["add", "-A"],
        vec!["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "base"],
    ] {
        let st = Command::new("git")
            .args(&args)
            .current_dir(root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git");
        assert!(st.success(), "git {:?} failed", args);
    }
}

fn write(root: &std::path::Path, rel: &str, content: &str) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, content).unwrap();
}

fn contract_json(body: &str) -> String {
    format!(r#"{{"version":1,{}}}"#, body)
}

#[test]
fn warrant_complete_rename_exits_zero_and_writes_certificate() {
    let fx = Fixture::new();
    let root = fx.dir.path();
    commit_all(root);

    // Rename alpha -> omega everywhere.
    write(
        root,
        "src/lib.rs",
        "//! Fixture crate.\n/// Omega does a thing.\npub fn omega() -> i32 {\n    beta() + 1\n}\n",
    );
    write(
        root,
        "src/other.rs",
        "/// Beta does another thing.\npub fn beta() -> i32 {\n    41\n}\n",
    );
    // A caller of alpha in a second file (added as part of the change).
    write(root, "src/call.rs", "pub fn run() -> i32 { omega() }\n");

    write(root, "done.json", &contract_json(
        r#""conditions":[{"kind":"rename","from":"alpha","to":"omega"}]"#,
    ));

    let out = run_cli(
        &["warrant", "--base", "HEAD", "--contract", "done.json", "--cert", "cert.json"],
        root,
        &[],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected closed (0), stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cert = std::fs::read_to_string(root.join("cert.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&cert).unwrap();
    assert_eq!(v["verdict"], "closed");
    assert_eq!(v["conditions"][0]["verdict"], "closed");
    assert!(v["certificate_sha256"].as_str().unwrap().starts_with("sha256:"));
}

#[test]
fn warrant_def_only_rename_exits_one_with_the_missed_site() {
    let fx = Fixture::new();
    let root = fx.dir.path();
    commit_all(root);

    // Rename the definition but not its call site in lib.rs (alpha is called
    // inside `run` in the same file).
    write(
        root,
        "src/lib.rs",
        "//! Fixture crate.\npub fn omega() -> i32 {\n    beta() + 1\n}\npub fn run() -> i32 { alpha() }\n",
    );
    write(root, "done.json", &contract_json(
        r#""conditions":[{"kind":"rename","from":"alpha","to":"omega"}]"#,
    ));

    let out = run_cli(
        &["warrant", "--base", "HEAD", "--contract", "done.json"],
        root,
        &[],
    );
    assert_eq!(out.status.code(), Some(1), "expected open (1)");
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("certificate on stdout");
    assert_eq!(v["verdict"], "open");
    let ev = &v["conditions"][0]["evidence"][0];
    assert_eq!(ev["file"], "src/lib.rs");
    assert!(ev["excerpt"].as_str().unwrap().contains("alpha"));
    assert!(ev["excerpt_sha256"].as_str().unwrap().starts_with("sha256:"));
}

#[test]
fn warrant_undeclared_removal_exits_one() {
    let fx = Fixture::new();
    let root = fx.dir.path();
    commit_all(root);

    // Delete beta and its call, declare nothing.
    write(
        root,
        "src/lib.rs",
        "//! Fixture crate.\n/// Alpha does a thing.\npub fn alpha() -> i32 {\n    1\n}\n",
    );
    std::fs::remove_file(root.join("src/other.rs")).unwrap();
    write(root, "done.json", &contract_json(r#""conditions":[]"#));

    let out = run_cli(
        &["warrant", "--base", "HEAD", "--contract", "done.json"],
        root,
        &[],
    );
    assert_eq!(out.status.code(), Some(1), "expected open (1)");
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("certificate on stdout");
    assert!(
        v["delta"]["unclaimed_removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "beta"),
        "beta removal must be unclaimed: {}",
        v["delta"]
    );
}

#[test]
fn warrant_bad_contract_exits_two() {
    let fx = Fixture::new();
    let root = fx.dir.path();
    commit_all(root);
    write(root, "done.json", "{not json");
    let out = run_cli(
        &["warrant", "--base", "HEAD", "--contract", "done.json"],
        root,
        &[],
    );
    assert_eq!(out.status.code(), Some(2), "invalid contract must be usage error");
}

#[test]
fn warrant_certificate_replay_is_byte_identical() {
    let fx = Fixture::new();
    let root = fx.dir.path();
    commit_all(root);
    write(
        root,
        "src/lib.rs",
        "//! Fixture crate.\n/// Omega does a thing.\npub fn omega() -> i32 {\n    beta() + 1\n}\n",
    );
    write(root, "done.json", &contract_json(
        r#""conditions":[{"kind":"rename","from":"alpha","to":"omega"}]"#,
    ));

    let first = run_cli(
        &["warrant", "--base", "HEAD", "--contract", "done.json", "--cert", "cert.json"],
        root,
        &[],
    );
    assert_eq!(first.status.code(), Some(0));
    let a = std::fs::read(root.join("cert.json")).unwrap();
    let second = run_cli(
        &["warrant", "--base", "HEAD", "--contract", "done.json", "--cert", "cert.json"],
        root,
        &[],
    );
    assert_eq!(second.status.code(), Some(0));
    let b = std::fs::read(root.join("cert.json")).unwrap();
    assert_eq!(a, b, "certificate replay must be byte-identical");
}
