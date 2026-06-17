use std::path::PathBuf;
use std::fs;
use std::net::TcpStream;
use std::time::Duration;
use serde_json::Value;
use std::process::Command;

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

fn color() -> &'static str { "\x1b[1m\x1b[32m" }
fn reset() -> &'static str { "\x1b[0m" }
fn dim() -> &'static str { "\x1b[2m" }
fn blue() -> &'static str { "\x1b[34m" }
fn red() -> &'static str { "\x1b[31m" }
fn yellow() -> &'static str { "\x1b[33m" }

fn daemon_alive() -> bool {
    TcpStream::connect_timeout(&"127.0.0.1:9090".parse().expect("invalid port"), Duration::from_millis(500)).is_ok()
}

fn has_upstream() -> bool {
    // Check proxy-routes.json
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
    // Check RELIARY_UPSTREAM_URL
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

pub fn doctor(fix: bool) {
    println!("\n{}| Reliary Doctor |{}\n", color(), reset());
    let mut all_good = true;
    let mut needs_daemon = false;
    let mut needs_index = false;

    // 1. Daemon
    print!("{}•{} Daemon: ", blue(), reset());
    if daemon_alive() {
        println!("{}✓{} Active on :9090", color(), reset());
    } else {
        println!("{}✗{} Stopped", red(), reset());
        needs_daemon = true;
        all_good = false;
    }

    // 2. Proxy upstream routing
    print!("{}•{} Upstream: ", blue(), reset());
    if has_upstream() {
        let count = proxy_routes_count();
        if count > 0 {
            println!("{}✓{} {} routes in proxy-routes.json", color(), reset(), count);
        } else {
            println!("{}✓{} RELIARY_UPSTREAM_URL set", color(), reset());
        }
    } else {
        println!("{}⚠{} No upstream configured", yellow(), reset());
        println!("  {} Set RELIARY_UPSTREAM_URL or run 'init' for auto-discovery{}", dim(), reset());
    }

    // 3. Pi Agent
    print!("{}•{} Pi: ", blue(), reset());
    let pi_gate = home_dir().map(|h| h.join(".local/share/reliary/gate.js")).unwrap_or_default();
    if pi_gate.exists() {
        println!("{}✓{} gate.js installed", color(), reset());
    } else {
        println!("{}-{} gate.js not found (optional)", yellow(), reset());
    }

    // 4. Claude Code MCP
    print!("{}•{} Claude: ", blue(), reset());
    let claude_cfg = home_dir().map(|h| h.join(".claude.json")).unwrap_or_default();
    if has_mcp_server(&claude_cfg, "reliary") {
        println!("{}✓{} Wired", color(), reset());
    } else {
        println!("{}-{} Not wired", yellow(), reset());
    }

    // 5. OpenCode MCP
    print!("{}•{} OpenCode: ", blue(), reset());
    let opencode_cfg = if cfg!(target_os = "windows") {
        dirs::config_dir().map(|d| d.join("opencode").join("opencode.json"))
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library/Application Support/opencode/opencode.json"))
    } else {
        home_dir().map(|h| h.join(".config/opencode/opencode.json"))
    }.unwrap_or_default();
    if has_mcp_server(&opencode_cfg, "reliary") {
        println!("{}✓{} Wired", color(), reset());
    } else {
        println!("{}-{} Not wired", yellow(), reset());
    }

    // 6. Project Health
    print!("{}•{} Index: ", blue(), reset());
    let index_path = PathBuf::from(".reliary/index.sqlite");
    if index_path.exists() {
        println!("{}✓{} Index exists", color(), reset());
    } else {
        println!("{}-{} No index found{}", yellow(), reset(), "");
        needs_index = true;
    }

    // 7. Mode
    print!("{}•{} Mode: ", blue(), reset());
    let mode = crate::config::resolve_mode(Some("."));
    println!("{}", mode.as_str());

    // Auto-fix
    if fix {
        println!("");
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
                    if daemon_alive() {
                        println!("{} started!", color());
                    } else {
                        println!("{} may need manual start", yellow());
                    }
                }
                Err(e) => {
                    println!("{} failed: {}", red(), e);
                }
            }
        }
        if needs_index {
            print!("  {} Building index... ", dim());
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("reliary-agent"));
            let status = Command::new(exe).arg("index").arg(".").stdout(std::process::Stdio::inherit()).stderr(std::process::Stdio::inherit()).status();
            if status.map_or(false, |s| s.success()) {
                println!("{} done", color());
            } else {
                println!("{} failed", red());
            }
        }
        // Re-check after fixes
        if daemon_alive() && !needs_daemon {
            println!("  {} All good.", color());
        } else if needs_daemon && daemon_alive() {
            println!("  {} All good after fixes.", color());
        }
    } else if !all_good {
        println!("\n  {} Tip: run '{} {}' to fix issues automatically.",
            dim(), "reliary-agent", "doctor --fix");
    }

    if all_good {
        println!("\n{}✓{} System ready.", color(), reset());
    } else {
        println!("\n{}⚠{} Some checks failed.", yellow(), reset());
    }
}

pub fn status() {
    println!("\n{}| Reliary Agent Status |{}\n", color(), reset());

    // 1. Daemon / Proxy
    print!("{}•{} Proxy: ", blue(), reset());
    if daemon_alive() {
        println!("{}✓{} Running on :9090", color(), reset());
    } else {
        println!("{}✗{} Stopped", red(), reset());
        if cfg!(unix) {
            println!("  {}→ Run 'reliary-agent start' to run in background{}", dim(), reset());
        }
    }

    // 2. Gate Mode
    print!("{}•{} Mode: ", blue(), reset());
    let mode = crate::config::resolve_mode(Some("."));
    println!("{}", mode.as_str());

    // 3. Upstream routing
    print!("{}•{} Routes: ", blue(), reset());
    let count = proxy_routes_count();
    if count > 0 {
        println!("{} routes in proxy-routes.json", count);
    } else if std::env::var("RELIARY_UPSTREAM_URL").ok().is_some() {
        println!("RELIARY_UPSTREAM_URL set");
    } else {
        println!("{}-{} None (proxy won't route)", yellow(), reset());
        println!("  {}→ Set RELIARY_UPSTREAM_URL or run 'init'{}", dim(), reset());
    }

    // 4. Project Intelligence
    print!("{}•{} Index: ", blue(), reset());
    let index_path = PathBuf::from(".reliary/index.sqlite");
    if !index_path.exists() {
        println!("{}-{} No index found{}", yellow(), reset(), "");
        println!("  {}→ Run 'reliary-agent index .' to build it{}", dim(), reset());
    } else {
        if let Ok(db) = rusqlite::Connection::open(&index_path) {
            let _ = db.execute_batch("PRAGMA synchronous=NORMAL;");
            let mut file_count = 0;
            if let Ok(mut stmt) = db.prepare("SELECT COUNT(DISTINCT file_id) FROM file_phrases") {
                if let Ok(mut rows) = stmt.query([]) {
                    if let Ok(Some(row)) = rows.next() {
                        file_count = row.get::<_, i64>(0).unwrap_or(0);
                    }
                }
            }
            println!("{} files indexed", file_count);

            print!("{}•{} Memory: ", blue(), reset());
            let mut event_count = 0;
            if let Ok(mut stmt) = db.prepare("SELECT COUNT(*) FROM chronicle") {
                if let Ok(mut rows) = stmt.query([]) {
                    if let Ok(Some(row)) = rows.next() {
                        event_count = row.get::<_, i64>(0).unwrap_or(0);
                    }
                }
            }
            println!("{} chronicle events", event_count);
        } else {
            println!("{}✗{} Failed to open index", red(), reset());
        }
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
                let status = Command::new("tail")
                    .arg("-f")
                    .arg(&log_path_str)
                    .status();
                if status.is_err() {
                    if let Ok(content) = std::fs::read_to_string(log_path) {
                        println!("{}", content);
                    }
                }
            } else if let Some(lvl) = level {
                let lower_lvl = lvl.to_lowercase();
                if let Ok(content) = std::fs::read_to_string(log_path) {
                    for line in content.lines() {
                        let upper = format!(" [{}] ", lvl.to_uppercase());
                        let lower = format!("[{}]", lower_lvl);
                        if line.contains(&upper) || line.contains(&lower) {
                            println!("{}", line);
                        }
                    }
                }
            } else {
                if let Ok(content) = std::fs::read_to_string(log_path) {
                    println!("{}", content);
                }
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
        if let Some(lvl) = level {
            cmd.arg(format!("-p{}", lvl.to_uppercase()));
        }
        if tail {
            cmd.arg("-f");
        }
        let status = cmd.status();
        if status.is_err() || status.map_or(true, |s| !s.success()) {
            eprintln!("{} Could not read daemon logs.{}", yellow(), reset());
            eprintln!("  {} Is the daemon running? Run 'reliary-agent doctor' to check.{}", dim(), reset());
        }
        return;
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
                    if status.is_err() {
                        if let Ok(content) = std::fs::read_to_string(&path_str) {
                            println!("{}", content);
                        }
                    }
                } else if let Some(lvl) = level {
                    let lvl_upper = lvl.to_uppercase();
                    if let Ok(content) = std::fs::read_to_string(&path_str) {
                        for line in content.lines() {
                            if line.contains(&format!("[{}]", lvl_upper))
                                || line.contains(&format!("[{}]", lvl.to_lowercase()))
                            {
                                println!("{}", line);
                            }
                        }
                    }
                } else {
                    if let Ok(content) = std::fs::read_to_string(&path_str) {
                        println!("{}", content);
                    }
                }
                return;
            }
        }
        eprintln!("{} No daemon log file found.{}", yellow(), reset());
        eprintln!("  {} Start the daemon: 'reliary-agent serve &'{}", dim(), reset());
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
                            let path_str = entry.path().to_string_lossy().to_string();
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
