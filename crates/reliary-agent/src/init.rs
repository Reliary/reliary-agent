use std::io::{self, Write};
use std::path::PathBuf;
use std::fs;
use std::process::Command;
use serde_json::Value;

fn ok(msg: &str) { println!("  \x1b[32m✓\x1b[0m {}", msg); }

// Embed gate.js at compile time

/// Atomic write: write to tmp, sync, rename. Prevents partial write corruption.
/// Bug 47: clean up tmp file on write or rename failure (was leaking).
fn atomic_write(path: &str, content: &str) -> bool {
    let tmp = format!("{}.tmp.{}", path, std::process::id());
    let write_ok = std::fs::write(&tmp, content).is_ok();
    if !write_ok {
        let _ = std::fs::remove_file(&tmp); // clean up partial write
        return false;
    }
    let rename_ok = std::fs::rename(&tmp, path).is_ok();
    if !rename_ok {
        let _ = std::fs::remove_file(&tmp); // clean up orphaned tmp
        return false;
    }
    true
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

/// Print a dry-run action and return true to skip the actual work.
fn dry_run_action(label: &str) -> bool {
    println!("  \x1b[36m[dry-run]\x1b[0m would {}", label);
    true
}

fn home_dir() -> Option<PathBuf> {
    if let Some(p) = test_home_override() { return Some(p); }
    dirs::home_dir()
}

fn test_home_override() -> Option<PathBuf> {
    TEST_HOME_OVERRIDE.with(|cell| cell.borrow().clone())
}

thread_local! {
    static TEST_HOME_OVERRIDE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Test-only: override `home_dir()` for the current thread. Returns a guard
/// that restores the previous value on drop.
#[cfg(test)]
pub(crate) fn set_test_home(path: PathBuf) -> TestHomeGuard {
    let prev = TEST_HOME_OVERRIDE.with(|cell| cell.borrow().clone());
    TEST_HOME_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(path));
    TestHomeGuard { prev }
}

#[cfg(test)]
pub(crate) struct TestHomeGuard {
    prev: Option<PathBuf>,
}

#[cfg(test)]
impl Drop for TestHomeGuard {
    fn drop(&mut self) {
        TEST_HOME_OVERRIDE.with(|cell| *cell.borrow_mut() = self.prev.clone());
    }
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

pub fn run(dry_run: bool) {
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
    if dry_run {
        println!("{}  \x1b[36m[dry-run]\x1b[0m{} Showing what would be installed, no changes will be made.", dim, reset);
    }
    println!();

    let mut configured_agents = 0;

    // 1. Pi Agent
    let pi_bin = home_dir().map(|h| h.join(".local/bin/pi")).unwrap_or_else(|| PathBuf::from("pi"));
    let has_pi = pi_bin.exists() || std::env::var("PATH").map(|p| {
        p.split(':').any(|dir| std::path::Path::new(dir).join("pi").exists())
    }).unwrap_or(false);
    
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
        if dry_run { dry_run_action("install Pi Agent gate.js"); } else if ask_yes_no(msg, true) {
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
        let claude_hooks_dir = home.join(".claude/hooks");
        if claude_cfg.exists() {
            if dry_run { dry_run_action("add Reliary MCP server to Claude Code (~/.claude.json)"); } else if ask_yes_no("Found Claude Code config. Add Reliary MCP server?", true) {
                    if inject_mcp_server(&claude_cfg, "reliary", "mcpServers") {
                        ok("Updated ~/.claude.json");
                        configured_agents += 1;
                    } else {
                        println!("  \x1b[31m✗\x1b[0m Failed to update ~/.claude.json\n");
                    }
            } else {
                println!("  \x1b[33m-\x1b[0m Skipped\n");
            }
            // Install code discovery gate hooks
            if dry_run { dry_run_action("install Claude Code hooks (~/.claude/hooks/reliary-*)"); } else if ask_yes_no("Install code discovery gate hooks? (blocks first grep/read, redirects to reliary tools)", true) {
                if install_claude_hooks(&claude_hooks_dir) {
                    ok("Installed Claude Code hooks (~/.claude/hooks/reliary-*)");
                } else {
                    println!("  \x1b[31m✗\x1b[0m Failed to install hooks\n");
                }
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
                if dry_run { dry_run_action("add Reliary MCP server to OpenCode (~/.config/opencode/opencode.json)"); } else if ask_yes_no("Found OpenCode config. Add Reliary MCP server?", true) {
                    if inject_mcp_server(&cfg_path, "reliary", "mcp") {
                        ok("Updated opencode.json");
                        configured_agents += 1;
                    } else {
                        println!("  \x1b[31m✗\x1b[0m Failed to update opencode.json\n");
                    }
                } else {
                    println!("  \x1b[33m-\x1b[0m Skipped\n");
                }

                // Offer to install the OpenCode plugin (regen-on-edit hook)
                if dry_run { dry_run_action("install reliary-opencode plugin in opencode.json"); } else if ask_yes_no("Install reliary-opencode plugin? (auto-reindex + regen pack after every write/edit)", true) {
                    let exe_path = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("reliary"));
                    match inject_opencode_plugin(&cfg_path, &exe_path) {
                        Ok(true) => ok("Installed reliary-opencode plugin in opencode.json"),
                        Ok(false) => println!("  \x1b[33m-\x1b[0m Plugin already present, skipped\n"),
                        Err(e) => println!("  \x1b[31m✗\x1b[0m Plugin install failed: {}\n", e),
                    }
                } else {
                    println!("  \x1b[33m-\x1b[0m Plugin install skipped\n");
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
                if dry_run { dry_run_action("add Reliary MCP server to Cline"); } else if ask_yes_no("Found Cline config. Add Reliary MCP server?", true) {
                    if inject_mcp_server(&cfg_path, "reliary", "mcpServers") {
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
        println!("  {} No agents were configured. You can run `reliary init` again later.", dim);
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
    println!("{}  │   Then: {}reliary-agent trust .{}               │{}", dim, bold, dim, reset);
    println!("{}  ╰────────────────────────────────────────────╯{}", bold, reset);
    println!();
}

/// Install Claude Code hook scripts (PreToolUse gate + SessionStart reminder).
fn install_claude_hooks(hooks_dir: &PathBuf) -> bool {
    if let Err(e) = fs::create_dir_all(hooks_dir) {
        eprintln!("  Failed to create hooks dir: {}", e);
        return false;
    }
    // Embed hook scripts at compile time
    const GATE_SCRIPT: &str = include_str!("../../../hooks/claude-code-gate.sh");
    const REMINDER_SCRIPT: &str = include_str!("../../../hooks/claude-session-reminder.sh");
    // V14: also install the sift pretooluse hook for bash auto-rewrite (RTK parity).
    const SIFT_PRETOOLUSE: &str = include_str!("../../../hooks/claude-pretooluse.sh");
    let gate_path = hooks_dir.join("reliary-code-gate");
    let reminder_path = hooks_dir.join("reliary-session-reminder");
    let sift_path = hooks_dir.join("reliary-sift-pretooluse");
    if let Err(e) = fs::write(&gate_path, GATE_SCRIPT) {
        eprintln!("  Failed to write gate hook: {}", e);
        return false;
    }
    if let Err(e) = fs::write(&reminder_path, REMINDER_SCRIPT) {
        eprintln!("  Failed to write reminder hook: {}", e);
        return false;
    }
    if let Err(e) = fs::write(&sift_path, SIFT_PRETOOLUSE) {
        eprintln!("  Failed to write sift pretooluse hook: {}", e);
        return false;
    }
    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [&gate_path, &reminder_path, &sift_path] {
            if let Ok(meta) = fs::metadata(p) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = fs::set_permissions(p, perms);
            }
        }
    }
    // V14: register sift pretooluse in ~/.claude/settings.json so it fires
    // automatically on Bash tool calls (RTK parity — user doesn't need to set
    // RELIARY_SIFT_BASH=1 manually).
    // V61: register_claude_sift_hook returns false when settings.json is
    // malformed — init must not report success with a dead hook.
    if !register_claude_sift_hook() {
        eprintln!("\u{26A0}\u{FE0F} Failed to register sift hook in ~/.claude/settings.json (malformed JSON?)");
        return false;
    }
    true
}

/// V14: Inject sift pretooluse hook into ~/.claude/settings.json. Idempotent.
fn register_claude_sift_hook() -> bool {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };
    let settings_path = home.join(".claude/settings.json");
    let content = match fs::read_to_string(&settings_path) {
        Ok(c) => c,
        Err(_) => {
            // File doesn't exist — create with just the hooks.
            let initial = serde_json::json!({
                "hooks": {
                    "PreToolUse": [{
                        "matcher": "Bash",
                        "hooks": [{
                            "type": "command",
                            "command": "~/.claude/hooks/reliary-sift-pretooluse"
                        }]
                    }]
                }
            });
            return atomic_write(&settings_path.to_string_lossy(), &initial.to_string());
        }
    };
    let mut v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let sift_hook_exists = v.get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter().any(|entry| {
                entry.get("matcher").and_then(|m| m.as_str()) == Some("Bash")
                    && entry.get("hooks").and_then(|h| h.as_array())
                        .map(|hooks| {
                            hooks.iter().any(|h| {
                                h.get("command").and_then(|c| c.as_str())
                                    .map(|c| c.contains("reliary-sift-pretooluse"))
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if sift_hook_exists { return true; }

    if let Some(obj) = v.as_object_mut() {
        let hooks = obj.entry("hooks").or_insert(serde_json::json!({}));
        if let Some(hooks_obj) = hooks.as_object_mut() {
            let pretooluse = hooks_obj.entry("PreToolUse").or_insert(serde_json::json!([]));
            if let Some(arr) = pretooluse.as_array_mut() {
                arr.push(serde_json::json!({
                    "matcher": "Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "~/.claude/hooks/reliary-sift-pretooluse"
                    }]
                }));
            }
        }
        if let Ok(new_content) = serde_json::to_string_pretty(&v) {
            return atomic_write(&settings_path.to_string_lossy(), &new_content);
        }
    }
    false
}

fn inject_mcp_server(cfg_path: &PathBuf, server_name: &str, mcp_key: &str) -> bool {
    let content = match fs::read_to_string(cfg_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let mut v: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };

    if let Some(obj) = v.as_object_mut() {
        let mcp_servers = obj.entry(mcp_key).or_insert(serde_json::json!({}));
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

/// Inject the reliary-opencode-plugin entry into opencode.json's "plugin" array.
///
/// Locates the plugin source relative to the running binary (sibling directory at
/// `<install_root>/../../opencode-plugin`), checks it's built (dist/index.js exists),
/// and appends the absolute path to the config's "plugin" array. Preserves existing
/// entries; removes any prior `@reliary/opencode` (the deprecated v0.x plugin) entry.
///
/// Returns:
///   - Ok(true) if the plugin was added (or already present)
///   - Ok(false) if the user declined the prompt or the plugin source wasn't found
///   - Err on filesystem/parse failure
fn inject_opencode_plugin(cfg_path: &PathBuf, exe_path: &std::path::Path) -> Result<bool, String> {
    // Locate the plugin dist/. Binary layout:
    //   <repo>/target/release/reliary
    //   <repo>/opencode-plugin/dist/index.js
    // The exe is at `<repo>/target/release/reliary`; ancestors[3] gives `<repo>`.
    let repo_root = exe_path
        .ancestors()
        .nth(3)
        .ok_or("cannot determine repo root")?;
    let plugin_src = repo_root.join("opencode-plugin");
    let plugin_dist = plugin_src.join("dist").join("index.js");

    if !plugin_dist.exists() {
        return Err(format!(
            "plugin not built: {}\n  Run `cd {} && npm install && npm run build` to build it.",
            plugin_dist.display(),
            plugin_src.display()
        ));
    }
    let plugin_path = plugin_dist.canonicalize()
        .map_err(|e| format!("canonicalize {}: {}", plugin_dist.display(), e))?
        .to_string_lossy().to_string();

    let content = fs::read_to_string(cfg_path)
        .map_err(|e| format!("read {}: {}", cfg_path.display(), e))?;
    let mut v: Value = serde_json::from_str(&content)
        .map_err(|e| format!("parse {}: {}", cfg_path.display(), e))?;

    if let Some(obj) = v.as_object_mut() {
        let plugin_arr = obj.entry("plugin").or_insert(serde_json::json!([]));
        if let Some(arr) = plugin_arr.as_array_mut() {
            // Drop any deprecated entry (the old `@reliary/opencode` v0.x plugin)
            arr.retain(|v| {
                let s = v.as_str().unwrap_or("");
                !s.contains("@reliary/opencode") || s.ends_with("opencode-plugin/dist/index.js")
            });
            // Idempotent: only add if not present
            let already = arr.iter().any(|v| {
                v.as_str().map(|s| s == plugin_path).unwrap_or(false)
            });
            if !already {
                arr.push(serde_json::Value::String(plugin_path.clone()));
            }
            if let Ok(new_content) = serde_json::to_string_pretty(&v) {
                if atomic_write(&cfg_path.to_string_lossy(), &new_content) {
                    return Ok(true);
                }
                return Err(format!("failed to write {}", cfg_path.display()));
            }
        }
    }
    Ok(false)
}

pub fn uninstall() {
    println!("\nReliary Uninstall");
    println!("-----------------");

    let mut removed_agents = 0;

    // 1. Pi Agent
    // Detect Pi by file existence only (no `pi --version` exec which can hang
    // on stdin or fail with wrong exit code).
    let pi_bin = home_dir().map(|h| h.join(".local/bin/pi")).unwrap_or_else(|| PathBuf::from("pi"));
    let pi_in_path = std::env::var("PATH").map(|p| {
        p.split(':').any(|dir| std::path::Path::new(dir).join("pi").exists())
    }).unwrap_or(false);
    let has_pi = pi_bin.exists() || pi_in_path;
    
    if has_pi {
        println!("Removing Pi Agent extension...");
        if let Some(data_dir) = get_data_dir() {
            let target_dir = data_dir.join("reliary");
            let target_path = target_dir.join("gate.js");
            
            if target_path.exists() {
                // Use 'pi -e' to remove the extension, or fall back to direct file removal.
                // The 'pi uninstall' subcommand may not exist in all Pi versions, so we
                // try the Pi command but always remove the file regardless of its result.
                let pi_cmd = if pi_bin.exists() { pi_bin.to_str().unwrap_or("pi") } else { "pi" };
                // Use proper command args instead of shell command to avoid quoting issues
                let _ = Command::new(pi_cmd)
                    .arg("-e")
                    .arg(format!("rm {}", target_path.to_str().unwrap_or("")))
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
        if claude_cfg.exists() && remove_mcp_server(&claude_cfg, "reliary", "mcpServers") {
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
            if cfg_path.exists()
                && remove_mcp_server(&cfg_path, "reliary", "mcp") {
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
            if cfg_path.exists() && remove_mcp_server(&cfg_path, "reliary", "mcpServers") {
                ok("Removed Reliary from Cline");
                removed_agents += 1;
            }
        }
    }

    // 5. Claude Code hooks
    println!("Removing Claude Code hooks...");
    if let Some(home) = home_dir() {
        // V61: also strip the PreToolUse entry from ~/.claude/settings.json —
        // uninstall only removed the hook FILES before, leaving a broken
        // command reference that fires on every Bash call.
        remove_claude_sift_hook(&home);
        let hooks_dir = home.join(".claude/hooks");
        if hooks_dir.exists() {
            let gate_path = hooks_dir.join("reliary-code-gate");
            let reminder_path = hooks_dir.join("reliary-session-reminder");
            let sift_path = hooks_dir.join("reliary-sift-pretooluse");
            let mut hooks_removed = 0;
            for path in [gate_path, reminder_path, sift_path] {
                if path.exists() {
                    let _ = fs::remove_file(&path);
                    hooks_removed += 1;
                }
            }
            if hooks_removed > 0 {
                ok(&format!("Removed {} Claude Code hook(s)", hooks_removed));
                removed_agents += 1;
            }
        }
    }

    if removed_agents == 0 {
        println!("- No MCP integrations found or modified");
    }
    println!();

    // 5. Config
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

/// V61: strip the sift PreToolUse entry from ~/.claude/settings.json.
/// Mirror of register_claude_sift_hook — uninstall must remove what
/// install created (it previously only deleted the hook files, leaving a
/// broken command reference that fired on every Bash tool call).
fn remove_claude_sift_hook(home: &std::path::Path) {
    let settings_path = home.join(".claude/settings.json");
    if !settings_path.exists() { return; }
    let content = match fs::read_to_string(&settings_path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return,
    };
    let removed = v.get_mut("hooks")
        .and_then(|h| h.get_mut("PreToolUse"))
        .and_then(|p| p.as_array_mut())
        .map(|arr| {
            let before = arr.len();
            arr.retain(|entry| {
                !entry.get("hooks").and_then(|h| h.as_array())
                    .map(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command").and_then(|c| c.as_str())
                                .map(|c| c.contains("reliary-sift-pretooluse"))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            });
            arr.len() != before
        })
        .unwrap_or(false);
    if !removed { return; }
    // Drop empty PreToolUse array to keep settings clean.
    if let Some(arr) = v.get("hooks").and_then(|h| h.get("PreToolUse")).and_then(|p| p.as_array()) {
        if arr.is_empty() {
            if let Some(hooks) = v.get_mut("hooks").and_then(|h| h.as_object_mut()) {
                hooks.remove("PreToolUse");
            }
        }
    }
    if let Ok(new_content) = serde_json::to_string_pretty(&v) {
        let _ = atomic_write(&settings_path.to_string_lossy(), &new_content);
    }
}

fn remove_mcp_server(cfg_path: &PathBuf, server_name: &str, mcp_key: &str) -> bool {
    let content = match fs::read_to_string(cfg_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    
    let mut v: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };

    if let Some(obj) = v.as_object_mut() {
        if let Some(mcp_servers) = obj.get_mut(mcp_key).and_then(|m| m.as_object_mut()) {
            if mcp_servers.remove(server_name).is_some() {
                if let Ok(new_content) = serde_json::to_string_pretty(&v) {
                    return atomic_write(&cfg_path.to_string_lossy(), &new_content);
                }
            }
        }
    }
    false
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
        let _lock = INIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());  // GUARDED: intentional — test serialization lock, must hold across HOME mutation
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("reliary_init_test_{}_{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::create_dir_all(dir.join(".reliary"));  // GUARDED: intentional — test-only code
        let _home_guard = set_test_home(dir.clone());

        // Clear RELIARY_* env vars to avoid interference
        let old_pi_key = std::env::var("OPENAI_API_KEY").ok();  // GUARDED: intentional
        let old_anthro_key = std::env::var("ANTHROPIC_API_KEY").ok();  // GUARDED: intentional
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");

        test(dir.clone());

        // Restore env
        if let Some(k) = old_pi_key { std::env::set_var("OPENAI_API_KEY", k); }
        if let Some(k) = old_anthro_key { std::env::set_var("ANTHROPIC_API_KEY", k); }
        let _ = std::fs::remove_dir_all(&dir);  // GUARDED: intentional — test code, lock held across I/O
    }
    #[test]
    fn test_inject_mcp_server_stdio() {
        with_temp_home(|home| {
            let cfg_path = home.join("test_mcp_config.json");
            std::fs::write(&cfg_path, r#"{}"#).unwrap();

            let result = inject_mcp_server(&cfg_path, "reliary_test", "mcp");
            assert!(result, "should inject MCP server entry");

            let content = std::fs::read_to_string(&cfg_path).unwrap();
            let v: Value = serde_json::from_str(&content).unwrap();
            let servers = v.get("mcp").and_then(|m| m.as_object()).unwrap();
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
            std::fs::write(&cfg_path, r#"{"mcp":{"reliary_test":{"command":"/bin/reliary-agent","args":["mcp"]}}}"#).unwrap();

            let result = remove_mcp_server(&cfg_path, "reliary_test", "mcp");
            assert!(result, "should remove MCP server entry");

            let content = std::fs::read_to_string(&cfg_path).unwrap();
            assert!(!content.contains("reliary_test"), "should no longer contain the server");
        });
    }

    #[test]
    fn test_inject_mcp_server_existing_servers() {
        with_temp_home(|home| {
            let cfg_path = home.join("test_existing_config.json");
            std::fs::write(&cfg_path, r#"{"mcp":{"existing":{"command":"/bin/old"}}}"#).unwrap();

            inject_mcp_server(&cfg_path, "reliary_new", "mcp");
            let content = std::fs::read_to_string(&cfg_path).unwrap();
            let v: Value = serde_json::from_str(&content).unwrap();
            let servers = v.get("mcp").and_then(|m| m.as_object()).unwrap();
            assert!(servers.contains_key("existing"), "existing server should survive");
            assert!(servers.contains_key("reliary_new"), "new server should be added");
        });
    }
}
