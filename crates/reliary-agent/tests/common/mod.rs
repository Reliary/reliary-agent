#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static DAEMON_LOCK: Mutex<()> = Mutex::new(());

/// Shared daemon instance per test binary. Tests share one daemon.
fn get_shared_daemon() -> &'static DaemonGuard {
    static DAEMON: OnceLock<DaemonGuard> = OnceLock::new();
    DAEMON.get_or_init(|| start_daemon_inner())
}

pub struct DaemonGuard {
    child: Option<Child>,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        // Don't kill — other test binaries may still be using this daemon.
        // The daemon process will be orphaned and cleaned up by init.
        let _ = self.child.take();
    }
}

fn is_daemon_running() -> bool {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    client.get("http://127.0.0.1:9090/ping").send().is_ok()
}

fn start_daemon_inner() -> DaemonGuard {
    if is_daemon_running() {
        return DaemonGuard { child: None };
    }

    let _lock = DAEMON_LOCK.lock().unwrap();
    if is_daemon_running() {
        return DaemonGuard { child: None };
    }

    let bin = binary_path();
    let mut child = Command::new(&bin)
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start daemon");

    let deadline = Instant::now() + Duration::from_secs(15);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("daemon did not start within 15s");
        }
        if client.get("http://127.0.0.1:9090/ping").send().is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    DaemonGuard { child: Some(child) }
}

pub fn start_daemon() -> &'static DaemonGuard {
    get_shared_daemon()
}

pub fn load_api_key() -> Option<String> {
    let pi_config = dirs::home_dir()
        .map(|h| h.join(".pi/agent/models.json"))?;
    if !pi_config.exists() {
        return None;
    }
    let content = std::fs::read_to_string(pi_config).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let key = json["providers"]["deepinfra"]["apiKey"].as_str()?.to_string();
    if key.is_empty() || key == "YOUR_API_KEY" {
        return None;
    }
    Some(key)
}

pub fn binary_path() -> PathBuf {
    // Relative to workspace root when running `cargo test`
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../target/release/reliary-agent");
    if path.exists() {
        return path;
    }
    // Fallback: debug build
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../target/debug/reliary-agent");
    path
}

pub fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap()
}

pub fn skip_without_live() -> bool {
    std::env::var("RELIARY_E2E_LIVE").is_err()
}

pub struct McpGuard {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
    stdin: std::process::ChildStdin,
}

impl McpGuard {
    pub fn send(&mut self, request: &serde_json::Value) -> serde_json::Value {
        let mut line = serde_json::to_string(request).unwrap();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
        let mut resp = String::new();
        self.reader.read_line(&mut resp).unwrap();
        serde_json::from_str(&resp).unwrap()
    }

    /// Send a raw JSON string without waiting for a response.
    /// Used for notifications where no response is expected.
    pub fn send_raw(&mut self, json: &str) {
        let mut line = json.to_string();
        if !line.ends_with('\n') { line.push('\n'); }
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }
}

impl Drop for McpGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn start_mcp() -> McpGuard {
    let bin = binary_path();
    let mut child = Command::new(&bin)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start MCP");

    let reader = BufReader::new(child.stdout.take().unwrap());
    let stdin = child.stdin.take().unwrap();

    McpGuard { child, reader, stdin }
}
