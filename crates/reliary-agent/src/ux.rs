use std::path::PathBuf;
use std::fs;
use std::net::TcpStream;
use std::time::Duration;
use serde_json::Value;
use std::process::Command;

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

pub fn doctor() {
    println!("\n{}| Reliary Doctor |{}\n", color(), reset());
    let mut all_good = true;

    // 1. Daemon Status
    print!("{}•{} Daemon Status: ", blue(), reset());
    if TcpStream::connect_timeout(&"127.0.0.1:9090".parse().expect("invalid port"), Duration::from_millis(500)).is_ok() {
        println!("{}✓{} Active on port 9090", color(), reset());
    } else {
        println!("{}✗{} Inactive or unreachable", red(), reset());
        println!("  {}→ Run 'reliary-agent serve' to start it{}", dim(), reset());
        all_good = false;
    }

    // 2. Proxy Status (same check — same port)
    print!("{}•{} Proxy Status: ", blue(), reset());
    if TcpStream::connect_timeout(&"127.0.0.1:9090".parse().expect("invalid port"), Duration::from_millis(500)).is_ok() {
        println!("{}✓{} Active (auth-based routing)", color(), reset());
    } else {
        println!("{}✗{} Inactive{}", red(), reset(), if !all_good { "" } else { " (daemon not running, proxy can't start)" });
    }

    // 3. Pi Agent
    print!("{}•{} Pi Agent: ", blue(), reset());
    let pi_gate = home_dir().map(|h| h.join(".local/share/reliary/gate.js")).unwrap_or_default();
    if pi_gate.exists() {
        println!("{}✓{} gate.js installed", color(), reset());
    } else {
        println!("{}-{} gate.js not found (optional — only needed for Pi)", yellow(), reset());
    }

    // 4. Claude Code MCP
    print!("{}•{} Claude Code MCP: ", blue(), reset());
    let claude_cfg = home_dir().map(|h| h.join(".claude.json")).unwrap_or_default();
    if has_mcp_server(&claude_cfg, "reliary") {
        println!("{}✓{} Wired", color(), reset());
    } else {
        println!("{}-{} Not wired (run 'rel init')", yellow(), reset());
    }

    // 5. OpenCode MCP
    print!("{}•{} OpenCode MCP: ", blue(), reset());
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
    print!("\n{}•{} Project Health: ", blue(), reset());
    let index_path = PathBuf::from(".reliary/index.sqlite");
    if index_path.exists() {
        println!("{}✓{} Index exists", color(), reset());
    } else {
        println!("{}-{} No index found{}", yellow(), reset(), "");
        println!("  {}→ Run 'reliary-agent index .' to build it{}", dim(), reset());
    }

    // 7. Config State
    print!("{}•{} Config State: ", blue(), reset());
    let mode = crate::config::resolve_mode(Some("."));
    println!("{}", mode.as_str());

    if all_good {
        println!("\n{}✓{} System ready.", color(), reset());
    } else {
        println!("\n{}⚠{} Some checks failed. See tips above.", yellow(), reset());
    }
}

pub fn status() {
    println!("\n{}| Project Intelligence Overview |{}\n", color(), reset());

    let index_path = PathBuf::from(".reliary/index.sqlite");
    if !index_path.exists() {
        println!("{}-{} No index found in current directory.{}", yellow(), reset(), "");
        println!("  {}→ Run 'reliary-agent index .' to build it{}", dim(), reset());
        return;
    }

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
        println!("{}•{} Index: {} files indexed", blue(), reset(), file_count);

        let mut event_count = 0;
        if let Ok(mut stmt) = db.prepare("SELECT COUNT(*) FROM chronicle") {
            if let Ok(mut rows) = stmt.query([]) {
                if let Ok(Some(row)) = rows.next() {
                    event_count = row.get::<_, i64>(0).unwrap_or(0);
                }
            }
        }
        println!("{}•{} Chronicle: {} events recorded", blue(), reset(), event_count);
    } else {
        println!("{}✗{} Failed to open index.", red(), reset());
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

fn color() -> &'static str { "\x1b[1m\x1b[32m" }
fn reset() -> &'static str { "\x1b[0m" }
fn dim() -> &'static str { "\x1b[2m" }
fn blue() -> &'static str { "\x1b[34m" }
fn red() -> &'static str { "\x1b[31m" }
fn yellow() -> &'static str { "\x1b[33m" }

pub fn logs(tail: bool, level: Option<String>) {
    // If RELIARY_LOG_FILE is set, tail or dump the log file
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

    // No RELIARY_LOG_FILE set — invoke OS log manager directly
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
        // macOS: try to find the daemon's log file
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

    // Fallback (unsupported OS)
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
