use std::io::{self, Write};
use std::path::PathBuf;
use std::fs;
use std::collections::HashMap;
use std::process::Command;
use serde_json::Value;

fn ok(msg: &str) { println!("  {} {}", "\x1b[32m✓\x1b[0m", msg); }

// Embed gate.js at compile time

/// Atomic write: write to tmp, sync, rename. Prevents partial write corruption.
fn atomic_write(path: &str, content: &str) -> bool {
    let tmp = format!("{}.tmp.{}", path, std::process::id());
    std::fs::write(&tmp, content).is_ok() && std::fs::rename(&tmp, path).is_ok()
}
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
                    let content = EMBEDDED_GATE_JS.as_bytes();
                    let tmp = format!("{}.tmp.{}", target_path.display(), std::process::id());
                    if std::fs::write(&tmp, content).is_ok()
                        && std::fs::rename(&tmp, &target_path).is_ok()
                    {
                        let pi_cmd = if pi_bin.exists() { pi_bin.to_str().unwrap_or("pi") } else { "pi" };
                        let status = Command::new(pi_cmd)
                            .args(["install", target_path.to_str().unwrap_or("/dev/null")])
                            .output();
                        
                        if let Ok(output) = status {
                            if output.status.success() {
                                ok("Installed gate.js");
                                configured_agents += 1;
                            } else {
                                println!("  {} Failed to run `pi install`\n", "\x1b[31m✗\x1b[0m");
                            }
                        } else {
                            println!("  {} Failed to run `pi install`\n", "\x1b[31m✗\x1b[0m");
                        }
                    } else {
                        println!("  {} Failed to write gate.js\n", "\x1b[31m✗\x1b[0m");
                    }
                } else {
                    println!("  {} Failed to create directory {:?}\n", "\x1b[31m✗\x1b[0m", target_dir);
                }
            } else {
                println!("  {} Could not determine data directory\n", "\x1b[31m✗\x1b[0m");
            }
        } else {
            println!("  {} Skipped\n", "\x1b[33m-\x1b[0m");
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
                        println!("  {} Failed to update ~/.claude.json\n", "\x1b[31m✗\x1b[0m");
                    }
            } else {
                println!("  {} Skipped\n", "\x1b[33m-\x1b[0m");
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
                // Offer SSE MCP URL if daemon will be installed
                let use_sse = ask_yes_no("Found OpenCode config. Add Reliary MCP server via SSE? (single port, shared state)", true);
                if use_sse {
                    if inject_sse_mcp_server(&cfg_path, "reliary", 9090) {
                        ok("Updated opencode.json (SSE MCP)");
                        configured_agents += 1;
                    } else if inject_mcp_server(&cfg_path, "reliary") {
                        ok("Updated opencode.json (stdio fallback)");
                        configured_agents += 1;
                    } else {
                        println!("  {} Failed to update opencode.json\n", "\x1b[31m✗\x1b[0m");
                    }
                } else {
                    println!("  {} Skipped\n", "\x1b[33m-\x1b[0m");
                }

                // Proxy routing — after MCP injection, ask to mutate provider baseURLs
                if ask_yes_no("\nRoute all OpenCode providers through Reliary proxy?\n(Your provider baseURLs will be updated to point at localhost:9090\nand proxy-routes.json will be generated automatically)", true) {
                    let (count, routes) = inject_opencode_proxy_routes(&cfg_path);
                    if count > 0 {
                        ok(&format!("Updated {} OpenCode provider baseURLs", count));
                        if write_proxy_routes(&routes) {
                            ok("Generated ~/.reliary/proxy-routes.json");
                        }
                    } else {
                        println!("  {} No providers found to update\n", "\x1b[33m-\x1b[0m");
                    }
                } else {
                    println!("  {} Skipped\n", "\x1b[33m-\x1b[0m");
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
                        println!("  {} Failed to update cline_mcp_settings.json\n", "\x1b[31m✗\x1b[0m");
                    }
                } else {
                    println!("  {} Skipped\n", "\x1b[33m-\x1b[0m");
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
            println!("  {} Failed to install daemon\n", "\x1b[31m✗\x1b[0m");
        }
    } else {
        println!("  {} Skipped\n", "\x1b[33m-\x1b[0m");
    }

    println!("\n  {} Setup complete! Your agents are now connected.", "\x1b[32m✓\x1b[0m");
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
                return atomic_write(&cfg_path.to_string_lossy(), &new_content);
            }
        }
    }
    false
}

/// Inject SSE MCP server entry into config JSON (url-based, no subprocess).
fn inject_sse_mcp_server(cfg_path: &PathBuf, server_name: &str, port: u16) -> bool {
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
            servers.insert(server_name.to_string(), serde_json::json!({
                "url": format!("http://127.0.0.1:{}/mcp/sse", port),
            }));
            if let Ok(new_content) = serde_json::to_string_pretty(&v) {
                return atomic_write(&cfg_path.to_string_lossy(), &new_content);
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
            
            if !atomic_write(&service_path.to_string_lossy(), &service_content) { return false; }
            
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
            
            if !atomic_write(&plist_path.to_string_lossy(), &plist_content) { return false; }
            
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
            
            if atomic_write(&vbs_path.to_string_lossy(), &vbs_content) {
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
            if cfg_path.exists() {
                if remove_mcp_server(&cfg_path, "reliary") {
                    ok("Removed Reliary from OpenCode");
                    removed_agents += 1;
                }
                // Restore original provider baseURLs
                if restore_opencode_proxy_routes() {
                    ok("Restored OpenCode provider baseURLs");
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
                    return atomic_write(&cfg_path.to_string_lossy(), &new_content);
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

/// Inject proxy baseURLs for all OpenCode providers.
/// Returns (count_of_updated_providers, routes_map).
fn inject_opencode_proxy_routes(cfg_path: &PathBuf) -> (usize, HashMap<String, String>) {
    let content = match std::fs::read_to_string(cfg_path) {
        Ok(c) => c,
        Err(_) => return (0, HashMap::new()),
    };
    let mut v: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return (0, HashMap::new()),
    };

    let mut routes: HashMap<String, String> = HashMap::new();
    let mut backups: HashMap<String, String> = HashMap::new();
    let mut count = 0;

    if let Some(obj) = v.as_object_mut() {
        if let Some(providers) = obj.get_mut("provider").and_then(|p| p.as_object_mut()) {
            for (name, provider) in providers.iter_mut() {
                if let Some(options) = provider.get_mut("options").and_then(|o| o.as_object_mut()) {
                    let original_url = match options.get("baseURL").and_then(|v| v.as_str()) {
                        Some(u) => u.to_string(),
                        None => continue,
                    };
                    // Skip if already pointing at proxy
                    if original_url.contains("127.0.0.1:9090") || original_url.contains("localhost:9090") {
                        continue;
                    }
                    let api_key = match options.get("apiKey").and_then(|v| v.as_str()) {
                        Some(k) => k.to_string(),
                        None => continue,
                    };
                    // Update baseURL to proxy
                    options.insert("baseURL".to_string(), Value::String("http://127.0.0.1:9090/v1".to_string()));
                    routes.insert(api_key, original_url.clone());
                    backups.insert(name.clone(), original_url);
                    count += 1;
                }
            }
        }
    }

    // Write updated opencode.json
    if count > 0 {
        // Add backup metadata to the routes map
        let mut routes_with_backups = routes.clone();
        if !backups.is_empty() {
            routes_with_backups.insert("__backups".to_string(), serde_json::to_string(&backups).unwrap_or_default());
        }

        if let Ok(new_content) = serde_json::to_string_pretty(&v) {
            atomic_write(&cfg_path.to_string_lossy(), &new_content);
        }
        (count, routes)
    } else {
        (0, routes)
    }
}

/// Write proxy-routes.json to ~/.reliary/
fn write_proxy_routes(routes: &HashMap<String, String>) -> bool {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };
    let config_dir = home.join(".reliary");
    let _ = std::fs::create_dir_all(&config_dir);

    // Build routes JSON from api_key -> upstream_url
    let mut routes_json = serde_json::Map::new();
    for (key, value) in routes {
        routes_json.insert(key.clone(), Value::String(value.clone()));
    }

    let routes_path = config_dir.join("proxy-routes.json");
    let content = serde_json::to_string_pretty(&serde_json::Value::Object(routes_json))
        .unwrap_or_default();
    atomic_write(&routes_path.to_string_lossy(), &content)
}

/// Restore original OpenCode provider baseURLs from proxy-routes.json backup.
pub fn restore_opencode_proxy_routes() -> bool {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };
    let cfg_path = if cfg!(target_os = "windows") {
        std::env::var("APPDATA").ok()
            .map(|d| PathBuf::from(d).join("opencode").join("opencode.json"))
    } else if cfg!(target_os = "macos") {
        Some(home.join("Library/Application Support/opencode/opencode.json"))
    } else {
        Some(home.join(".config/opencode/opencode.json"))
    };
    let cfg_path = match cfg_path {
        Some(p) => p,
        None => return false,
    };
    if !cfg_path.exists() { return false; }

    let routes_path = home.join(".reliary/proxy-routes.json");
    let routes_content = match std::fs::read_to_string(&routes_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let routes: Value = match serde_json::from_str(&routes_content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let backups = match routes.get("__backups").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return false,
    };
    let backups: HashMap<String, String> = match serde_json::from_str(backups) {
        Ok(m) => m,
        Err(_) => return false,
    };

    let content = match std::fs::read_to_string(&cfg_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut v: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let mut restored = 0;
    if let Some(obj) = v.as_object_mut() {
        if let Some(providers) = obj.get_mut("provider").and_then(|p| p.as_object_mut()) {
            for (name, provider) in providers.iter_mut() {
                if let Some(original_url) = backups.get(name) {
                    if let Some(options) = provider.get_mut("options").and_then(|o| o.as_object_mut()) {
                        options.insert("baseURL".to_string(), Value::String(original_url.clone()));
                        restored += 1;
                    }
                }
            }
        }
    }

    if restored > 0 {
        if let Ok(new_content) = serde_json::to_string_pretty(&v) {
            atomic_write(&cfg_path.to_string_lossy(), &new_content);
        }
    }
    restored > 0
}
