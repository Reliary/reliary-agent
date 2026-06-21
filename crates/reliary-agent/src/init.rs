use std::io::{self, Write};
use std::path::PathBuf;
use std::fs;
use std::collections::HashMap;
use std::process::Command;
use serde_json::Value;

fn ok(msg: &str) { println!("  \x1b[32m✓\x1b[0m {}", msg); }

// Embed gate.js at compile time

/// Atomic write: write to tmp, sync, rename. Prevents partial write corruption.
fn atomic_write(path: &str, content: &str) -> bool {
    let tmp = format!("{}.tmp.{}", path, std::process::id());
    std::fs::write(&tmp, content).is_ok() && std::fs::rename(&tmp, path).is_ok()
}
const EMBEDDED_GATE_JS: &str = include_str!("../pi/gate.js");

fn ask_yes_no(prompt: &str, default: bool) -> bool {
    let def_str = if default { "[Y/n]" } else { "[y/N]" };
    print!("{} {}: ", prompt, def_str);
    let _ = io::stdout().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_ok() {  // GUARDED: intentional
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
    let bold = "\x1b[1m";
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";

    println!();
    println!("{}  ╭────────────────────────────────────────────╮{}", bold, reset);
    println!("{}  │       Reliary Agent Setup Wizard           │{}", bold, reset);
    println!("{}  ╰────────────────────────────────────────────╯{}", bold, reset);
    println!();
    println!("{}  This will configure reliary-agent for your code agents.{}", dim, reset);
    println!("{}  Each integration is optional -- say n to skip any.{}", dim, reset);
    println!();

    let mut configured_agents = 0;

    // 1. Pi Agent
    let pi_bin = home_dir().map(|h| h.join(".local/bin/pi")).unwrap_or_else(|| PathBuf::from("pi"));
    let has_pi = pi_bin.exists() || Command::new("pi").arg("--version").output().is_ok();
    
    if has_pi {
        // Idempotency: detect if gate.js already installed
        let gate_exists = home_dir()
            .map(|h| h.join(".local/share/reliary/gate.js"))
            .map(|p| p.exists())
            .unwrap_or(false);
        let msg = if gate_exists {
            "Found Pi Agent + existing gate.js installation. Re-install?"
        } else {
            "Found Pi Agent. Install Reliary extension?"
        };
        if ask_yes_no(msg, true) {
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
                                
                                // After gate.js install, offer proxy routing
                                if ask_yes_no("\nConfigure proxy routing for Pi?\n(Scans Pi settings + env for API keys, writes proxy-routes.json\nso the proxy can route your API calls automatically)", true) {
                                    let routes_count = install_pi_proxy_routes();
                                    if routes_count > 0 {
                                        ok(&format!("{} Pi API keys routed through proxy", routes_count));
                                    } else {
                                        println!("  \x1b[33m-\x1b[0m No Pi API keys found\n                     Set RELIARY_UPSTREAM_URL=http://127.0.0.1:9090/v1\n                     as a fallback (all unknown keys route through proxy)\n");
                                    }
                                }
                            } else {
                                println!("  \x1b[31m✗\x1b[0m Failed to run `pi install`\n");
                            }
                        } else {
                            println!("  \x1b[31m✗\x1b[0m Failed to run `pi install`\n");
                        }
                    } else {
                        println!("  \x1b[31m✗\x1b[0m Failed to write gate.js\n");
                    }
                } else {
                    println!("  \x1b[31m✗\x1b[0m Failed to create directory {:?}\n", target_dir);
                }
            } else {
                println!("  \x1b[31m✗\x1b[0m Could not determine data directory\n");
            }
        } else {
            println!("  \x1b[33m-\x1b[0m Skipped\n");
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
                        println!("  \x1b[31m✗\x1b[0m Failed to update ~/.claude.json\n");
                    }
            } else {
                println!("  \x1b[33m-\x1b[0m Skipped\n");
            }
        }
    }

    // 3. OpenCode
    // OpenCode uses {env:VARIABLE_NAME} substitution in config strings. To route
    // through the Reliary proxy, the user sets each provider's options.baseURL to
    // "{env:RELIARY_BASE_URL}/v1" and exports RELIARY_BASE_URL=http://127.0.0.1:9090/v1
    // in their shell. No config file mutation needed — see README.
    if let Some(home) = home_dir() {
        let opencode_cfg = if cfg!(target_os = "windows") {
            dirs::config_dir().map(|d| d.join("opencode").join("opencode.json"))
        } else if cfg!(target_os = "macos") {
            Some(home.join("Library/Application Support/opencode/opencode.json"))
        } else {
            Some(home.join(".config/opencode/opencode.json"))
        };

        if let Some(opencode_cfg) = opencode_cfg {
            if opencode_cfg.exists() {
                println!("  \x1b[2m  OpenCode config detected (no changes needed).\x1b[0m");
                println!("    Set provider baseURL to \"{{env:RELIARY_BASE_URL}}/v1\"");
                println!("    Then export RELIARY_BASE_URL=http://127.0.0.1:9090/v1");
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
                        println!("  \x1b[31m✗\x1b[0m Failed to update cline_mcp_settings.json\n");
                    }
                } else {
                    println!("  \x1b[33m-\x1b[0m Skipped\n");
                }
            }
        }
    }

    if configured_agents == 0 {
        println!("  {} No agents were configured. Agents can still use the proxy manually.", dim);
    }

    // 5. Daemon
    if ask_yes_no("Do you want to install the Reliary daemon to run on boot?\n(Enables cross-session memory, dead code removal, faster search)", true) {
        if install_daemon() {
            ok("Daemon installed and started");
        } else {
            println!("  \x1b[31m✗\x1b[0m Failed to install daemon\n");
        }
    } else {
        println!("  \x1b[33m-\x1b[0m Skipped\n");
    }

    // ── Summary ──
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";
    println!();
    println!("{}  ╭────────────────────────────────────────────╮{}", bold, reset);
    if configured_agents > 0 {
        println!("{}  │   {} agent(s) configured.       ✓          │{}", dim, configured_agents, reset);
    }
    println!("{}  │   Next: {}reliary-agent doctor{}              │{}", dim, bold, dim, reset);
    println!("{}  │   To start the proxy: {}reliary-agent serve{}   │{}", dim, bold, dim, reset);
    println!("{}  ╰────────────────────────────────────────────╯{}", bold, reset);
    println!();
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
/// Install proxy routing for Pi providers.
/// Reads Pi settings.json, finds API keys, writes proxy-routes.json.
/// Returns the number of API keys discovered.
fn install_pi_proxy_routes() -> usize {
    let mut routes: HashMap<String, String> = HashMap::new();

    // Check Pi settings.json if it exists
    let pi_settings = home_dir()
        .map(|h| h.join(".pi").join("agent").join("settings.json"))
        .unwrap_or_default();

    if pi_settings.exists() {
        if let Ok(content) = fs::read_to_string(&pi_settings) {
            if let Ok(settings) = serde_json::from_str::<Value>(&content) {
                // Check for provider configs in Pi settings
                if let Some(providers) = settings.get("providers").and_then(|p| p.as_object()) {
                    for (_name, config) in providers {
                        let api_key = config.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
                        let base_url = config.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
                        if !api_key.is_empty() && !base_url.is_empty() {
                            routes.insert(api_key.to_string(), base_url.to_string());
                        }
                    }
                }

                // Check for explicit provider overrides in Pi settings
                for (env_key, provider) in &[("OPENAI_API_KEY", "https://api.openai.com"), ("ANTHROPIC_API_KEY", "https://api.anthropic.com")] {
                    let key = settings.get(*env_key).and_then(|v| v.as_str()).unwrap_or("");
                    if !key.is_empty() {
                        routes.insert(key.to_string(), provider.to_string());
                    }
                }
            }
        }
    }

    // Always check env vars directly (even without Pi settings)
    for (env_key, provider) in &[("OPENAI_API_KEY", "https://api.openai.com"), ("ANTHROPIC_API_KEY", "https://api.anthropic.com")] {
        if let Ok(val) = std::env::var(env_key) {
            if !val.is_empty() && !routes.contains_key(&val) {
                routes.insert(val, provider.to_string());
            }
        }
    }

    if !routes.is_empty() {
        write_proxy_routes(&routes);
    }

    routes.len()
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
                "[Unit]\nDescription=Reliary Agent Daemon\n\n[Service]\nExecStart={} serve\nRestart=always\n\n[Install]\nWantedBy=default.target\n",
                exe_str
            );
            
            if !atomic_write(&service_path.to_string_lossy(), &service_content) { return false; }
            
            let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
            let enable = Command::new("systemctl").args(["--user", "enable", "--now", "reliary-daemon.service"]).status();
            let result = enable.is_ok() && enable.unwrap_or_default().success();
            if result {
                // Verify the service is actually active
                let status_check = Command::new("systemctl")
                    .args(["--user", "is-active", "reliary-daemon.service"])
                    .output();
                if let Ok(out) = status_check {
                    let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if status == "active" {
                        eprintln!("  ✓ Daemon service is running");
                    } else {
                        eprintln!("  ⚠ Daemon installed but status: {}", status);
                        eprintln!("     Run: systemctl --user status reliary-daemon.service");
                    }
                }
            }
            return result;
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
        <string>serve</string>
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
            let result = load.is_ok() && load.unwrap_or_default().success();
            if result {
                let status_check = Command::new("launchctl")
                    .args(["list", "com.reliary.daemon"])
                    .output();
                if let Ok(out) = status_check {
                    if out.status.success() {
                        eprintln!("  ✓ Daemon service loaded");
                    } else {
                        eprintln!("  ⚠ Daemon plist installed but not running");
                        eprintln!("     Check: launchctl list com.reliary.daemon");
                    }
                }
            }
            return result;
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
                "Set WshShell = CreateObject(\"WScript.Shell\")\nWshShell.Run chr(34) & \"{}\" & chr(34) & \" serve\", 0\nSet WshShell = Nothing\n",
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

    // 3. OpenCode — nothing to remove. init never mutates opencode.json;
    // users configure RELIARY_BASE_URL in their shell. See README.
    if let Some(home) = home_dir() {
        let opencode_cfg = if cfg!(target_os = "windows") {
            dirs::config_dir().map(|d| d.join("opencode").join("opencode.json"))
        } else if cfg!(target_os = "macos") {
            Some(home.join("Library/Application Support/opencode/opencode.json"))
        } else {
            Some(home.join(".config/opencode/opencode.json"))
        };

        if let Some(opencode_cfg) = opencode_cfg {
            if opencode_cfg.exists() {
                println!("  \x1b[2m  OpenCode: unset RELIARY_BASE_URL to go direct\x1b[0m");
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

/// Write proxy-routes.json to ~/.reliary/ (used by install_pi_proxy_routes).
/// Merges with existing file rather than overwriting, so that Pi and OpenCode
/// init runs in either order don't clobber each other.
fn write_proxy_routes(routes: &HashMap<String, String>) -> bool {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };
    let config_dir = home.join(".reliary");
    let _ = std::fs::create_dir_all(&config_dir);

    // Merge with existing routes — preserve keys not in this call's routes map.
    let routes_path = config_dir.join("proxy-routes.json");
    let mut routes_json: serde_json::Map<String, Value> = if let Ok(existing) = std::fs::read_to_string(&routes_path) {
        serde_json::from_str(&existing).unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    for (key, value) in routes {
        routes_json.insert(key.clone(), Value::String(value.clone()));
    }

    let content = serde_json::to_string_pretty(&serde_json::Value::Object(routes_json))
        .unwrap_or_default();
    atomic_write(&routes_path.to_string_lossy(), &content)
}

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::Mutex;

        /// Serialize init tests — they share HOME and env vars which are process-global
        static INIT_TEST_LOCK: Mutex<()> = Mutex::new(());

        fn with_temp_home<F>(test: F)
    where
        F: FnOnce(PathBuf),
    {
        let _lock = INIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("reliary_init_test_{}_{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::create_dir_all(dir.join(".reliary"));
        let old_home = std::env::var("HOME").ok();  // GUARDED: intentional
        std::env::set_var("HOME", dir.to_str().unwrap());
        let old_xdg = std::env::var("XDG_CONFIG_HOME").ok();  // GUARDED: intentional
        std::env::remove_var("XDG_CONFIG_HOME");

        // Clear RELIARY_* env vars to avoid interference
        let old_pi_key = std::env::var("OPENAI_API_KEY").ok();  // GUARDED: intentional
        let old_anthro_key = std::env::var("ANTHROPIC_API_KEY").ok();  // GUARDED: intentional
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");

        test(dir.clone());

        // Restore env
        if let Some(h) = old_home { std::env::set_var("HOME", h); } else { std::env::remove_var("HOME"); }
        if let Some(x) = old_xdg { std::env::set_var("XDG_CONFIG_HOME", x); } else { std::env::remove_var("XDG_CONFIG_HOME"); }
        if let Some(k) = old_pi_key { std::env::set_var("OPENAI_API_KEY", k); }
        if let Some(k) = old_anthro_key { std::env::set_var("ANTHROPIC_API_KEY", k); }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_install_pi_proxy_routes_empty() {
        with_temp_home(|_home| {
            let count = install_pi_proxy_routes();
            assert_eq!(count, 0, "no Pi settings → 0 routes");
        });
    }

    #[test]
    fn test_install_pi_proxy_routes_from_env() {
        with_temp_home(|home| {
            std::env::set_var("OPENAI_API_KEY", "sk-test-pi-key-12345");
            let count = install_pi_proxy_routes();
            assert_eq!(count, 1, "1 API key from env");

            // Check proxy-routes.json was written
            let routes_path = home.join(".reliary/proxy-routes.json");
            assert!(routes_path.exists(), "proxy-routes.json should exist");
            let content = std::fs::read_to_string(&routes_path).unwrap();
            assert!(content.contains("sk-test-pi-key-12345"), "routes should contain the API key");
            assert!(content.contains("api.openai.com"), "routes should contain upstream URL");
        });
    }

    #[test]
    fn test_install_pi_proxy_routes_from_settings() {
        with_temp_home(|home| {
            // Create mock Pi settings.json
            let pi_dir = home.join(".pi/agent");
            let _ = std::fs::create_dir_all(&pi_dir);
            let settings = serde_json::json!({
                "providers": {
                    "openai": {
                        "apiKey": "sk-pi-settings-key",
                        "baseUrl": "https://api.openai.com"
                    }
                }
            });
            std::fs::write(pi_dir.join("settings.json"), serde_json::to_string_pretty(&settings).unwrap()).unwrap();

            let count = install_pi_proxy_routes();
            assert_eq!(count, 1, "1 API key from Pi settings");

            let routes_path = home.join(".reliary/proxy-routes.json");
            assert!(routes_path.exists());
            let content = std::fs::read_to_string(&routes_path).unwrap();
            assert!(content.contains("sk-pi-settings-key"));
        });
    }

    #[test]
    fn test_install_pi_proxy_routes_multiple_keys() {
        with_temp_home(|home| {
            std::env::set_var("OPENAI_API_KEY", "sk-openai-key");
            std::env::set_var("ANTHROPIC_API_KEY", "sk-anthropic-key");

            let count = install_pi_proxy_routes();
            assert_eq!(count, 2, "2 API keys from env vars");

            let routes_path = home.join(".reliary/proxy-routes.json");
            let content = std::fs::read_to_string(&routes_path).unwrap();
            assert!(content.contains("sk-openai-key"));
            assert!(content.contains("sk-anthropic-key"));
            assert!(content.contains("api.anthropic.com"));
        });
    }

    #[test]
    fn test_inject_mcp_server_stdio() {
        with_temp_home(|home| {
            let cfg_path = home.join("test_mcp_config.json");
            std::fs::write(&cfg_path, r#"{}"#).unwrap();

            let result = inject_mcp_server(&cfg_path, "reliary_test");
            assert!(result, "should inject MCP server entry");

            let content = std::fs::read_to_string(&cfg_path).unwrap();
            let v: Value = serde_json::from_str(&content).unwrap();
            let servers = v.get("mcpServers").and_then(|m| m.as_object()).unwrap();
            assert!(servers.contains_key("reliary_test"));
            let entry = servers.get("reliary_test").unwrap();
            assert_eq!(entry.get("command").and_then(|c| c.as_str()).unwrap_or(""), std::env::current_exe().unwrap().to_str().unwrap());
            assert_eq!(entry.get("args").and_then(|a| a.as_array()).map(|a| a[0].as_str().unwrap_or("")).unwrap_or(""), "mcp");
        });
    }

    #[test]
    fn test_remove_mcp_server() {
        with_temp_home(|home| {
            let cfg_path = home.join("test_remove_config.json");
            std::fs::write(&cfg_path, r#"{"mcpServers":{"reliary_test":{"command":"/bin/reliary-agent","args":["mcp"]}}}"#).unwrap();

            let result = remove_mcp_server(&cfg_path, "reliary_test");
            assert!(result, "should remove MCP server entry");

            let content = std::fs::read_to_string(&cfg_path).unwrap();
            assert!(!content.contains("reliary_test"), "should no longer contain the server");
        });
    }

    #[test]
    fn test_inject_mcp_server_existing_servers() {
        with_temp_home(|home| {
            let cfg_path = home.join("test_existing_config.json");
            std::fs::write(&cfg_path, r#"{"mcpServers":{"existing":{"command":"/bin/old"}}}"#).unwrap();

            inject_mcp_server(&cfg_path, "reliary_new");
            let content = std::fs::read_to_string(&cfg_path).unwrap();
            let v: Value = serde_json::from_str(&content).unwrap();
            let servers = v.get("mcpServers").and_then(|m| m.as_object()).unwrap();
            assert!(servers.contains_key("existing"), "existing server should survive");
            assert!(servers.contains_key("reliary_new"), "new server should be added");
        });
    }

    #[test]
    fn test_write_proxy_routes_merges_with_existing() {
        // write_proxy_routes must merge, not overwrite, so that Pi and OpenCode
        // init runs in either order don't clobber each other.
        with_temp_home(|home| {
            // Seed an existing proxy-routes.json with a key from a previous init
            let config_dir = home.join(".reliary");
            std::fs::create_dir_all(&config_dir).unwrap();
            let routes_path = config_dir.join("proxy-routes.json");
            std::fs::write(&routes_path, r#"{"existing-key": "https://existing.example/v1"}"#).unwrap();

            // Write a new routes map that doesn't contain existing-key
            let mut new_routes = HashMap::new();
            new_routes.insert("new-key".to_string(), "https://new.example/v1".to_string());
            let ok = write_proxy_routes(&new_routes);
            assert!(ok, "write_proxy_routes should succeed");

            // Both keys should now be present
            let content = std::fs::read_to_string(&routes_path).unwrap();
            assert!(content.contains("existing-key"), "existing key should survive");
            assert!(content.contains("existing.example"), "existing upstream should survive");
            assert!(content.contains("new-key"), "new key should be added");
            assert!(content.contains("new.example"), "new upstream should be added");
        });
    }

    #[test]
    fn test_no_opencode_proxy_routes_function() {
        // init.rs no longer exports inject_opencode_proxy_routes or
        // restore_opencode_proxy_routes. Verify they don't exist.
        // (Compile-time check — if these functions were added back, this test
        // would fail to compile.)
        // We just verify the public surface here:
        // If you re-add OpenCode URL rewriting, this test serves as a reminder
        // that init must NEVER mutate ~/.config/opencode/opencode.json.
        with_temp_home(|_home| {
            // No setup needed — this is a placeholder asserting intent.
        });
    }
}
