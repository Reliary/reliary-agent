use std::path::PathBuf;
use std::fs;
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

/// Names the binary can be installed under: the shipped name (`reliary`) and
/// the legacy crate/bin name (`reliary-agent`) that older releases used.
const BINARY_NAMES: [&str; 2] = ["reliary", "reliary-agent"];

/// Scan for multiple installations of the reliary binary.
pub struct InstallInfo {
    pub path: String,
    pub version: String,
    pub method: &'static str,
    pub active: bool,
}

pub fn find_installs() -> Vec<InstallInfo> {
    let mut installs: Vec<InstallInfo> = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    // Find active binary via PATH (either name).
    let which = if cfg!(target_os = "windows") { "where" } else { "which" };
    for bin_name in BINARY_NAMES {
        if let Ok(output) = Command::new(which).arg("-a").arg(bin_name).output() {
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
    }

    // Check cargo bin
    if let Some(home) = home_dir() {
        for bin_name in BINARY_NAMES {
            let cargo_bin = home.join(".cargo/bin").join(bin_name);
            let cargo_path = cargo_bin.to_string_lossy().to_string();
            if cargo_bin.exists() && seen_paths.insert(cargo_path.clone()) {
                let version = binary_version(&cargo_path);
                installs.push(InstallInfo { path: cargo_path, version, method: "cargo", active: false });
            }

            // Check npm global
            let npm_bin = home.join(".local/share/io.npm/.npm-global/bin").join(bin_name);
            let npm_path = npm_bin.to_string_lossy().to_string();
            if npm_bin.exists() && seen_paths.insert(npm_path.clone()) {
                let version = binary_version(&npm_path);
                installs.push(InstallInfo { path: npm_path, version, method: "npm", active: false });
            }
            // Also check npm's common global dir
            let npm_bin2 = home.join("node_modules/.bin").join(bin_name);
            let npm_path2 = npm_bin2.to_string_lossy().to_string();
            if npm_bin2.exists() && seen_paths.insert(npm_path2.clone()) {
                let version = binary_version(&npm_path2);
                installs.push(InstallInfo { path: npm_path2, version, method: "npm", active: false });
            }
        }
    }

    // Check Homebrew paths
    for dir in &[
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/home/linuxbrew/.linuxbrew/bin",
    ] {
        for bin_name in BINARY_NAMES {
            let brew_path = format!("{}/{}", dir, bin_name);
            let p = std::path::Path::new(&brew_path);
            if p.exists() && seen_paths.insert(brew_path.clone()) {
                let version = binary_version(&brew_path);
                installs.push(InstallInfo { path: brew_path, version, method: "brew", active: false });
            }
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

struct DoctorCheck {
    name: &'static str,
    ok: bool,
    detail: String,
    fixable: bool,
    optional: bool,
}

fn doctor_checks(installs: &[InstallInfo]) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    // --- Binary reachability (critical: MCP server needs this) ---
    let exe = std::env::current_exe().ok(); // GUARDED: intentional — None falls back to "reliary"
    let exe_name = exe.as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("reliary");
    checks.push(DoctorCheck {
        name: "binary",
        ok: exe.is_some(),
        detail: match &exe {
            Some(p) => format!("{} at {}", exe_name, p.display()),
            None => "could not resolve current exe".into(),
        },
        fixable: false,
        optional: false,
    });

    // --- Index health (the core capability) ---
    let index_path = PathBuf::from(".reliary/index.sqlite");
    let index_exists = index_path.exists();
    let index_detail = if index_exists {
        // Probe the index for freshness
        let probe: Option<(i64, i64, f64)> = (|| -> Option<(i64, i64, f64)> {
            let db = rusqlite::Connection::open(&index_path).ok()?;
            let file_count: i64 = db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0))
                .inspect_err(|e| eprintln!("[doctor] file count query failed: {}", e)).ok()?;
            let phrase_count: i64 = db.query_row("SELECT COUNT(*) FROM phrases", [], |r| r.get(0))
                .inspect_err(|e| eprintln!("[doctor] phrase count query failed: {}", e)).ok()?;
            let age_days: f64 = db.query_row(
                "SELECT (julianday('now') - julianday(MAX(mtime), 'unixepoch')) FROM file_map",
                [], |r| r.get(0)
            ).unwrap_or(0.0);
            Some((file_count, phrase_count, age_days))
        })();
        match probe {
            Some((files, phrases, age)) => {
                if age > 7.0 {
                    format!("{} files, {} phrases, {:.0}d old (stale — run `reliary trust .`)", files, phrases, age)
                } else {
                    format!("{} files, {} phrases, {:.0}d old", files, phrases, age)
                }
            }
            None => "cannot probe index".into(),
        }
    } else {
        "no index found — run `reliary trust .`".into()
    };
    checks.push(DoctorCheck {
        name: "index",
        ok: index_exists,
        detail: index_detail,
        fixable: true,
        optional: false,
    });

    // --- Mode (configuration check) ---
    let mode = crate::config::resolve_mode(Some("."));
    checks.push(DoctorCheck {
        name: "mode",
        ok: true,
        detail: mode.as_str().into(),
        fixable: false,
        optional: false,
    });

    // --- Claude Code integration (optional) ---
    let claude_cfg = home_dir().map(|h| h.join(".claude.json")).unwrap_or_default();
    let claude_ok = has_mcp_server(&claude_cfg, "reliary");
    let claude_hooks_dir = home_dir().map(|h| h.join(".claude/hooks")).unwrap_or_default();
    let hooks_count = ["reliary-code-gate", "reliary-session-reminder", "reliary-sift-pretooluse"]
        .iter()
        .filter(|name| claude_hooks_dir.join(name).exists())
        .count();
    let claude_detail = if claude_ok {
        format!("MCP wired, {}/3 hooks installed", hooks_count)
    } else if claude_cfg.exists() {
        "config exists but not wired — run `reliary init`".into()
    } else {
        "not found (optional)".into()
    };
    checks.push(DoctorCheck {
        name: "claude",
        ok: claude_ok && hooks_count == 3,
        detail: claude_detail,
        fixable: false,
        optional: true,
    });

    // --- OpenCode integration (optional) ---
    let opencode_cfg = if cfg!(target_os = "windows") {
        dirs::config_dir().map(|d| d.join("opencode").join("opencode.json"))
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library/Application Support/opencode/opencode.json"))
    } else {
        home_dir().map(|h| h.join(".config/opencode/opencode.json"))
    }.unwrap_or_default();
    let opencode_ok = has_mcp_server(&opencode_cfg, "reliary");
    checks.push(DoctorCheck {
        name: "opencode",
        ok: opencode_ok,
        detail: if opencode_ok { "MCP wired".into() } else if opencode_cfg.exists() { "config exists but not wired — run `reliary init`".into() } else { "not found (optional)".into() },
        fixable: false,
        optional: true,
    });

    // --- Pi Agent integration (optional) ---
    let pi_gate = home_dir().map(|h| h.join(".local/share/reliary/gate.js")).unwrap_or_default();
    checks.push(DoctorCheck {
        name: "pi",
        ok: pi_gate.exists(),
        detail: if pi_gate.exists() { "gate.js installed".into() } else { "not found (optional)".into() },
        fixable: false,
        optional: true,
    });

    // --- Tee/recovery directory cleanup (low-priority hygiene) ---
    let tee_dir = std::env::temp_dir().join("reliary-tee");
    let tee_count = if tee_dir.exists() {
        std::fs::read_dir(&tee_dir).map(|d| d.count()).unwrap_or(0)
    } else {
        0
    };
    if tee_count > 100 {
        checks.push(DoctorCheck {
            name: "tee",
            ok: false,
            detail: format!("{} entries in {} — run `reliary clean --global` to free space", tee_count, tee_dir.display()),
            fixable: false,
            optional: true,
        });
    }

    // --- Multi-install check (installs passed in to avoid redundant find_installs call) ---
    if installs.len() > 1 {
        let active_version = installs.iter().find(|i| i.active).map(|i| i.version.clone()).unwrap_or_default();
        let stale_count = installs.iter().filter(|i| !i.active && i.version != active_version).count();
        if stale_count > 0 {
            checks.push(DoctorCheck {
                name: "installs",
                ok: false,
                detail: format!("{} installations, {} stale — remove old binaries from PATH", installs.len(), stale_count),
                fixable: false,
                optional: false,
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
            optional: false,
        });
    }

    checks
}

fn doctor_json(checks: &[DoctorCheck]) -> Value {
    let all_good = checks.iter().all(|c| c.ok || c.optional);
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
    let installs = find_installs();
    let mut checks = doctor_checks(&installs);

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

    let mut needs_index = false;

    for c in &checks {
        let icon = if c.ok {
            format!("{}✓{}", color(), reset())
        } else if c.optional {
            format!("{}-{}", dim(), reset())
        } else {
            format!("{}✗{}", red(), reset())
        };
        println!("  {} {} {}{}", icon, c.name, dim(), c.detail);
        if !c.ok && c.fixable
            && c.name == "index" { needs_index = true }
    }

    if fix && needs_index {
        println!();
        if needs_index {
            print!("  {} Building index... ", dim());
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("reliary-agent"));
            let status = Command::new(exe).arg("index").arg(".").stdout(std::process::Stdio::inherit()).stderr(std::process::Stdio::inherit()).status();
            if status.is_ok_and(|s| s.success()) { println!("{}done", color()); } else { println!("{}failed", red()); }
        }
        // Re-evaluate after fixes
        checks = doctor_checks(&installs);
    }

    if installs.len() > 1 {
        println!();
        println!("  {}| Installations |{}", color(), reset());
        print_install_table(&installs);
    }

    let all_good = checks.iter().all(|c| c.ok || c.optional);
    if all_good {
        println!("\n{}✓{} System ready.", color(), reset());
    } else if !fix {
        println!("\n  {}Tip: run 'reliary doctor --fix' to fix issues automatically.{}", dim(), reset());
    } else {
        println!("\n{}✓{} System ready after fix.", color(), reset());
        // Show remaining non-optional failures with hints
        for c in &checks {
            if !c.ok && !c.optional {
                let hint = match c.name {
                    "index" => "Run 'reliary index .' to view errors",
                    "installs" => "Remove stale binary paths manually",
                    _ => "Check the detail above",
                };
                println!("  {} {} {}", dim(), c.name, hint);
            }
        }
    }
}

struct StatusData {
    mode: String,
    index_files: i64,
    chronicle_events: i64,
    index_exists: bool,
    /// V70 P7: age of the index in seconds (None if no index).
    index_age_secs: Option<u64>,
    /// V70 P7: watcher availability (env-gated at spawn in the MCP server).
    watcher_enabled: bool,
}

fn status_data() -> StatusData {
    let index_path = PathBuf::from(".reliary/index.sqlite");
    let mut index_files = 0i64;
    let mut chronicle_events = 0i64;
    let index_exists = index_path.exists();

    if index_exists {
        if let Ok(db) = rusqlite::Connection::open(&index_path) {
            let _ = db.execute_batch("PRAGMA synchronous=NORMAL;");
            if let Ok(mut stmt) = db.prepare("SELECT COUNT(*) FROM file_map") {
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

    let index_age_secs = std::fs::metadata(&index_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs());
    let watcher_enabled = std::env::var("NO_RELIARY_WATCHER").is_err();

    StatusData {
        mode: crate::config::resolve_mode(Some(".")).as_str().to_string(),
        index_files,
        chronicle_events,
        index_exists,
        index_age_secs,
        watcher_enabled,
    }
}

fn status_json(d: &StatusData) -> Value {
    json!({
        "mode": d.mode,
        "index": {
            "exists": d.index_exists,
            "files": d.index_files,
            "age_secs": d.index_age_secs,
        },
        "watcher": { "enabled": d.watcher_enabled },
        "chronicle": { "events": d.chronicle_events },
    })
}

pub fn status(format: &str) {
    let d = status_data();

    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&status_json(&d)).unwrap_or_else(|_| "{}".to_string()));
        return;
    }

    println!("\n{}| Reliary Agent Status |{}\n", color(), reset());
    println!("  {}•{} Mode: {}", blue(), reset(), d.mode);

    if d.index_exists {
        let age = match d.index_age_secs {
            Some(s) if s < 60 => format!("{}s ago", s),
            Some(s) if s < 3600 => format!("{}m ago", s / 60),
            Some(s) if s < 86400 => format!("{}h ago", s / 3600),
            Some(s) => format!("{}d ago", s / 86400),
            None => "unknown".to_string(),
        };
        println!("  {}•{} Index: {} files indexed (updated {})", blue(), reset(), d.index_files, age);
        println!("  {}•{} Watcher: {}", blue(), reset(),
            if d.watcher_enabled { "enabled (auto-reindex on save)" } else { "disabled (NO_RELIARY_WATCHER=1)" });
        println!("  {}•{} Memory: {} chronicle events", blue(), reset(), d.chronicle_events);
    } else {
        println!("  {}•{} Index: {}-{} No index found", blue(), reset(), yellow(), reset());
        println!("    {}→ Run 'reliary index .' to build it{}", dim(), reset());
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
        // V61: doctor flags >100 tee entries but clean --global never
        // removed them — /tmp/reliary-tee grew forever.
        match crate::tee::clean_tee() {
            Ok(bytes) if bytes > 0 => {
                println!("{}✓{} Cleaned {} bytes of tee artifacts (/tmp/reliary-tee)", color(), reset(), bytes);
            }
            _ => {
                println!("{}-{} No tee artifacts to clean", yellow(), reset());
            }
        }
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
                let upper = format!(" [{}] ", lvl.to_uppercase());
                let lower = format!("[{}]", lower_lvl);
                if let Ok(content) = std::fs::read_to_string(log_path) {
                    for line in content.lines() {
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
    eprintln!("{} No RELIARY_LOG_FILE env var set.{}", yellow(), reset());
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
        // V74: floor_char_boundary — slicing at byte 60 panicked on multibyte text.
        println!("  {} {} {}", icon, path, &risk[..risk.floor_char_boundary(60.min(risk.len()))]);
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
            for key in &["mcpServers", "mcp"] {
                if let Some(servers) = v.get(key).and_then(|m| m.as_object()) {
                    if servers.contains_key(server_name) {
                        return true;
                    }
                }
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
