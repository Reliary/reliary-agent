//! Adversarial end-to-end tests.
//!
//! Hostile inputs: malformed source, invalid UTF-8, unterminated strings and
//! comments, pathological nesting and line lengths, plus path-traversal and
//! injection attempts against the MCP surface. The contract these tests
//! enforce is:
//!
//!   * the tool never panics, hangs, or corrupts its index;
//!   * it always returns a structured answer (or an explicit error);
//!   * well-formed files in the same corpus are still indexed correctly
//!     (degradation is partial, not total);
//!   * output is deterministic for a fixed input + index.
//!
//! Run: `cargo test -p reliary-agent --test e2e_adversarial`
mod common;

use common::{run_cli, Fixture, Mcp};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

/// Write a corpus of malformed / hostile source files into `root/src`.
fn write_hostile_corpus(root: &Path) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();

    // Invalid UTF-8 bytes.
    std::fs::write(src.join("invalid_utf8.rs"), b"pub fn badutf() { \xff\xfe\x00\x01 }\n")
        .unwrap();
    // NUL bytes.
    std::fs::write(src.join("nul_bytes.rs"), b"fn nulprobe() {\x00\x00}\n").unwrap();
    // A single 200 KB line.
    let mut long = String::from("pub fn longline() -> i32 { ");
    long.push_str(&"x".repeat(200_000));
    long.push_str(" }\n");
    std::fs::write(src.join("long_line.rs"), long).unwrap();
    // Empty file.
    std::fs::write(src.join("empty.rs"), "").unwrap();
    // Comments only.
    std::fs::write(src.join("only_comments.rs"), "// a\n/* b */\n# c\n*/\n").unwrap();
    // Unterminated double-quoted string.
    std::fs::write(
        src.join("unterminated_string.rs"),
        "pub fn unterm() { let s = \"never closed;\n}\n",
    )
    .unwrap();
    // Unterminated block comment.
    std::fs::write(
        src.join("unterminated_comment.rs"),
        "/* never closed\npub fn swallowed() {}\n",
    )
    .unwrap();
    // Unbalanced braces.
    std::fs::write(src.join("unbalanced.rs"), "pub fn unbalanced() { { { } }\n}}\n").unwrap();
    // 200 levels of nesting.
    let mut deep = String::from("pub fn deepnest() {\n");
    deep.push_str(&"  if x {\n".repeat(200));
    deep.push_str(&"  }\n".repeat(200));
    deep.push_str("}\n");
    std::fs::write(src.join("deep_nesting.rs"), deep).unwrap();
    // CRLF line endings.
    std::fs::write(src.join("crlf.rs"), b"pub fn crlfprobe() -> i32 {\r\n    7\r\n}\r\n").unwrap();
    // Non-ASCII identifiers and strings.
    std::fs::write(
        src.join("unicode.rs"),
        "pub fn \u{00e9}l\u{00e9}ment() -> &'static str { \"\u{4f60}\u{597d}\" }\n",
    )
    .unwrap();
    // Minified JS: one line, no whitespace.
    std::fs::write(
        src.join("minified.js"),
        format!("function a(){{return b()}}{}\n", "function b(){return 1}".repeat(200)),
    )
    .unwrap();

    // A well-formed control file: whatever happens to the hostile files, this
    // symbol must remain findable.
    std::fs::write(
        src.join("control.rs"),
        "/// A normal function.\npub fn control_symbol() -> i32 {\n    42\n}\n",
    )
    .unwrap();
}

fn hostile_fixture() -> Fixture {
    let fx = Fixture::new();
    write_hostile_corpus(fx.path());
    fx
}

// ── indexing ──────────────────────────────────────────────────────────────

#[test]
fn e2e_adv_trust_survives_hostile_corpus() {
    let fx = hostile_fixture();
    let start = Instant::now();
    fx.trust();
    assert!(
        start.elapsed() < Duration::from_secs(60),
        "trust on hostile input must not hang (took {:?})",
        start.elapsed()
    );

    // The index must be a valid, queryable SQLite database.
    let conn = rusqlite::Connection::open(fx.path().join(".reliary/index.sqlite")).unwrap();
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .expect("integrity_check must run");
    assert_eq!(integrity, "ok", "index failed SQLite integrity check");

    let files: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0))
        .unwrap();
    assert!(
        files >= 10,
        "most hostile files should still be registered, got {}",
        files
    );
}

#[test]
fn e2e_adv_wellformed_file_survives_hostile_neighbours() {
    // Degradation must be partial: a clean file in the same corpus stays
    // fully indexed and queried normally.
    let fx = hostile_fixture();
    fx.trust();

    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();
    let text = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "control_symbol", "def_only": true }),
    );
    assert!(
        text.contains("control_symbol"),
        "the clean control file must still be indexed: {}",
        text
    );
    assert!(
        text.contains("control.rs"),
        "control symbol must resolve to control.rs: {}",
        text
    );
}

#[test]
fn e2e_adv_trust_is_deterministic_on_hostile_input() {
    // Two trusts of the same hostile corpus must produce the same file set and
    // the same phrase count — no data-dependent nondeterminism.
    let fx = hostile_fixture();

    fx.trust();
    let snapshot = |root: &Path| -> Vec<(String, i64)> {
        let conn = rusqlite::Connection::open(root.join(".reliary/index.sqlite")).unwrap();
        let mut rows: Vec<(String, i64)> = conn
            .prepare("SELECT file_path, COALESCE((SELECT COUNT(*) FROM file_phrases fp WHERE fp.file_id = fm.id), 0) FROM file_map fm ORDER BY file_path")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        rows.sort();
        rows
    };
    let first = snapshot(fx.path());

    fx.trust();
    let second = snapshot(fx.path());
    assert_eq!(first, second, "re-trust must be byte-stable on hostile input");
}

// ── querying ──────────────────────────────────────────────────────────────

#[test]
fn e2e_adv_queries_on_hostile_index_do_not_crash() {
    let fx = hostile_fixture();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    // Every tool must answer, even when the corpus contains malformed files.
    for (tool, args) in [
        ("reliary_search", json!({ "query": "invalid_utf8" })),
        ("reliary_find_references", json!({ "name": "unterminated_string" })),
        ("reliary_find_references", json!({ "name": "longline", "def_only": true })),
        ("reliary_find_references", json!({ "name": "deepnest", "def_only": true })),
        ("reliary_call_graph", json!({ "name": "control_symbol" })),
        ("reliary_describe", json!({ "name": "control_symbol" })),
        ("reliary_find_dead_code", json!({ "path": "src" })),
        ("reliary_verify", json!({ "text": "control_symbol at src/control.rs:2" })),
    ] {
        let resp = mcp.call_tool(tool, args.clone());
        assert!(
            resp.get("result").is_some() || resp.get("error").is_some(),
            "{} with {:?} must answer, not crash: {}",
            tool,
            args,
            resp
        );
        assert!(mcp.is_alive(), "{} died on {:?}", tool, args);
    }
}

#[test]
fn e2e_adv_pathological_symbol_names_are_handled() {
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    // Names that stress the phrase index and SQL layers.
    for name in [
        "",
        " ",
        "a".repeat(500).as_str(),
        "\u{4f60}\u{597d}",
        "'; DROP TABLE phrases; --",
        "%_%",
        "../../../etc/passwd",
        "\n\t\r",
        "alpha\0beta",
    ] {
        let resp = mcp.call_tool("reliary_find_references", json!({ "name": name }));
        assert!(
            resp.get("result").is_some() || resp.get("error").is_some(),
            "hostile symbol name {:?} must not crash: {}",
            name,
            resp
        );
        assert!(mcp.is_alive(), "server died on symbol name {:?}", name);
    }

    // The index must still work afterwards — proving no SQL injection.
    let text = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "alpha", "def_only": true }),
    );
    assert!(
        text.contains("alpha"),
        "index must remain intact after injection attempts: {}",
        text
    );
}

#[test]
fn e2e_adv_query_results_are_deterministic() {
    let fx = hostile_fixture();
    fx.trust();

    let args = json!({ "name": "control_symbol" });
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();
    let first = mcp.call_tool_text("reliary_find_references", args.clone());
    let second = mcp.call_tool_text("reliary_find_references", args.clone());
    assert_eq!(
        first, second,
        "identical queries must return byte-identical results (KV-cache safety)"
    );
    assert!(!first.is_empty(), "determinism test must not be vacuous");
}

#[test]
fn e2e_adv_reindex_after_edit_keeps_index_consistent() {
    // Editor hooks reindex a single file after every write. Doing that on a
    // hostile corpus must leave the index valid and queries answered.
    let fx = hostile_fixture();
    fx.trust();

    let target = fx.path().join("src/control.rs");
    std::fs::write(&target, "/// Edited.\npub fn control_symbol() -> i32 {\n    43\n}\n").unwrap();

    let out = run_cli(
        &["reindex-file", "src/control.rs"],
        fx.path(),
        &[],
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!text.contains("panicked"), "reindex must not panic: {}", text);

    let conn = rusqlite::Connection::open(fx.path().join(".reliary/index.sqlite")).unwrap();
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok", "index corrupt after reindex");

    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();
    let answer = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "control_symbol", "def_only": true }),
    );
    assert!(
        answer.contains("control"),
        "edited symbol must still resolve: {}",
        answer
    );
}

// ── CLI robustness ────────────────────────────────────────────────────────

#[test]
fn e2e_adv_cli_on_missing_and_unreadable_paths() {
    let dir = tempfile::tempdir().unwrap();

    for args in [
        vec!["trust", "/nonexistent/definitely/not/here"],
        vec!["search", "x"],
        vec!["verify", ""],
        vec!["verify", "not a claim at all"],
        vec!["status"],
        vec!["doctor"],
        vec!["dead", "/nonexistent/path"],
        vec!["impact", "no_such_symbol"],
        vec!["test-plan", "--files", "/nonexistent/file.rs"],
    ] {
        let out = run_cli(&args, dir.path(), &[]);
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !combined.contains("panicked"),
            "`reliary {}` must not panic: {}",
            args.join(" "),
            combined
        );
        // An error exit code is fine; a signal (segfault/abort) is not.
        if let Some(code) = out.status.code() {
            assert!(
                (0..=2).contains(&code),
                "`reliary {}` exited with unexpected code {}: {}",
                args.join(" "),
                code,
                combined
            );
        } else {
            panic!("`reliary {}` was killed by a signal: {}", args.join(" "), combined);
        }
    }
}

#[test]
fn e2e_adv_wrap_handles_binary_and_empty_input() {
    let dir = tempfile::tempdir().unwrap();

    // Empty stdin.
    let out = run_cli(&["wrap", "cat", "/dev/null"], dir.path(), &[]);
    assert!(
        out.status.code().is_some(),
        "wrap must not be killed by a signal on empty input"
    );

    // Binary content.
    let bin = dir.path().join("blob.bin");
    std::fs::write(&bin, (0u8..=255).collect::<Vec<u8>>()).unwrap();
    let out = run_cli(&["wrap", "cat", bin.to_str().unwrap()], dir.path(), &[]);
    assert!(
        out.status.code().is_some(),
        "wrap must not be killed by a signal on binary input"
    );
}

// ── hook injection surface ────────────────────────────────────────────────

#[test]
fn e2e_adv_hook_rewrites_bash_and_rejects_injection() {
    // The hook reads the tool payload from stdin and the binary path from
    // RELIARY_BIN_PATH. This test drives the real interface: it asserts the
    // hook *rewrites* a rewriteable command, and that a metacharacter-laden
    // RELIARY_BIN_PATH is rejected before it can reach a shell.
    //
    // Requires `jq` (the hook exits 0 silently without it) and a real reliary
    // binary to point at.
    if std::process::Command::new("jq").arg("--version").output().is_err() {
        eprintln!("skipping: jq not installed");
        return;
    }
    let Some(reliary) = std::env::var_os("CARGO_BIN_EXE_reliary") else {
        eprintln!("skipping: CARGO_BIN_EXE_reliary not set");
        return;
    };
    let real_bin = reliary.to_string_lossy().to_string();
    let dir = tempfile::tempdir().unwrap();
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "git status --short" }
    })
    .to_string();

    // 1. Legitimate path: the hook must emit a rewrite decision.
    // hooks/ lives at the workspace root, two levels above the crate dir.
    let hook = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../hooks/claude-pretooluse.sh");
    assert!(hook.exists(), "hook not found at {}", hook.display());
    let cache = dir.path().join("cache-ok");
    std::fs::create_dir_all(&cache).unwrap();
    let mut child = std::process::Command::new("sh")
        .arg(&hook)
        .env("XDG_RUNTIME_DIR", &cache)
        .env("HOME", dir.path())
        .env("RELIARY_BIN_PATH", &real_bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.as_mut().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("permissionDecision"),
        "with a valid binary the hook must emit a decision: {}{}",
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("wrap bash -c"),
        "the rewrite must route through `reliary wrap`: {}",
        stdout
    );

    // 2. Hostile path: the dangerous payload is an executable whose path
    //    contains a single quote. The rewrite template is
    //    `'$RELIARY_BIN' wrap bash -c '...'` — a quote in the path would break
    //    out of that quoting when Claude Code executes the rewritten command.
    //    The allowlist must reject it so no rewrite decision is emitted.
    let evil_dir = dir.path().join("evi'l");
    std::fs::create_dir_all(&evil_dir).unwrap();
    let evil_bin = evil_dir.join("reliary");
    std::fs::write(&evil_bin, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&evil_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cache2 = dir.path().join("cache-hostile");
    std::fs::create_dir_all(&cache2).unwrap();
    let mut child = std::process::Command::new("sh")
        .arg(&hook)
        .env("XDG_RUNTIME_DIR", &cache2)
        .env("HOME", dir.path())
        .env("RELIARY_BIN_PATH", &evil_bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("permissionDecision"),
        "an executable path containing a quote must never be rewritten into a \
         shell command: {}",
        stdout
    );
    assert!(
        out.status.success(),
        "the hook must exit 0 on a hostile path (fail open, no rewrite)"
    );

    // 3. Metacharacter path: also rejected (belt and braces — this payload is
    //    not executable, so it would fail the -x check even without the
    //    allowlist; assert the behavior either way).
    let marker = dir.path().join("pwned");
    let injected = format!("reliary; touch {}", marker.display());
    let cache3 = dir.path().join("cache-metachar");
    std::fs::create_dir_all(&cache3).unwrap();
    let mut child = std::process::Command::new("sh")
        .arg(&hook)
        .env("XDG_RUNTIME_DIR", &cache3)
        .env("HOME", dir.path())
        .env("RELIARY_BIN_PATH", &injected)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("permissionDecision"),
        "a metacharacter-laden RELIARY_BIN_PATH must not be rewritten"
    );
    assert!(
        !marker.exists(),
        "a metacharacter in RELIARY_BIN_PATH must never reach the shell"
    );
}
