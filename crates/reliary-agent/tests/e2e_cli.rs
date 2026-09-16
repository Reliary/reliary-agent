//! End-to-end tests for the CLI surface.
//!
//! Each test spawns the real `reliary` binary and asserts on stdout / stderr /
//! exit code, with `HOME` pointed at a temp dir so no user state is touched.
//!
//! Run: `cargo test -p reliary-agent --test e2e_cli`
mod common;

use common::{binary_path, run_cli, run_cli_stdin, Fixture};
use std::path::Path;

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Strip ANSI escapes so assertions do not depend on colour support.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // skip to 'm'
            for c2 in chars.by_ref() {
                if c2 == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ── trust / index ─────────────────────────────────────────────────────────

#[test]
fn e2e_cli_trust_builds_index() {
    let fx = Fixture::new();
    fx.trust();
    assert!(fx.path().join(".reliary/index.sqlite").exists());

    // The index must be a usable SQLite database with content.
    let conn = rusqlite::Connection::open(fx.path().join(".reliary/index.sqlite")).unwrap();
    let files: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0))
        .unwrap();
    assert!(files >= 2, "expected both fixture files indexed, got {}", files);
}

#[test]
fn e2e_cli_trust_is_idempotent() {
    let fx = Fixture::new();

    // Row counts across the whole index must not change on re-trust.
    // (V73 fixed a bug where every reindex duplicated phrase blobs.)
    let counts = |root: &Path| -> Vec<(String, i64)> {
        let conn = rusqlite::Connection::open(root.join(".reliary/index.sqlite")).unwrap();
        ["file_map", "phrases", "phrase_occ", "file_phrases", "file_stats"]
            .iter()
            .map(|t| {
                let n: i64 = conn
                    .query_row(&format!("SELECT COUNT(*) FROM {}", t), [], |r| r.get(0))
                    .unwrap_or(-1);
                (t.to_string(), n)
            })
            .collect()
    };

    fx.trust();
    let first = counts(fx.path());
    assert!(
        first.iter().all(|(_, n)| *n > 0),
        "first trust must populate every table: {:?}",
        first
    );

    fx.trust();
    let second = counts(fx.path());
    assert_eq!(
        first, second,
        "re-trust must not duplicate rows — index size drifted: {:?} -> {:?}",
        first, second
    );
}

// ── search ────────────────────────────────────────────────────────────────

#[test]
fn e2e_cli_search_finds_symbol_file() {
    let fx = Fixture::new();
    fx.trust();

    let out = run_cli(&["search", "alpha"], fx.path(), &[]);
    assert!(out.status.success(), "search failed: {}", stderr_of(&out));
    let text = plain(&stdout_of(&out));
    assert!(
        text.contains("lib.rs"),
        "search for alpha must name src/lib.rs: {}",
        text
    );
}

#[test]
fn e2e_cli_search_without_index_explains_itself() {
    // No trust: the CLI must offer to build the index (or explain), never
    // panic and never silently return nothing. Feed "n" so it exits without
    // indexing, then assert the prompt named the problem.
    let dir = tempfile::tempdir().unwrap();
    let out = run_cli_stdin(&["search", "anything"], dir.path(), &[], "n\n");
    let combined = plain(&format!("{}{}", stdout_of(&out), stderr_of(&out)));
    assert!(
        !combined.contains("panicked"),
        "search without index must not panic: {}",
        combined
    );
    let lower = combined.to_ascii_lowercase();
    assert!(
        lower.contains("no project index")
            || lower.contains("no index")
            || lower.contains("index")
            || lower.contains("trust"),
        "search without index must explain how to fix it: {}",
        combined
    );
    // Declining the prompt must not create an index.
    assert!(
        !dir.path().join(".reliary/index.sqlite").exists(),
        "declining the build prompt must leave the directory unindexed"
    );
}

// ── verify ────────────────────────────────────────────────────────────────

#[test]
fn e2e_cli_verify_true_claim() {
    let fx = Fixture::new();
    fx.trust();

    let out = run_cli(&["verify", "alpha at src/lib.rs:3"], fx.path(), &[]);
    let text = plain(&format!("{}{}", stdout_of(&out), stderr_of(&out)));
    assert!(
        text.contains("VERIFIED"),
        "a true claim must verify: {}",
        text
    );
}

#[test]
fn e2e_cli_verify_false_claim_exits_nonzero() {
    let fx = Fixture::new();
    fx.trust();

    let out = run_cli(&["verify", "zeta_missing at src/lib.rs:99000"], fx.path(), &[]);
    assert!(
        !out.status.success(),
        "a falsified claim must exit non-zero: {}",
        plain(&stdout_of(&out))
    );
    let text = plain(&format!("{}{}", stdout_of(&out), stderr_of(&out)));
    assert!(
        text.contains("FALSE") || text.contains("false"),
        "falsified claim should be reported: {}",
        text
    );
}

#[test]
fn e2e_cli_verify_json_is_valid_and_stable() {
    let fx = Fixture::new();
    fx.trust();

    let out = run_cli(&["verify", "--json", "alpha at src/lib.rs:3"], fx.path(), &[]);
    let text = stdout_of(&out);
    let parsed: serde_json::Value =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("verify --json invalid: {} ({})", e, text));
    assert!(parsed["verified"].is_number(), "json needs a verified count: {}", parsed);
    assert!(parsed["falsified"].is_number(), "json needs a falsified count: {}", parsed);
    assert!(parsed["claims"].is_array(), "json needs a claims array: {}", parsed);

    // Deterministic: same input, same index, identical bytes.
    let out2 = run_cli(&["verify", "--json", "alpha at src/lib.rs:3"], fx.path(), &[]);
    assert_eq!(
        text, stdout_of(&out2),
        "verify --json must be deterministic"
    );
}

// ── status / doctor ───────────────────────────────────────────────────────

#[test]
fn e2e_cli_status_reports_index() {
    let fx = Fixture::new();
    fx.trust();

    let out = run_cli(&["status"], fx.path(), &[]);
    assert!(out.status.success(), "status failed: {}", stderr_of(&out));
    let text = plain(&stdout_of(&out));
    assert!(
        text.contains("Index"),
        "status must report the index: {}",
        text
    );
    assert!(
        text.contains('2'),
        "status should report the 2 indexed files: {}",
        text
    );
}

#[test]
fn e2e_cli_doctor_runs_clean() {
    let fx = Fixture::new();
    fx.trust();

    let out = run_cli(&["doctor"], fx.path(), &[]);
    let text = plain(&format!("{}{}", stdout_of(&out), stderr_of(&out)));
    assert!(!text.contains("panicked"), "doctor must not panic: {}", text);
    assert!(
        text.contains("binary") || text.contains("index"),
        "doctor should report checks: {}",
        text
    );
}

#[test]
fn e2e_cli_doctor_without_index_still_runs() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_cli(&["doctor"], dir.path(), &[]);
    let text = plain(&format!("{}{}", stdout_of(&out), stderr_of(&out)));
    assert!(!text.contains("panicked"), "doctor must not panic: {}", text);
}

// ── wrap (bash compression) ───────────────────────────────────────────────

#[test]
fn e2e_cli_wrap_compresses_repetitive_output() {
    let dir = tempfile::tempdir().unwrap();
    let script = "for i in $(seq 1 40); do echo \"Compiling crate_$i v0.1.0\"; done; echo 'error: boom'";
    let out = run_cli(&["wrap", "bash", "-c", script], dir.path(), &[]);
    let text = plain(&stdout_of(&out));

    assert!(
        text.contains("error: boom"),
        "compression must preserve errors: {}",
        text
    );
    // 40 repetitive lines must have collapsed.
    let compiling_lines = text.lines().filter(|l| l.contains("Compiling")).count();
    assert!(
        compiling_lines < 40,
        "expected the 40 Compiling lines to collapse, got {}: {}",
        compiling_lines,
        text
    );
}

#[test]
fn e2e_cli_wrap_passes_through_source_reads() {
    // `cat <source file>` must NOT be compressed — read tools need the real bytes.
    let fx = Fixture::new();
    let src = fx.path().join("src/lib.rs");
    let out = run_cli(
        &["wrap", "cat", src.to_str().unwrap()],
        fx.path(),
        &[],
    );
    let text = stdout_of(&out);
    assert!(
        text.contains("pub fn alpha"),
        "cat of a source file must pass through unchanged: {}",
        text
    );
    assert!(
        text.contains("pub fn beta") || text.contains("beta()"),
        "full file body must be present: {}",
        text
    );
}

#[test]
fn e2e_cli_wrap_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let script = "for i in $(seq 1 25); do echo \"step $i\"; done";
    let a = run_cli(&["wrap", "bash", "-c", script], dir.path(), &[]);
    let b = run_cli(&["wrap", "bash", "-c", script], dir.path(), &[]);
    assert_eq!(
        stdout_of(&a),
        stdout_of(&b),
        "wrap output must be byte-identical across runs (KV-cache safety)"
    );
}

// ── init / uninstall (fake HOME) ──────────────────────────────────────────

/// Build a fake HOME with Claude Code and OpenCode configs present.
fn fake_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();
    let oc = home.path().join(".config/opencode");
    std::fs::create_dir_all(&oc).unwrap();
    std::fs::write(oc.join("opencode.json"), r#"{"mcpServers":{}}"#).unwrap();
    home
}

fn init_with(home: &Path, answers: &str) -> std::process::Output {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(binary_path())
        .arg("init")
        .current_dir(home)
        .env("HOME", home)
        .env("NO_RELIARY_WATCHER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn init");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(answers.as_bytes())
        .unwrap();
    child.wait_with_output().expect("init wait")
}

#[test]
fn e2e_cli_init_wires_mcp_entries() {
    let home = fake_home();
    let out = init_with(home.path(), "Y\nN\nY\nN\n");
    let text = plain(&format!("{}{}", stdout_of(&out), stderr_of(&out)));

    let claude: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join(".claude.json")).unwrap())
            .expect("claude config must stay valid JSON");
    assert!(
        claude["mcpServers"]["reliary"].is_object(),
        "init must add the reliary MCP server to Claude config: {}",
        claude
    );
    assert_eq!(
        claude["mcpServers"]["reliary"]["args"][0], "mcp",
        "MCP command must be the `mcp` subcommand: {}",
        claude
    );

    let oc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".config/opencode/opencode.json")).unwrap(),
    )
    .expect("opencode config must stay valid JSON");
    // OpenCode uses the "mcp" key (not "mcpServers").
    assert!(
        oc["mcp"]["reliary"].is_object() || oc["mcpServers"]["reliary"].is_object(),
        "init must add the reliary MCP server to OpenCode config: {}",
        oc
    );
    assert!(!text.contains("panicked"), "init must not panic: {}", text);
}

#[test]
fn e2e_cli_init_is_idempotent() {
    let home = fake_home();
    init_with(home.path(), "Y\nN\nY\nN\n");
    init_with(home.path(), "Y\nN\nY\nN\n");

    let claude: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join(".claude.json")).unwrap())
            .unwrap();
    // Exactly one entry — a second init must not duplicate servers or args.
    let servers = claude["mcpServers"].as_object().expect("mcpServers object");
    assert_eq!(
        servers.keys().filter(|k| *k == "reliary").count(),
        1,
        "second init duplicated the server: {}",
        claude
    );
    let args = claude["mcpServers"]["reliary"]["args"].as_array().unwrap();
    assert_eq!(args.len(), 1, "args must not accumulate: {:?}", args);
}

#[test]
fn e2e_cli_uninstall_removes_mcp_entries() {
    let home = fake_home();
    init_with(home.path(), "Y\nN\nY\nN\n");

    // Confirm it was wired first.
    let before: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join(".claude.json")).unwrap())
            .unwrap();
    assert!(
        before["mcpServers"]["reliary"].is_object(),
        "precondition: reliary must be wired"
    );

    let out = run_cli(&["uninstall"], home.path(), &[("HOME", home.path().to_str().unwrap())]);
    let text = plain(&format!("{}{}", stdout_of(&out), stderr_of(&out)));

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join(".claude.json")).unwrap())
            .expect("claude config must remain valid JSON after uninstall");
    assert!(
        after["mcpServers"].get("reliary").is_none(),
        "uninstall must remove the reliary MCP server: {}",
        after
    );
    assert!(!text.contains("panicked"), "uninstall must not panic: {}", text);
}

// ── completions / man ─────────────────────────────────────────────────────

#[test]
fn e2e_cli_completions_and_man_emit_output() {
    let dir = tempfile::tempdir().unwrap();
    for shell in ["bash", "zsh", "fish"] {
        let out = run_cli(&["completions", shell], dir.path(), &[]);
        assert!(
            out.status.success(),
            "completions {} failed: {}",
            shell,
            stderr_of(&out)
        );
        assert!(
            !stdout_of(&out).trim().is_empty(),
            "completions {} produced no output",
            shell
        );
    }

    let out = run_cli(&["man"], dir.path(), &[]);
    let text = format!("{}{}", stdout_of(&out), stderr_of(&out));
    assert!(
        text.contains("reliary"),
        "man page must mention the binary: {}",
        &text[..text.len().min(200)]
    );
}

// ── version ───────────────────────────────────────────────────────────────

#[test]
fn e2e_cli_version_matches_crate_version() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_cli(&["--version"], dir.path(), &[]);
    let text = stdout_of(&out);
    let expected = env!("CARGO_PKG_VERSION");
    assert!(
        text.contains(expected),
        "--version must report the crate version {}: {}",
        expected,
        text
    );
}
