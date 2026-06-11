use std::path::PathBuf;
use std::fs;
use std::net::TcpStream;
use std::time::Duration;
use serde_json::Value;

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

pub fn doctor() {
    println!("\nReliary Doctor - System Health & Diagnosis");
    println!("------------------------------------------\n");

    let mut all_good = true;

    // 1. Daemon Status
    print!("Daemon Status: ");
    if TcpStream::connect_timeout(&"127.0.0.1:9799".parse().unwrap(), Duration::from_millis(500)).is_ok() {
        println!("✅ Active on port 9799");
    } else {
        println!("❌ Inactive or unreachable");
        println!("   💡 Run 'reliary-agent init' to install the service, or 'reliary-agent daemon &' to start it manually.");
        all_good = false;
    }

    // 2. Pi Agent
    print!("Pi Agent Integration: ");
    let pi_gate = home_dir().map(|h| h.join(".local/share/reliary/gate.js")).unwrap_or_default();
    if pi_gate.exists() {
        println!("✅ gate.js installed");
    } else {
        println!("❌ gate.js not found");
    }

    // 3. MCP Clients
    print!("Claude Code MCP: ");
    let claude_cfg = home_dir().map(|h| h.join(".claude.json")).unwrap_or_default();
    if has_mcp_server(&claude_cfg, "reliary") {
        println!("✅ Wired");
    } else {
        println!("- Not wired");
    }

    print!("OpenCode MCP: ");
    let opencode_cfg = if cfg!(target_os = "windows") {
        dirs::config_dir().map(|d| d.join("opencode").join("opencode.json"))
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library/Application Support/opencode/opencode.json"))
    } else {
        home_dir().map(|h| h.join(".config/opencode/opencode.json"))
    }.unwrap_or_default();
    if has_mcp_server(&opencode_cfg, "reliary") {
        println!("✅ Wired");
    } else {
        println!("- Not wired");
    }

    // 4. Project Health
    print!("\nProject Health: ");
    let index_path = PathBuf::from(".reliary/index.sqlite");
    if index_path.exists() {
        println!("✅ Index exists");
    } else {
        println!("❌ No index found in current directory");
        println!("   💡 Run 'reliary-agent index .' to build it.");
        all_good = false;
    }

    // 5. Config State
    print!("Config State: ");
    let mode = crate::config::resolve_mode(Some("."));
    println!("{} mode", mode.as_str());

    if all_good {
        println!("\n✨ Your system is healthy and ready to go!");
    } else {
        println!("\n⚠️ Some checks failed. See the tips above to fix them.");
    }
}

pub fn status() {
    println!("\nProject Intelligence Overview");
    println!("---------------------------\n");

    let index_path = PathBuf::from(".reliary/index.sqlite");
    if !index_path.exists() {
        println!("No index found in current directory. Run 'reliary-agent index .'");
        return;
    }

    if let Ok(db) = rusqlite::Connection::open(&index_path) {
        let mut file_count = 0;
        if let Ok(mut stmt) = db.prepare("SELECT COUNT(DISTINCT file_id) FROM file_phrases") {
            if let Ok(mut rows) = stmt.query([]) {
                if let Ok(Some(row)) = rows.next() {
                    file_count = row.get::<_, i64>(0).unwrap_or(0);
                }
            }
        }
        println!("Index: {} files indexed", file_count);

        let mut event_count = 0;
        if let Ok(mut stmt) = db.prepare("SELECT COUNT(*) FROM chronicle") {
            if let Ok(mut rows) = stmt.query([]) {
                if let Ok(Some(row)) = rows.next() {
                    event_count = row.get::<_, i64>(0).unwrap_or(0);
                }
            }
        }
        println!("Chronicle: {} events recorded", event_count);
    } else {
        println!("Failed to open index.");
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

pub fn logs() {
    println!("Daemon logs are managed by your OS service manager.");
    #[cfg(target_os = "linux")]
    {
        println!("Run: journalctl --user -u reliary-daemon.service -f");
    }
    #[cfg(target_os = "macos")]
    {
        println!("Check standard output/error files configured for com.reliary.daemon, or use Console.app.");
    }
    #[cfg(target_os = "windows")]
    {
        println!("Daemon runs silently via VBScript on Windows. Custom logging is not currently implemented.");
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
