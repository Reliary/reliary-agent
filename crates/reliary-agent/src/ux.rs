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
    println!("\n{} Reliary Doctor — System Health & Diagnosis {}\n", "\x1b[1m\x1b[34m", "\x1b[0m");
    let mut all_good = true;

    // 1. Daemon Status
    print!("{}  Daemon Status: ", "\x1b[34m•\x1b[0m");
    if TcpStream::connect_timeout(&"127.0.0.1:9090".parse().expect("invalid port"), Duration::from_millis(500)).is_ok() {
        println!("{} Active on port 9090", "\x1b[32m✓\x1b[0m");
    } else {
        println!("{} Inactive or unreachable", "\x1b[31m✗\x1b[0m");
        println!("     {} Run 'reliary-agent serve' to start it", "\x1b[2m→\x1b[0m");
        all_good = false;
    }

    // 2. Proxy Status
    print!("{}  Proxy Status: ", "\x1b[34m•\x1b[0m");
    if TcpStream::connect_timeout(&"127.0.0.1:9090".parse().expect("invalid port"), Duration::from_millis(500)).is_ok() {
        println!("{} Active (auth-based routing)", "\x1b[32m✓\x1b[0m");
    } else {
        println!("{} Inactive", "\x1b[31m✗\x1b[0m");
    }

    // 3. Pi Agent
    print!("{}  Pi Agent: ", "\x1b[34m•\x1b[0m");
    let pi_gate = home_dir().map(|h| h.join(".local/share/reliary/gate.js")).unwrap_or_default();
    if pi_gate.exists() {
        println!("{} gate.js installed", "\x1b[32m✓\x1b[0m");
    } else {
        println!("{} gate.js not found (optional — only needed for Pi)", "\x1b[33m-\x1b[0m");
    }

    // 4. MCP Clients
    print!("{}  Claude Code MCP: ", "\x1b[34m•\x1b[0m");
    let claude_cfg = home_dir().map(|h| h.join(".claude.json")).unwrap_or_default();
    if has_mcp_server(&claude_cfg, "reliary") {
        println!("{} Wired", "\x1b[32m✓\x1b[0m");
    } else {
        println!("{} Not wired (run 'rel init')", "\x1b[33m-\x1b[0m");
    }

    print!("{}  OpenCode MCP: ", "\x1b[34m•\x1b[0m");
    let opencode_cfg = if cfg!(target_os = "windows") {
        dirs::config_dir().map(|d| d.join("opencode").join("opencode.json"))
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library/Application Support/opencode/opencode.json"))
    } else {
        home_dir().map(|h| h.join(".config/opencode/opencode.json"))
    }.unwrap_or_default();
    if has_mcp_server(&opencode_cfg, "reliary") {
        println!("{} Wired", "\x1b[32m✓\x1b[0m");
    } else {
        println!("{} Not wired", "\x1b[33m-\x1b[0m");
    }

    // 5. Project Health
    print!("\n{}  Project Health: ", "\x1b[34m•\x1b[0m");
    let index_path = PathBuf::from(".reliary/index.sqlite");
    if index_path.exists() {
        println!("{} Index exists", "\x1b[32m✓\x1b[0m");
    } else {
        println!("{} No index found", "\x1b[33m-\x1b[0m");
        println!("     {} Run 'reliary-agent index .' to build it", "\x1b[2m→\x1b[0m");
    }

    // 6. Config State
    print!("{}  Config State: ", "\x1b[34m•\x1b[0m");
    let mode = crate::config::resolve_mode(Some("."));
    println!("{} mode", mode.as_str());

    if all_good {
        println!("\n  {} System ready.", "\x1b[32m✓\x1b[0m");
    } else {
        println!("\n  {} Some checks failed. See tips above.", "\x1b[33m⚠\x1b[0m");
    }
}

pub fn status() {
    println!("\n{} Project Intelligence Overview {}\n", "\x1b[1m\x1b[34m", "\x1b[0m");

    let index_path = PathBuf::from(".reliary/index.sqlite");
    if !index_path.exists() {
        println!("{} No index found in current directory.", "\x1b[33m-\x1b[0m");
        println!("     {} Run 'reliary-agent index .' to build it", "\x1b[2m→\x1b[0m");
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
        println!("{} Index: {} files indexed", "\x1b[34m•\x1b[0m", file_count);

        let mut event_count = 0;
        if let Ok(mut stmt) = db.prepare("SELECT COUNT(*) FROM chronicle") {
            if let Ok(mut rows) = stmt.query([]) {
                if let Ok(Some(row)) = rows.next() {
                    event_count = row.get::<_, i64>(0).unwrap_or(0);
                }
            }
        }
        println!("{} Chronicle: {} events recorded", "\x1b[34m•\x1b[0m", event_count);
    } else {
        println!("{} Failed to open index.", "\x1b[31m✗\x1b[0m");
    }
}

pub fn clean(global: bool, all: bool) {
    let do_global = global || all;
    let do_local = !global || all;

    if do_local {
        let local_dir = PathBuf::from(".reliary");
        if local_dir.exists() {
            if fs::remove_dir_all(&local_dir).is_ok() {
                println!("✓ Cleaned project state (.reliary)");
            } else {
                println!("✗ Failed to clean project state");
            }
        } else {
            println!("- No project state found");
        }
    }

    if do_global {
        if let Some(home) = home_dir() {
            let global_dir = home.join(".reliary");
            if global_dir.exists() {
                if fs::remove_dir_all(&global_dir).is_ok() {
                    println!("✓ Cleaned global state (~/.reliary)");
                } else {
                    println!("✗ Failed to clean global state");
                }
            } else {
                println!("- No global state found");
            }
        }
    }
}

pub fn logs(tail: bool, level: Option<String>) {
    // If RELIARY_LOG_FILE is set, tail or dump the log file
    if let Ok(log_path_str) = std::env::var("RELIARY_LOG_FILE") {
        let log_path = std::path::Path::new(&log_path_str);
        if log_path.exists() {
            if tail {
                println!("Tailing {}...", log_path_str);
                let status = Command::new("tail")
                    .arg("-f")
                    .arg(&log_path_str)
                    .status();
                if status.is_err() {
                    // tail not available, fall back to dump
                    if let Ok(content) = std::fs::read_to_string(log_path) {
                        println!("{}", content);
                    }
                }
            } else {
                if let Some(lvl) = level {
                    if let Ok(content) = std::fs::read_to_string(log_path) {
                        for line in content.lines() {
                            if line.contains(&format!("[{}]", lvl)) {
                                println!("{}", line);
                            }
                        }
                    }
                } else {
                    if let Ok(content) = std::fs::read_to_string(log_path) {
                        println!("{}", content);
                    }
                }
            }
            return;
        }
    }

    // Fallback: show OS-specific log management
    println!("Daemon logs are managed by your OS service manager.");
    #[cfg(target_os = "linux")]
    { println!("Run: journalctl --user -u reliary-daemon.service -f"); }
    #[cfg(target_os = "macos")]
    { println!("Check standard output/error files configured for com.reliary.daemon, or use Console.app."); }
    #[cfg(target_os = "windows")]
    { println!("Daemon runs silently via VBScript on Windows. Custom logging is not currently implemented."); }

    if !tail {
        println!("");
        println!("Set RELIARY_LOG_FILE=/path/to/daemon.log to enable file logging.");
        println!("Set RELIARY_LOG=debug|trace for verbose output.");
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
