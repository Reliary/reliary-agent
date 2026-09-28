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
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    let oc = home.path().join(".config/opencode");
    std::fs::create_dir_all(&oc).unwrap();
    std::fs::write(oc.join("opencode.json"), r#"{"mcpServers":{}}"#).unwrap();
    home
}

fn init_with(home: &Path, answers: &str) -> std::process::Output {
    init_args(home, &["init"], answers)
}

/// Run `init` (or `init --dry-run`) against a fake HOME with scripted answers.
fn init_args(home: &Path, args: &[&str], answers: &str) -> std::process::Output {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(binary_path())
        .args(args)
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

// ── integration conformance (init ↔ settings ↔ doctor ↔ uninstall) ────────
//
// The install path used to write hook files and register only one of them,
// and uninstall removed files without removing their registrations — leaving
// a command that fires a failing exec on every tool call. These tests pin the
// whole loop: every registered command must resolve to a file on disk, and
// uninstall must remove exactly what install added.

/// Read `~/.claude/settings.json` from the fake HOME.
fn read_settings(home: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(home.join(".claude/settings.json"))
        .expect("settings.json must exist");
    serde_json::from_str(&raw).expect("settings.json must stay valid JSON")
}

/// Every hook command in settings.json, paired with its event.
fn hook_commands(settings: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) {
        for (event, entries) in hooks {
            let Some(arr) = entries.as_array() else { continue };
            for entry in arr {
                let Some(hs) = entry.get("hooks").and_then(|h| h.as_array()) else { continue };
                for h in hs {
                    if let Some(cmd) = h.get("command").and_then(|c| c.as_str()) {
                        out.push((event.clone(), cmd.to_string()));
                    }
                }
            }
        }
    }
    out
}

#[test]
fn e2e_cli_init_registers_resolvable_hooks() {
    let home = fake_home();
    let out = init_with(home.path(), "Y\nY\nY\nN\n");
    let text = plain(&format!("{}{}", stdout_of(&out), stderr_of(&out)));
    assert!(!text.contains("panicked"), "init must not panic: {}", text);

    // Both hook files must exist and be executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["reliary-session-reminder", "reliary-sift-pretooluse"] {
            let p = home.path().join(".claude/hooks").join(name);
            assert!(p.exists(), "hook file must be written: {}", p.display());
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "{} must be executable", p.display());
        }
    }

    // Every registered hook command must resolve to an existing file. A
    // registration without its file fires a failing exec on every call.
    let settings = read_settings(home.path());
    let commands = hook_commands(&settings);
    assert!(
        !commands.is_empty(),
        "init must register at least one hook: {}",
        settings
    );
    for (event, cmd) in &commands {
        let prog = cmd.split_whitespace().next().unwrap_or("");
        let path = if let Some(rest) = prog.strip_prefix("~/") {
            home.path().join(rest)
        } else {
            std::path::PathBuf::from(prog)
        };
        assert!(
            path.exists(),
            "registered {} hook command does not exist on disk: {} (resolved {})",
            event,
            cmd,
            path.display()
        );
    }
}

#[test]
fn e2e_cli_doctor_reports_wired_integrations() {
    let home = fake_home();
    init_with(home.path(), "Y\nY\nY\nN\n");

    let out = run_cli(
        &["doctor", "--format", "json"],
        home.path(),
        &[("HOME", home.path().to_str().unwrap())],
    );
    let text = stdout_of(&out);
    let v: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("doctor --format json must emit JSON ({}): {}", e, text));
    let checks = v["checks"].as_array().expect("checks array");
    for name in ["claude", "opencode"] {
        let c = checks
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("doctor must report a {} check: {}", name, v));
        assert_eq!(
            c["ok"], true,
            "doctor must report {} wired after init: {}",
            name, c
        );
    }
}

#[test]
fn e2e_cli_init_dry_run_mutates_nothing() {
    let home = fake_home();
    // Seed a settings file with unrelated content that must survive.
    let settings_path = home.path().join(".claude/settings.json");
    std::fs::write(&settings_path, r#"{"env":{"KEEP":"1"}}"#).unwrap();
    let before_settings = std::fs::read(&settings_path).unwrap();
    let before_claude = std::fs::read(home.path().join(".claude.json")).unwrap();

    let out = init_args(home.path(), &["init", "--dry-run"], "\n\n\n\n\n\n");
    assert!(out.status.success(), "dry-run init must exit 0");

    assert_eq!(
        std::fs::read(&settings_path).unwrap(),
        before_settings,
        "dry-run must not modify settings.json"
    );
    assert_eq!(
        std::fs::read(home.path().join(".claude.json")).unwrap(),
        before_claude,
        "dry-run must not modify .claude.json"
    );
    assert!(
        !home.path().join(".claude/hooks").exists(),
        "dry-run must not create hook files"
    );
}

#[test]
fn e2e_cli_reinit_does_not_duplicate_hooks() {
    let home = fake_home();
    init_with(home.path(), "Y\nY\nY\nN\n");
    let first = hook_commands(&read_settings(home.path()));
    init_with(home.path(), "Y\nY\nY\nN\n");
    let second = hook_commands(&read_settings(home.path()));
    assert_eq!(
        first.len(),
        second.len(),
        "a second init must not duplicate hook entries: {:?} -> {:?}",
        first,
        second
    );
}

#[test]
fn e2e_cli_uninstall_removes_hooks_and_registrations() {
    let home = fake_home();
    // Seed an unrelated hook that must survive uninstall.
    let settings_path = home.path().join(".claude/settings.json");
    std::fs::write(
        &settings_path,
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/bin/true"}]}]}}"#,
    )
    .unwrap();

    init_with(home.path(), "Y\nY\nY\nN\n");
    assert!(
        !hook_commands(&read_settings(home.path())).is_empty(),
        "precondition: init must register hooks"
    );

    let out = run_cli(
        &["uninstall"],
        home.path(),
        &[("HOME", home.path().to_str().unwrap())],
    );
    let text = plain(&format!("{}{}", stdout_of(&out), stderr_of(&out)));
    assert!(!text.contains("panicked"), "uninstall must not panic: {}", text);

    // No reliary hook command may remain registered.
    let remaining = hook_commands(&read_settings(home.path()));
    let reliary: Vec<_> = remaining
        .iter()
        .filter(|(_, c)| c.contains("reliary"))
        .collect();
    assert!(
        reliary.is_empty(),
        "uninstall must strip every reliary hook registration: {:?}",
        reliary
    );
    // The unrelated hook must survive.
    assert!(
        remaining.iter().any(|(_, c)| c == "/bin/true"),
        "uninstall must not touch unrelated hooks: {:?}",
        remaining
    );
    // Hook files must be gone.
    assert!(
        !home.path().join(".claude/hooks/reliary-session-reminder").exists(),
        "uninstall must remove the reminder hook file"
    );
    assert!(
        !home.path().join(".claude/hooks/reliary-sift-pretooluse").exists(),
        "uninstall must remove the sift hook file"
    );
}

#[test]
fn e2e_cli_doctor_flags_unusable_index() {
    // A `.reliary/index.sqlite` that exists but is not a database used to be
    // reported as a green tick. It must now be a failure that names the file.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".reliary")).unwrap();
    std::fs::write(dir.path().join(".reliary/index.sqlite"), b"not a database").unwrap();

    let out = run_cli(&["doctor", "--format", "json"], dir.path(), &[]);
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    let index = v["checks"].as_array().unwrap().iter()
        .find(|c| c["name"] == "index").expect("index check");
    assert_eq!(
        index["ok"], false,
        "doctor must not report an unusable index as ok: {}", index
    );
    assert!(
        index["detail"].as_str().unwrap_or("").contains("not a usable index"),
        "doctor must explain the index is unusable: {}", index
    );
    assert_eq!(v["ready"], false, "an unusable index means not ready: {}", v);

    // And an absent index is still a failure with the rebuild hint.
    let dir2 = tempfile::tempdir().unwrap();
    let out = run_cli(&["doctor", "--format", "json"], dir2.path(), &[]);
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    let index = v["checks"].as_array().unwrap().iter()
        .find(|c| c["name"] == "index").expect("index check");
    assert_eq!(index["ok"], false, "no index must not be ok: {}", index);
    assert!(
        index["detail"].as_str().unwrap_or("").contains("reliary trust"),
        "doctor must suggest how to build the index: {}", index
    );
}

#[test]
fn e2e_cli_doctor_flags_dangling_hook_registration() {
    // A settings.json that registers a reliary hook whose file is missing is
    // a broken install: doctor must report claude as not-ok and name it.
    let home = fake_home();
    std::fs::write(home.path().join(".claude.json"),
        r#"{"mcpServers":{"reliary":{"command":"/bin/true","args":["mcp"]}}}"#).unwrap();
    std::fs::create_dir_all(home.path().join(".claude/hooks")).unwrap();
    std::fs::write(home.path().join(".claude/settings.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"~/.claude/hooks/reliary-sift-pretooluse"}]}]}}"#).unwrap();

    let out = run_cli(
        &["doctor", "--format", "json"],
        home.path(),
        &[("HOME", home.path().to_str().unwrap())],
    );
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    let claude = v["checks"].as_array().unwrap().iter()
        .find(|c| c["name"] == "claude").expect("claude check");
    assert_eq!(
        claude["ok"], false,
        "doctor must flag a registered hook with no file on disk: {}", claude
    );
    assert!(
        claude["detail"].as_str().unwrap_or("").contains("reliary-sift-pretooluse"),
        "doctor must name the dangling hook: {}", claude
    );

    // Creating the file must clear the failure.
    std::fs::write(home.path().join(".claude/hooks/reliary-sift-pretooluse"), "#!/bin/sh\n").unwrap();
    std::fs::write(home.path().join(".claude/hooks/reliary-session-reminder"), "#!/bin/sh\n").unwrap();
    let out = run_cli(
        &["doctor", "--format", "json"],
        home.path(),
        &[("HOME", home.path().to_str().unwrap())],
    );
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    let claude = v["checks"].as_array().unwrap().iter()
        .find(|c| c["name"] == "claude").expect("claude check");
    assert_eq!(
        claude["ok"], true,
        "doctor must clear the flag once the hook file exists: {}", claude
    );
}
