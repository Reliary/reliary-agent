use std::path::PathBuf;
use std::fs;
use std::net::TcpStream;
use std::time::Duration;
use serde_json::{json, Value};
use std::process::Command;
use std::io::Write;

/// Show a spinner while a closure runs. Clears the line when done.
/// Uses a helper that prints a message, runs the closure, then clears.
/// Avoids threading (rusqlite Connection is !Sync).
pub fn with_spinner<F, T>(msg: &str, f: F) -> T
where
    F: FnOnce() -> T,
{
    eprint!("{} ... ", msg);
    let _ = std::io::stderr().flush();
    let result = f();
    eprint!("\r\x1b[K");
    result
}

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

fn color() -> &'static str { "\x1b[1m\x1b[32m" }
fn reset() -> &'static str { "\x1b[0m" }
fn dim() -> &'static str { "\x1b[2m" }
fn blue() -> &'static str { "\x1b[34m" }
fn red() -> &'static str { "\x1b[31m" }
fn yellow() -> &'static str { "\x1b[33m" }

pub fn daemon_alive() -> bool {
    TcpStream::connect_timeout(&"127.0.0.1:9090".parse().expect("invalid port"), Duration::from_millis(500)).is_ok()
}

fn daemon_pid_path() -> PathBuf {
    std::path::Path::new("/tmp/reliary-agent-9090.pid").to_path_buf()
}

/// Read daemon PID from file, returns (pid, alive) where alive means process exists
pub fn daemon_pid() -> Option<(u32, bool)> {
    let pid_path = daemon_pid_path();
    let pid_str = std::fs::read_to_string(&pid_path).ok()?;
    let pid: u32 = pid_str.trim().parse().ok()?;
    let alive = Command::new("kill").arg("-0").arg(pid.to_string()).status().is_ok_and(|s| s.success());
    Some((pid, alive))
}

/// Write PID file for daemon
pub fn write_pid_file() {
    let pid_path = daemon_pid_path();
    let _ = std::fs::write(&pid_path, format!("{}\n", std::process::id()));
}

/// Remove PID file for daemon
pub fn remove_pid_file() {
    let pid_path = daemon_pid_path();
    let _ = std::fs::remove_file(&pid_path);
}

/// Wait until daemon health check passes, with timeout
pub fn wait_for_daemon(timeout_secs: u64) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < timeout_secs {
        if daemon_alive() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Scan for multiple installations of reliary-agent
pub struct InstallInfo {
    pub path: String,
    pub version: String,
    pub method: &'static str,
    pub active: bool,
}

pub fn find_installs() -> Vec<InstallInfo> {
    let mut installs: Vec<InstallInfo> = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    // Find active binary via PATH
    let which = if cfg!(target_os = "windows") { "where" } else { "which" };
    if let Ok(output) = Command::new(which).arg("-a").arg("reliary-agent").output() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for path in stdout.lines() {
            let p = path.trim();
            if !p.is_empty() && seen_paths.insert(p.to_string()) {
                let version = binary_version(p);
                installs.push(InstallInfo {
                    path: p.to_string(),
                    version,
                    method: "PATH",
                    active: installs.is_empty(),
                });
            }
        }
    }

    // Check cargo bin
    if let Some(home) = home_dir() {
        let cargo_bin = home.join(".cargo/bin/reliary-agent");
        let cargo_path = cargo_bin.to_string_lossy().to_string();
        if cargo_bin.exists() && seen_paths.insert(cargo_path.clone()) {
            let version = binary_version(&cargo_path);
            installs.push(InstallInfo { path: cargo_path, version, method: "cargo", active: false });
        }

        // Check npm global
        let npm_bin = home.join(".local/share/io.npm/.npm-global/bin/reliary-agent");
        let npm_path = npm_bin.to_string_lossy().to_string();
        if npm_bin.exists() && seen_paths.insert(npm_path.clone()) {
            let version = binary_version(&npm_path);
            installs.push(InstallInfo { path: npm_path, version, method: "npm", active: false });
        }
        // Also check npm's common global dir
        let npm_bin2 = home.join("node_modules/.bin/reliary-agent");
        let npm_path2 = npm_bin2.to_string_lossy().to_string();
        if npm_bin2.exists() && seen_paths.insert(npm_path2.clone()) {
            let version = binary_version(&npm_path2);
            installs.push(InstallInfo { path: npm_path2, version, method: "npm", active: false });
        }
    }

    // Check Homebrew paths
    for brew_path in &[
        "/opt/homebrew/bin/reliary-agent",
        "/usr/local/bin/reliary-agent",
        "/home/linuxbrew/.linuxbrew/bin/reliary-agent",
    ] {
        let p = std::path::Path::new(brew_path);
        if p.exists() && seen_paths.insert(brew_path.to_string()) {
            let version = binary_version(brew_path);
            installs.push(InstallInfo { path: brew_path.to_string(), version, method: "brew", active: false });
        }
    }

    installs
}

fn binary_version(path: &str) -> String {
    let output = Command::new(path).arg("--version").output();
    match output {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { "?".to_string() } else { s }
        }
        _ => "?".to_string(),
    }
}

/// Print install table to display alongside doctor
pub fn print_install_table(installs: &[InstallInfo]) {
    if installs.is_empty() { return; }
    println!("  {}", "─".repeat(50));
    for inst in installs {
        let marker = if inst.active { format!("{}→{}", blue(), reset()) } else { " ".to_string() };
        println!("  {} {} {}v{}  {}", marker, dim(), dim(), inst.version, inst.path);
        if inst.active {
            println!("    {}Active (from PATH){}", dim(), reset());
        }
    }
    println!("  {}", "─".repeat(50));
}

fn has_upstream() -> bool {
    if let Some(home) = home_dir() {
        let routes_file = home.join(".reliary/proxy-routes.json");
        if routes_file.exists() {
            if let Ok(content) = fs::read_to_string(&routes_file) {
                if content.trim().len() > 4 {
                    return true;
                }
            }
        }
    }
    if let Ok(upstream) = std::env::var("RELIARY_UPSTREAM_URL") {
        if !upstream.is_empty() {
            return true;
        }
    }
    false
}

fn proxy_routes_count() -> usize {
    if let Some(home) = home_dir() {
        let routes_file = home.join(".reliary/proxy-routes.json");
        if routes_file.exists() {
            if let Ok(content) = fs::read_to_string(&routes_file) {
                if let Ok(map) = serde_json::from_str::<serde_json::Map<String, Value>>(&content) {
                    return map.len();
                }
            }
        }
    }
    0
}

struct DoctorCheck {
    name: &'static str,
    ok: bool,
    detail: String,
    fixable: bool,
}

fn doctor_checks() -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let daemon_ok = daemon_alive();
    checks.push(DoctorCheck { name: "daemon", ok: daemon_ok, detail: if daemon_ok { "Active on :9090".into() } else { "Stopped".into() }, fixable: true });

    let upstream_ok = has_upstream();
    let upstream_detail = if upstream_ok {
        let count = proxy_routes_count();
        if count > 0 { format!("{} routes", count) } else { "RELIARY_UPSTREAM_URL set".into() }
    } else { "No upstream configured".into() };
    checks.push(DoctorCheck { name: "upstream", ok: upstream_ok, detail: upstream_detail, fixable: false });

    let pi_gate = home_dir().map(|h| h.join(".local/share/reliary/gate.js")).unwrap_or_default();
    checks.push(DoctorCheck { name: "pi", ok: pi_gate.exists(), detail: if pi_gate.exists() { "gate.js installed".into() } else { "not found (optional)".into() }, fixable: false });

    let claude_cfg = home_dir().map(|h| h.join(".claude.json")).unwrap_or_default();
    let claude_ok = has_mcp_server(&claude_cfg, "reliary");
    checks.push(DoctorCheck { name: "claude", ok: claude_ok, detail: if claude_ok { "Wired".into() } else { "Not wired".into() }, fixable: false });

    let opencode_cfg = if cfg!(target_os = "windows") {
        dirs::config_dir().map(|d| d.join("opencode").join("opencode.json"))
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library/Application Support/opencode/opencode.json"))
    } else {
        home_dir().map(|h| h.join(".config/opencode/opencode.json"))
    }.unwrap_or_default();
    let opencode_ok = has_mcp_server(&opencode_cfg, "reliary");
    checks.push(DoctorCheck { name: "opencode", ok: opencode_ok, detail: if opencode_ok { "Wired".into() } else { "Not wired".into() }, fixable: false });

    let index_path = PathBuf::from(".reliary/index.sqlite");
    checks.push(DoctorCheck { name: "index", ok: index_path.exists(), detail: if index_path.exists() { "Index exists".into() } else { "No index found".into() }, fixable: true });

    let mode = crate::config::resolve_mode(Some("."));
    checks.push(DoctorCheck { name: "mode", ok: true, detail: mode.as_str().into(), fixable: false });

    // Multi-install check
    let installs = find_installs();
    if installs.len() > 1 {
        let active_version = installs.iter().find(|i| i.active).map(|i| i.version.clone()).unwrap_or_default();
        let stale_count = installs.iter().filter(|i| !i.active && i.version != active_version).count();
        if stale_count > 0 {
            checks.push(DoctorCheck {
                name: "installs",
                ok: false,
                detail: format!("{} installations, {} stale", installs.len(), stale_count),
                fixable: false,
            });
        }
    }
    let installs_count = installs.len();
    if installs_count > 2 {
        checks.push(DoctorCheck {
            name: "installs",
            ok: false,
            detail: format!("{} active copies — clutter", installs_count),
            fixable: false,
        });
    }

    checks
}

fn doctor_json(checks: &[DoctorCheck]) -> Value {
    let all_good = checks.iter().all(|c| c.ok);
    json!({
        "ready": all_good,
        "checks": checks.iter().map(|c| json!({
            "name": c.name,
            "ok": c.ok,
            "detail": c.detail,
        })).collect::<Vec<_>>(),
    })
}

pub fn doctor(fix: bool, format: &str) {
    let checks = doctor_checks();
    let installs = find_installs();

    if format == "json" {
        let mut j = doctor_json(&checks);
        let install_json: Vec<Value> = installs.iter().map(|i| json!({
            "path": i.path,
            "version": i.version,
            "method": i.method,
            "active": i.active,
        })).collect();
        j.as_object_mut().unwrap().insert("installations".into(), Value::Array(install_json));
        println!("{}", serde_json::to_string_pretty(&j).unwrap_or_else(|_| r#"{"ready":false,"checks":[]}"#.to_string()));
        return;
    }

    println!("\n{}| Reliary Doctor |{}\n", color(), reset());

    let mut needs_daemon = false;
    let mut needs_index = false;

    for c in &checks {
        let icon = if c.ok { format!("{}✓{}", color(), reset()) } else { format!("{}✗{}", red(), reset()) };
        println!("  {} {} {}{}", icon, c.name, dim(), c.detail);
        if !c.ok && c.fixable {
            match c.name {
                "daemon" => needs_daemon = true,
                "index" => needs_index = true,
                _ => {}
            }
        }
    }

    if fix && (needs_daemon || needs_index) {
        println!();
        if needs_daemon {
            print!("  {} Starting daemon... ", dim());
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("reliary-agent"));
            let mut cmd = Command::new(exe);
            cmd.arg("serve");
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
            match cmd.spawn() {
                Ok(_child) => {
                    std::thread::sleep(Duration::from_secs(1));
                    if daemon_alive() { println!("{}started!", color()); } else { println!("{}may need manual start", yellow()); }
                }
                Err(e) => println!("{}failed: {}", red(), e),
            }
        }
        if needs_index {
            print!("  {} Building index... ", dim());
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("reliary-agent"));
            let status = Command::new(exe).arg("index").arg(".").stdout(std::process::Stdio::inherit()).stderr(std::process::Stdio::inherit()).status();
            if status.is_ok_and(|s| s.success()) { println!("{}done", color()); } else { println!("{}failed", red()); }
        }
    }

    if installs.len() > 1 {
        println!();
        println!("  {}| Installations |{}", color(), reset());
        print_install_table(&installs);
    }

    let all_good = checks.iter().all(|c| c.ok);
    if all_good {
        println!("\n{}✓{} System ready.", color(), reset());
    } else if !fix {
        println!("\n  {}Tip: run 'reliary-agent doctor --fix' to fix issues automatically.{}", dim(), reset());
    } else {
        println!("\n{}⚠{} Some checks failed.", yellow(), reset());
    }
}

struct StatusData {
    proxy_running: bool,
    mode: String,
    routes: usize,
    index_files: i64,
    chronicle_events: i64,
    index_exists: bool,
}

fn status_data() -> StatusData {
    let index_path = PathBuf::from(".reliary/index.sqlite");
    let mut index_files = 0i64;
    let mut chronicle_events = 0i64;
    let index_exists = index_path.exists();

    if index_exists {
        if let Ok(db) = rusqlite::Connection::open(&index_path) {
            let _ = db.execute_batch("PRAGMA synchronous=NORMAL;");
            if let Ok(mut stmt) = db.prepare("SELECT COUNT(DISTINCT file_id) FROM file_phrases") {
                if let Ok(mut rows) = stmt.query([]) {
                    if let Ok(Some(row)) = rows.next() { index_files = row.get(0).unwrap_or(0); }
                }
            }
            if let Ok(mut stmt) = db.prepare("SELECT COUNT(*) FROM chronicle") {
                if let Ok(mut rows) = stmt.query([]) {
                    if let Ok(Some(row)) = rows.next() { chronicle_events = row.get(0).unwrap_or(0); }
                }
            }
        }
    }

    StatusData {
        proxy_running: daemon_alive(),
        mode: crate::config::resolve_mode(Some(".")).as_str().to_string(),
        routes: proxy_routes_count(),
        index_files,
        chronicle_events,
        index_exists,
    }
}

fn status_json(d: &StatusData) -> Value {
    json!({
        "proxy": { "running": d.proxy_running, "port": 9090 },
        "mode": d.mode,
        "routes": d.routes,
        "index": { "exists": d.index_exists, "files": d.index_files },
        "chronicle": { "events": d.chronicle_events },
    })
}

pub fn status(format: &str) {
    let d = status_data();

    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&status_json(&d)).unwrap_or_else(|_| r#"{"proxy":{"running":false}}"#.to_string()));
        return;
    }

    println!("\n{}| Reliary Agent Status |{}\n", color(), reset());

    print!("{}•{} Proxy: ", blue(), reset());
    if d.proxy_running {
        let pid_info = match daemon_pid() {
            Some((pid, true)) => format!("PID {}", pid),
            _ => "".to_string(),
        };
        let extra = if pid_info.is_empty() { "".to_string() } else { format!(" ({})", pid_info) };
        println!("{}✓{} Running on :9090{}", color(), reset(), extra);
    } else {
        println!("{}✗{} Stopped", red(), reset());
        if cfg!(unix) { println!("  {}→ Run 'reliary-agent start' to run in background{}", dim(), reset()); }
    }

    println!("  {}•{} Mode: {}", blue(), reset(), d.mode);

    print!("  {}•{} Routes: ", blue(), reset());
    if d.routes > 0 {
        println!("{} routes in proxy-routes.json", d.routes);
    } else if std::env::var("RELIARY_UPSTREAM_URL").ok().is_some() {
        println!("RELIARY_UPSTREAM_URL set");
    } else {
        println!("{}-{} None (proxy won't route)", yellow(), reset());
        println!("    {}→ Set RELIARY_UPSTREAM_URL or run 'init'{}", dim(), reset());
    }

    if d.index_exists {
        println!("  {}•{} Index: {} files indexed", blue(), reset(), d.index_files);
        println!("  {}•{} Memory: {} chronicle events", blue(), reset(), d.chronicle_events);
    } else {
        println!("  {}•{} Index: {}-{} No index found", blue(), reset(), yellow(), reset());
        println!("    {}→ Run 'reliary-agent index .' to build it{}", dim(), reset());
    }
}

pub fn clean(global: bool, all: bool) {
    let do_global = global || all;
    let do_local = !global || all;

    if do_local {
        let local_dir = PathBuf::from(".reliary");
        if local_dir.exists() {
            if fs::remove_dir_all(&local_dir).is_ok() {
                println!("{}✓{} Cleaned project state (.reliary)", color(), reset());
            } else {
                println!("{}✗{} Failed to clean project state", red(), reset());
            }
        } else {
            println!("{}-{} No project state found", yellow(), reset());
        }
    }

    if do_global {
        if let Some(home) = home_dir() {
            let global_dir = home.join(".reliary");
            if global_dir.exists() {
                if fs::remove_dir_all(&global_dir).is_ok() {
                    println!("{}✓{} Cleaned global state (~/.reliary)", color(), reset());
                } else {
                    println!("{}✗{} Failed to clean global state", red(), reset());
                }
            } else {
                println!("{}-{} No global state found", yellow(), reset());
            }
        }
    }
}

pub fn logs(tail: bool, level: Option<String>) {
    if let Ok(log_path_str) = std::env::var("RELIARY_LOG_FILE") {
        let log_path = std::path::Path::new(&log_path_str);
        if log_path.exists() {
            if tail {
                println!("{} Tailing {}...{}", blue(), log_path_str, reset());
                let status = Command::new("tail").arg("-f").arg(&log_path_str).status();
                if status.is_err() {
                    if let Ok(content) = std::fs::read_to_string(log_path) { println!("{}", content); }
                }
            } else if let Some(lvl) = level {
                let lower_lvl = lvl.to_lowercase();
                if let Ok(content) = std::fs::read_to_string(log_path) {
                    for line in content.lines() {
                        let upper = format!(" [{}] ", lvl.to_uppercase());
                        let lower = format!("[{}]", lower_lvl);
                        if line.contains(&upper) || line.contains(&lower) { println!("{}", line); }
                    }
                }
            } else {
                if let Ok(content) = std::fs::read_to_string(log_path) { println!("{}", content); }
            }
            return;
        } else {
            eprintln!("{} Log file not found: {}{}", yellow(), log_path_str, reset());
            return;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut cmd = Command::new("journalctl");
        cmd.args(["--user", "-u", "reliary-daemon.service", "--no-pager"]);
        if let Some(lvl) = level { cmd.arg(format!("-p{}", lvl.to_uppercase())); }
        if tail { cmd.arg("-f"); }
        let status = cmd.status();
        if status.is_err() || status.map_or(true, |s| !s.success()) {
            eprintln!("{} Could not read daemon logs.{}", yellow(), reset());
            eprintln!("  {} Is the daemon running? Run 'reliary-agent doctor' to check.{}", dim(), reset());
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            let log_path = home.join("Library/Logs/com.reliary.daemon.log");
            if log_path.exists() {
                let path_str = log_path.to_string_lossy().to_string();
                if tail {
                    println!("{} Tailing {}...{}", blue(), path_str, reset());
                    let status = Command::new("tail").arg("-f").arg(&path_str).status();
                    if status.is_err() { if let Ok(content) = std::fs::read_to_string(&path_str) { println!("{}", content); } }
                } else if let Some(lvl) = level {
                    let lvl_upper = lvl.to_uppercase();
                    if let Ok(content) = std::fs::read_to_string(&path_str) {
                        for line in content.lines() {
                            if line.contains(&format!("[{}]", lvl_upper)) || line.contains(&format!("[{}]", lvl.to_lowercase())) { println!("{}", line); }
                        }
                    }
                } else {
                    if let Ok(content) = std::fs::read_to_string(&path_str) { println!("{}", content); }
                }
                return;
            }
        }
        eprintln!("{} No daemon log file found.{}", yellow(), reset());
        return;
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(home) = home_dir() {
            let log_dir = home.join(".reliary");
            if log_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&log_dir) {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        let name_str = name.to_string_lossy();
                        if name_str.ends_with(".log") {
                            let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
                            println!("{} {}:{}", dim(), name_str, reset());
                            println!("{}", if tail { &content[content.len().saturating_sub(500)..] } else { &content });
                            return;
                        }
                    }
                }
            }
        }
        eprintln!("{} No daemon log file found.{}", yellow(), reset());
        return;
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        eprintln!("{} 'logs' is not supported on this platform.{}", yellow(), reset());
    }
}

pub fn format_risk(path: &str, risk: &str, format: &str) {
    if format == "json" {
        let risk_lower = risk.to_lowercase();
        let (level, reason) = if risk_lower.contains("high") {
            ("high", "High blast radius — many callers or critical path")
        } else if risk_lower.contains("medium") {
            ("medium", "Moderate blast radius — some callers affected")
        } else {
            ("low", "Low risk: small file or few callers")
        };
        println!("{}", json!({
            "file": path,
            "risk": level,
            "reason": reason,
        }));
    } else {
        let risk_lower = risk.to_lowercase();
        let icon = if risk_lower.contains("high") {
            format!("{}⚠{}", red(), reset())
        } else if risk_lower.contains("medium") {
            format!("{}⚡{}", yellow(), reset())
        } else {
            format!("{}✓{}", color(), reset())
        };
        println!("  {} {} {}", icon, path, &risk[..60.min(risk.len())]);
    }
}

pub fn format_dead(path: &str, entries: &[String], format: &str) {
    if format == "json" {
        println!("{}", json!({
            "path": path,
            "candidates": entries,
        }));
    } else {
        println!("\n{}| Dead Code: {} |{}\n", color(), path, reset());
        if entries.is_empty() {
            println!("  {}No dead code candidates found.{}", dim(), reset());
        } else {
            for entry in entries {
                println!("  {}•{} {}", yellow(), reset(), entry);
            }
            println!("\n  {}Found {} candidate(s){}", dim(), entries.len(), reset());
        }
    }
}

fn has_mcp_server(cfg_path: &PathBuf, server_name: &str) -> bool {
    if let Ok(content) = fs::read_to_string(cfg_path) {
        if let Ok(v) = serde_json::from_str::<Value>(&content) {
            if let Some(servers) = v.get("mcpServers").and_then(|m| m.as_object()) {
                return servers.contains_key(server_name);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_with_spinner_runs_closure() {
        let result = with_spinner("testing", || 42);
        assert_eq!(result, 42);
    }

    #[test]
    fn test_with_spinner_no_side_effects() {
        let x = with_spinner("testing", || "hello world".to_string());
        assert_eq!(x, "hello world");
    }
}
