use std::io::{self, Write};
use std::path::PathBuf;
use std::fs;
use std::process::Command;
use serde_json::Value;

fn ok(msg: &str) { println!("  {} {}", "\x1b[32m✓\x1b[0m", msg); }

// Embed gate.js at compile time
const EMBEDDED_GATE_JS: &str = include_str!("../../../pi/gate.js");

fn ask_yes_no(prompt: &str, default: bool) -> bool {
    let def_str = if default { "[Y/n]" } else { "[y/N]" };
    print!("{} {}: ", prompt, def_str);
    let _ = io::stdout().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_ok() {
        let input = input.trim().to_lowercase();
        if input.is_empty() {
            return default;
        }
        return input == "y" || input == "yes";
    }
    default
}

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

fn get_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir()
    }
    #[cfg(not(target_os = "windows"))]
    {
        home_dir().map(|h| h.join(".local/share"))
    }
}

pub fn run() {
    println!("\nReliary Setup");
    println!("-------------");

    let mut configured_agents = 0;

    // 1. Pi Agent
    let pi_bin = home_dir().map(|h| h.join(".local/bin/pi")).unwrap_or_else(|| PathBuf::from("pi"));
    let has_pi = pi_bin.exists() || Command::new("pi").arg("--version").output().is_ok();
    
    if has_pi {
        if ask_yes_no("Found Pi Agent. Install Reliary extension?", true) {
            if let Some(data_dir) = get_data_dir() {
                let target_dir = data_dir.join("reliary");
                if fs::create_dir_all(&target_dir).is_ok() {
                    let target_path = target_dir.join("gate.js");
                    if fs::write(&target_path, EMBEDDED_GATE_JS).is_ok() {
                        let pi_cmd = if pi_bin.exists() { pi_bin.to_str().unwrap_or("pi") } else { "pi" };
                        let status = Command::new(pi_cmd)
                            .args(["install", target_path.to_str().unwrap_or("/dev/null")])
                            .status();
                        
                        if status.is_ok() && status.unwrap_or_default().success() {
                            ok("Installed gate.js");
                            configured_agents += 1;
                        } else {
                            println!("✗ Failed to run `pi install`\n");
                        }
                    } else {
                        println!("✗ Failed to write gate.js\n");
                    }
                } else {
                    println!("✗ Failed to create directory {:?}\n", target_dir);
                }
            } else {
                println!("✗ Could not determine data directory\n");
            }
        } else {
            println!("- Skipped\n");
        }
    }

    // 2. Claude Code
    if let Some(home) = home_dir() {
        let claude_cfg = home.join(".claude.json");
        if claude_cfg.exists() {
            if ask_yes_no("Found Claude Code config. Add Reliary MCP server?", true) {
                if inject_mcp_server(&claude_cfg, "reliary") {
                    ok("Updated ~/.claude.json");
                    configured_agents += 1;
                } else {
                    println!("✗ Failed to update ~/.claude.json\n");
                }
            } else {
                println!("- Skipped\n");
            }
        }
    }

    // 3. OpenCode
    if let Some(home) = home_dir() {
        let opencode_cfg = if cfg!(target_os = "windows") {
            dirs::config_dir().map(|d| d.join("opencode").join("opencode.json"))
        } else if cfg!(target_os = "macos") {
            Some(home.join("Library/Application Support/opencode/opencode.json"))
        } else {
            Some(home.join(".config/opencode/opencode.json"))
        };

        if let Some(cfg_path) = opencode_cfg {
            if cfg_path.exists() {
                if ask_yes_no("Found OpenCode config. Add Reliary MCP server?", true) {
                    if inject_mcp_server(&cfg_path, "reliary") {
                        ok("Updated opencode.json");
                        configured_agents += 1;
                    } else {
                        println!("✗ Failed to update opencode.json\n");
                    }
                } else {
                    println!("- Skipped\n");
                }
            }
        }
    }
    
    // 4. Cline
    if let Some(home) = home_dir() {
        let cline_cfg = if cfg!(target_os = "windows") {
            dirs::data_dir().map(|d| d.join("Code").join("User").join("globalStorage").join("rooveterinery.cline").join("cline_mcp_settings.json"))
        } else if cfg!(target_os = "macos") {
            Some(home.join("Library/Application Support/Code/User/globalStorage/rooveterinery.cline/cline_mcp_settings.json"))
        } else {
            Some(home.join(".config/Code/User/globalStorage/rooveterinery.cline/cline_mcp_settings.json"))
        };

        if let Some(cfg_path) = cline_cfg {
            if cfg_path.exists() {
                if ask_yes_no("Found Cline config. Add Reliary MCP server?", true) {
                    if inject_mcp_server(&cfg_path, "reliary") {
                        ok("Updated cline MCP settings");
                        configured_agents += 1;
                    } else {
                        println!("✗ Failed to update cline_mcp_settings.json\n");
                    }
                } else {
                    println!("- Skipped\n");
                }
            }
        }
    }

    if configured_agents == 0 {
        println!("No agents were configured.\n");
    }

    // 5. Daemon
    if ask_yes_no("Do you want to install the Reliary daemon to run on boot?\n(Enables cross-session memory, dead code removal, faster search)", true) {
        if install_daemon() {
            ok("Daemon installed and started");
        } else {
            println!("✗ Failed to install daemon\n");
        }
    } else {
        println!("- Skipped\n");
    }

    println!("Setup complete! Your agents are now connected.");
}

fn inject_mcp_server(cfg_path: &PathBuf, server_name: &str) -> bool {
    let content = match fs::read_to_string(cfg_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    
    let mut v: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };

    if let Some(obj) = v.as_object_mut() {
        let mcp_servers = obj.entry("mcpServers").or_insert(serde_json::json!({}));
        if let Some(servers) = mcp_servers.as_object_mut() {
            let exe_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("reliary-agent"));
            let exe_str = exe_path.to_string_lossy().to_string();
            
            servers.insert(server_name.to_string(), serde_json::json!({
                "command": exe_str,
                "args": ["mcp"]
            }));
            
            if let Ok(new_content) = serde_json::to_string_pretty(&v) {
                return fs::write(cfg_path, new_content).is_ok();
            }
        }
    }
    false
}

fn install_daemon() -> bool {
    let exe_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("reliary-agent"));
    let exe_str = exe_path.to_string_lossy().to_string();

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = home_dir() {
            let service_dir = home.join(".config/systemd/user");
            if fs::create_dir_all(&service_dir).is_err() { return false; }
            
            let service_path = service_dir.join("reliary-daemon.service");
            let service_content = format!(
                "[Unit]\nDescription=Reliary Agent Daemon\n\n[Service]\nExecStart={} daemon\nRestart=always\n\n[Install]\nWantedBy=default.target\n",
                exe_str
            );
            
            if fs::write(&service_path, service_content).is_err() { return false; }
            
            let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
            let enable = Command::new("systemctl").args(["--user", "enable", "--now", "reliary-daemon.service"]).status();
            return enable.is_ok() && enable.unwrap_or_default().success();
        }
        false
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            let plist_dir = home.join("Library/LaunchAgents");
            if fs::create_dir_all(&plist_dir).is_err() { return false; }
            
            let plist_path = plist_dir.join("com.reliary.daemon.plist");
            let plist_content = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.reliary.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
"#, exe_str
            );
            
            if fs::write(&plist_path, plist_content).is_err() { return false; }
            
            let _ = Command::new("launchctl").args(["unload", "-w", plist_path.to_str().unwrap_or("")]).status();
            let load = Command::new("launchctl").args(["load", "-w", plist_path.to_str().unwrap_or("")]).status();
            return load.is_ok() && load.unwrap_or_default().success();
        }
        false
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(roaming) = dirs::config_dir() {
            let startup_dir = roaming.join("Microsoft/Windows/Start Menu/Programs/Startup");
            if fs::create_dir_all(&startup_dir).is_err() { return false; }
            
            let vbs_path = startup_dir.join("reliary-daemon.vbs");
            let vbs_content = format!(
                "Set WshShell = CreateObject(\"WScript.Shell\")\nWshShell.Run chr(34) & \"{}\" & chr(34) & \" daemon\", 0\nSet WshShell = Nothing\n",
                exe_str
            );
            
            if fs::write(&vbs_path, vbs_content).is_ok() {
                // Try to start it right now too
                let _ = Command::new("wscript").arg(vbs_path.to_str().unwrap_or("")).status();
                return true;
            }
        }
        false
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        println!("Platform not supported for auto-start daemon.");
        false
    }
}

pub fn uninstall() {
    println!("\nReliary Uninstall");
    println!("-----------------");

    let mut removed_agents = 0;

    // 1. Pi Agent
    let pi_bin = home_dir().map(|h| h.join(".local/bin/pi")).unwrap_or_else(|| PathBuf::from("pi"));
    let has_pi = pi_bin.exists() || Command::new("pi").arg("--version").output().is_ok();
    
    if has_pi {
        println!("Removing Pi Agent extension...");
        if let Some(data_dir) = get_data_dir() {
            let target_dir = data_dir.join("reliary");
            let target_path = target_dir.join("gate.js");
            
            if target_path.exists() {
                let pi_cmd = if pi_bin.exists() { pi_bin.to_str().unwrap_or("pi") } else { "pi" };
                let _ = Command::new(pi_cmd)
                    .args(["uninstall", target_path.to_str().unwrap_or("/dev/null")])
                    .status();
                
                let _ = fs::remove_file(&target_path);
                
                // Attempt to remove directory if empty
                if let Ok(entries) = fs::read_dir(&target_dir) {
                    if entries.count() == 0 {
                        let _ = fs::remove_dir(&target_dir);
                    }
                }
                
                ok("Removed gate.js");
                removed_agents += 1;
            } else {
                println!("- gate.js not found\n");
            }
        }
    }

    // 2. Claude Code
    println!("Removing MCP integrations...");
    if let Some(home) = home_dir() {
        let claude_cfg = home.join(".claude.json");
        if claude_cfg.exists() && remove_mcp_server(&claude_cfg, "reliary") {
            ok("Removed Reliary from Claude Code");
            removed_agents += 1;
        }
    }

    // 3. OpenCode
    if let Some(home) = home_dir() {
        let opencode_cfg = if cfg!(target_os = "windows") {
            dirs::config_dir().map(|d| d.join("opencode").join("opencode.json"))
        } else if cfg!(target_os = "macos") {
            Some(home.join("Library/Application Support/opencode/opencode.json"))
        } else {
            Some(home.join(".config/opencode/opencode.json"))
        };

        if let Some(cfg_path) = opencode_cfg {
            if cfg_path.exists() && remove_mcp_server(&cfg_path, "reliary") {
                ok("Removed Reliary from OpenCode");
                removed_agents += 1;
            }
        }
    }
    
    // 4. Cline
    if let Some(home) = home_dir() {
        let cline_cfg = if cfg!(target_os = "windows") {
            dirs::data_dir().map(|d| d.join("Code").join("User").join("globalStorage").join("rooveterinery.cline").join("cline_mcp_settings.json"))
        } else if cfg!(target_os = "macos") {
            Some(home.join("Library/Application Support/Code/User/globalStorage/rooveterinery.cline/cline_mcp_settings.json"))
        } else {
            Some(home.join(".config/Code/User/globalStorage/rooveterinery.cline/cline_mcp_settings.json"))
        };

        if let Some(cfg_path) = cline_cfg {
            if cfg_path.exists() && remove_mcp_server(&cfg_path, "reliary") {
                ok("Removed Reliary from Cline");
                removed_agents += 1;
            }
        }
    }

    if removed_agents == 0 {
        println!("- No MCP integrations found or modified");
    }
    println!();

    // 5. Daemon
    println!("Stopping and removing background daemon...");
    if uninstall_daemon() {
        ok("Daemon service removed");
    } else {
        println!("- Daemon service not found or could not be removed\n");
    }

    // 6. Config
    if ask_yes_no("Do you want to delete global configuration files? (~/.reliary)", false) {
        if let Some(home) = home_dir() {
            let config_dir = home.join(".reliary");
            if config_dir.exists() {
                if fs::remove_dir_all(&config_dir).is_ok() {
                    ok("Deleted ~/.reliary");
                } else {
                    println!("✗ Failed to delete ~/.reliary\n");
                }
            } else {
                println!("- ~/.reliary not found\n");
            }
        }
    } else {
        println!("- Skipped\n");
    }

    println!("Uninstall complete. You can now safely run `cargo uninstall reliary-agent`.");
}

fn remove_mcp_server(cfg_path: &PathBuf, server_name: &str) -> bool {
    let content = match fs::read_to_string(cfg_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    
    let mut v: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };

    if let Some(obj) = v.as_object_mut() {
        if let Some(mcp_servers) = obj.get_mut("mcpServers").and_then(|m| m.as_object_mut()) {
            if mcp_servers.remove(server_name).is_some() {
                if let Ok(new_content) = serde_json::to_string_pretty(&v) {
                    return fs::write(cfg_path, new_content).is_ok();
                }
            }
        }
    }
    false
}

fn uninstall_daemon() -> bool {
    let mut removed = false;

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = home_dir() {
            let service_path = home.join(".config/systemd/user/reliary-daemon.service");
            if service_path.exists() {
                let _ = Command::new("systemctl").args(["--user", "disable", "--now", "reliary-daemon.service"]).status();
                if fs::remove_file(&service_path).is_ok() {
                    removed = true;
                }
                let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            let plist_path = home.join("Library/LaunchAgents/com.reliary.daemon.plist");
            if plist_path.exists() {
                let _ = Command::new("launchctl").args(["unload", "-w", plist_path.to_str().unwrap_or("")]).status();
                if fs::remove_file(&plist_path).is_ok() {
                    removed = true;
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(roaming) = dirs::config_dir() {
            let vbs_path = roaming.join("Microsoft/Windows/Start Menu/Programs/Startup/reliary-daemon.vbs");
            if vbs_path.exists() {
                let _ = Command::new("taskkill").args(["/F", "/IM", "reliary-agent.exe"]).status();
                if fs::remove_file(&vbs_path).is_ok() {
                    removed = true;
                }
            }
        }
    }
    
    removed
}
