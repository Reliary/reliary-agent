//! Shared harness for end-to-end tests.
//!
//! Spawns the real `reliary` binary as a subprocess and speaks line-delimited
//! JSON-RPC (MCP stdio transport) to it, exactly like Claude Code / OpenCode /
//! Pi do. No mocking: these tests exercise the shipped binary.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// Absolute path to the built `reliary` binary.
pub fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_reliary"))
}

/// A minimal indexed project: a git repo with a couple of source files so
/// auto-trust fires and queries return deterministic results.
pub struct Fixture {
    pub dir: tempfile::TempDir,
}

impl Fixture {
    /// Create a git repo with `src/lib.rs` (defines `alpha`, calls `beta`)
    /// and `src/other.rs` (defines `beta`).
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            r#"//! Fixture crate.
/// Alpha does a thing.
pub fn alpha() -> i32 {
    beta() + 1
}
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/other.rs"),
            r#"/// Beta does another thing.
pub fn beta() -> i32 {
    41
}
"#,
        )
        .unwrap();

        // git repo marker (auto-trust looks for .git/). git is present on
        // GitHub runners and developer machines; skip cleanly if it is not.
        let ok = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            ok,
            "git init failed — the e2e tests require git on PATH (install git, or skip with --skip e2e_)"
        );

        Fixture { dir }
    }

    /// Build the index without starting an MCP server.
    pub fn trust(&self) {
        let out = Command::new(binary_path())
            .args(["trust", "."])
            .current_dir(self.path())
            .env("NO_RELIARY_WATCHER", "1")
            .output()
            .expect("run trust");
        assert!(
            out.status.success(),
            "trust failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            self.path().join(".reliary/index.sqlite").exists(),
            "trust did not create an index"
        );
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Run the binary as a subprocess with a controlled environment.
pub fn run_cli(args: &[&str], cwd: &Path, envs: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(binary_path());
    cmd.args(args).current_dir(cwd);
    cmd.env("NO_RELIARY_WATCHER", "1");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn reliary")
}

// ── MCP stdio client ──────────────────────────────────────────────────────

/// A live `reliary mcp` subprocess speaking JSON-RPC over stdio.
pub struct Mcp {
    child: Child,
    stdin: ChildStdin,
    /// Responses arrive on a channel from a reader thread, so tests can time
    /// out instead of hanging if the server stops responding.
    responses: Receiver<serde_json::Value>,
    _stdout_reader: std::thread::JoinHandle<()>,
    next_id: i64,
}

impl Mcp {
    /// Spawn `reliary mcp` inside `cwd`.
    ///
    /// `RELIARY_RESULT_CACHE=0` keeps every identical call a real call;
    /// `RELIARY_NO_TRUNCATE=1` keeps assertions on full output stable.
    pub fn start(cwd: &Path) -> Mcp {
        let mut child = Command::new(binary_path())
            .arg("mcp")
            .current_dir(cwd)
            .env("NO_RELIARY_WATCHER", "1")
            .env("RELIARY_RESULT_CACHE", "0")
            .env("RELIARY_NO_TRUNCATE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn reliary mcp");

        let stdin = child.stdin.take().expect("child stdin");
        let stdout: ChildStdout = child.stdout.take().expect("child stdout");
        let (tx, rx) = mpsc::channel();

        let reader = std::thread::spawn(move || {
            let mut lines = BufReader::new(stdout).lines();
            while let Some(Ok(line)) = lines.next() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(line) {
                    Ok(v) => {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                    // Non-JSON line on stdout is a protocol violation, but the
                    // reader must not die — surface it as a sentinel so the
                    // test can assert rather than hang.
                    Err(_) => {
                        let _ = tx.send(serde_json::json!({ "__non_json__": line }));
                    }
                }
            }
        });

        Mcp { child, stdin, responses: rx, _stdout_reader: reader, next_id: 1 }
    }

    /// Send one JSON-RPC request and wait for its response.
    pub fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send_raw(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        self.read_response(Duration::from_secs(30))
    }

    /// Send raw bytes on stdin (for malformed-input tests).
    pub fn send_raw(&mut self, msg: &serde_json::Value) {
        let line = serde_json::to_string(msg).unwrap();
        self.send_line(&line);
    }

    /// Send an arbitrary line (no JSON validation).
    pub fn send_line(&mut self, line: &str) {
        writeln!(self.stdin, "{}", line).expect("write stdin");
        self.stdin.flush().expect("flush stdin");
    }

    /// Read the next message from stdout, or panic on timeout.
    pub fn read_response(&mut self, timeout: Duration) -> serde_json::Value {
        self.responses
            .recv_timeout(timeout)
            .unwrap_or_else(|_| panic!("timed out after {:?} waiting for MCP response", timeout))
    }

    /// Try to read a message, returning None on timeout (used by
    /// "server must stay silent" assertions).
    pub fn try_read_response(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        self.responses.recv_timeout(timeout).ok()
    }

    pub fn initialize(&mut self) -> serde_json::Value {
        self.request(
            "initialize",
            serde_json::json!({ "protocolVersion": "2024-11-05" }),
        )
    }

    pub fn list_tools(&mut self) -> Vec<serde_json::Value> {
        let resp = self.request("tools/list", serde_json::json!({}));
        resp["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools/list did not return an array: {}", resp))
            .clone()
    }

    /// Call a tool. Returns the raw JSON-RPC response.
    pub fn call_tool(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        self.request(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": args }),
        )
    }

    /// Call a tool and return its concatenated text content.
    pub fn call_tool_text(&mut self, name: &str, args: serde_json::Value) -> String {
        let resp = self.call_tool(name, args);
        assert!(
            resp.get("error").is_none(),
            "tool {} returned an error: {}",
            name,
            resp
        );
        resp["result"]["content"]
            .as_array()
            .unwrap_or_else(|| panic!("tool {} returned no content array: {}", name, resp))
            .iter()
            .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Is the child still running?
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
