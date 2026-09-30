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

/// The built OpenCode plugin (tsup output) embedded at compile time, the same
/// way gate.js is. It is written to the data dir on `init` so the plugin works
/// from a cargo/npm/tarball install where no source tree exists. CI keeps this
/// copy byte-identical to `opencode-plugin/dist/index.js`.
const EMBEDDED_OPENCODE_PLUGIN: &str = include_str!("../opencode-plugin/index.js");

/// Filename the OpenCode plugin is written under in the reliary data dir.
const OPENCODE_PLUGIN_FILENAME: &str = "opencode-plugin.js";

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

/// The Cline global storage settings file.
///
/// Cline's extension id changed from `rooveterinery.cline` to
/// `saoudrizwan.claude-dev`; we prefer the current id and fall back to the
/// legacy one so installs of either generation are found.
fn cline_config_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        dirs::data_dir().map(|d| d.join("Code").join("User").join("globalStorage"))
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library/Application Support/Code/User/globalStorage"))
    } else {
        home_dir().map(|h| h.join(".config/Code/User/globalStorage"))
    }?;
    let current = base.join("saoudrizwan.claude-dev").join("cline_mcp_settings.json");
    if current.exists() {
        return Some(current);
    }
    let legacy = base.join("rooveterinery.cline").join("cline_mcp_settings.json");
    if legacy.exists() {
        return Some(legacy);
    }
    Some(current)
}

/// Directory OpenCode reads global config from, per platform.
fn opencode_config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        dirs::config_dir().map(|d| d.join("opencode"))
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library/Application Support/opencode"))
    } else {
        home_dir().map(|h| h.join(".config/opencode"))
    }
}

/// The OpenCode global config file to use.
///
/// OpenCode merges `opencode.jsonc`, `opencode.json`, and `config.json` (it
/// prefers `.jsonc` when present). We return the highest-precedence file that
/// exists so we edit the one the user actually runs; if none exist we return
/// the `opencode.json` path so the caller can report "not found" rather than
/// silently skipping a `.jsonc`-only setup.
fn opencode_config_path() -> Option<PathBuf> {
    let dir = opencode_config_dir()?;
    for name in ["opencode.jsonc", "opencode.json", "config.json"] {
        let p = dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    Some(dir.join("opencode.json"))
}

/// Path of the short-alias shim that the `--help` text advertises.
fn rel_shim_path() -> Option<PathBuf> {
    home_dir().map(|h| {
        h.join(".local/bin")
            .join(if cfg!(windows) { "rel.cmd" } else { "rel" })
    })
}

/// Install the `rel` short alias so the hint in `--help` is true rather than
/// aspirational. A symlink to the running binary on Unix; a tiny `.cmd`
/// wrapper on Windows (non-symlink is the safe default there). Best-effort:
/// a failure is reported but never aborts `init`.
fn install_rel_shim(dry_run: bool) -> bool {
    let Some(shim) = rel_shim_path() else { return false };
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("reliary"));

    if dry_run {
        dry_run_action(&format!("install 'rel' shim → {}", shim.display()));
        return true;
    }

    let Some(parent) = shim.parent() else { return false };
    if fs::create_dir_all(parent).is_err() {
        return false;
    }
    // Replace any existing shim so a stale or wrong target cannot survive.
    let _ = fs::remove_file(&shim);

    #[cfg(unix)]
    let created = std::os::unix::fs::symlink(&exe, &shim).is_ok();

    #[cfg(windows)]
    let created = fs::write(&shim, format!("@echo off\r\n\"{}\" %*\r\n", exe.display())).is_ok();

    if created {
        ok(&format!("Installed 'rel' alias → {}", shim.display()));
        if !path_contains(parent) {
            println!(
                "  \x1b[33m!\x1b[0m {} is not on your PATH — add it to use 'rel'",
                parent.display()
            );
        }
    }
    created
}

/// Whether `dir` appears in `PATH`.
fn path_contains(dir: &std::path::Path) -> bool {
    std::env::var("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d == dir))
        .unwrap_or(false)
}

/// Remove the short-alias shim. Symmetric with [`install_rel_shim`].
fn remove_rel_shim() {
    if let Some(shim) = rel_shim_path() {
        // A dangling symlink has no regular-file metadata, so check the link
        // itself too, or we would leave a broken `rel` behind.
        if (shim.exists() || shim.symlink_metadata().is_ok())
            && fs::remove_file(&shim).is_ok()
        {
            ok("Removed 'rel' alias");
        }
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
    println!("{}  This will configure reliary for your code agents.{}", dim, reset);
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
            // Install Claude Code hooks (session reminder + bash sift)
            if dry_run { dry_run_action("install Claude Code hooks (~/.claude/hooks/reliary-*)"); } else if ask_yes_no("Install Claude Code hooks? (session reminder + bash output compression)", true) {
                if install_claude_hooks(&claude_hooks_dir) {
                    ok("Installed Claude Code hooks (~/.claude/hooks/reliary-*)");
                } else {
                    println!("  \x1b[31m✗\x1b[0m Failed to install hooks\n");
                }
            }
        }
    }

    // 3. OpenCode
    if let Some(cfg_path) = opencode_config_path() {
        if cfg_path.exists() {
            let name = cfg_path.file_name().and_then(|s| s.to_str()).unwrap_or("opencode.json");
            if dry_run { dry_run_action(&format!("add Reliary MCP server to OpenCode ({})", name)); } else if ask_yes_no("Found OpenCode config. Add Reliary MCP server?", true) {
                if inject_opencode_mcp_server(&cfg_path, "reliary") {
                    ok(&format!("Updated {}", name));
                    configured_agents += 1;
                } else {
                    println!("  \x1b[31m✗\x1b[0m Failed to update {}\n", name);
                }
            } else {
                println!("  \x1b[33m-\x1b[0m Skipped\n");
            }

            // Offer to install the OpenCode plugin (regen-on-edit hook)
            if dry_run { dry_run_action(&format!("install reliary-opencode plugin in {}", name)); } else if ask_yes_no("Install reliary-opencode plugin? (auto-reindex + regen pack after every write/edit)", true) {
                match inject_opencode_plugin(&cfg_path) {
                    Ok(true) => ok(&format!("Installed reliary-opencode plugin in {}", name)),
                    Ok(false) => println!("  \x1b[33m-\x1b[0m Plugin already present, skipped\n"),
                    Err(e) => println!("  \x1b[31m✗\x1b[0m Plugin install failed: {}\n", e),
                }
            } else {
                println!("  \x1b[33m-\x1b[0m Plugin install skipped\n");
            }
        } else {
            println!("  \x1b[33m-\x1b[0m No OpenCode config found (optional)\n");
        }
    } else {
        println!("  \x1b[33m-\x1b[0m Could not locate OpenCode config dir (optional)\n");
    }
    
    // 4. Cline
    if let Some(cfg_path) = cline_config_path() {
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

    if configured_agents == 0 {
        println!("  {} No agents were configured. You can run `reliary init` again later.", dim);
    }

    // Short alias 'rel', advertised in `reliary --help`.
    if dry_run {
        dry_run_action("install 'rel' shim");
    } else if ask_yes_no("Install the short 'rel' command alias?", true) {
        install_rel_shim(false);
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
    println!("{}  │   Next: {}reliary doctor{}              │{}", dim, bold, dim, reset);
    println!("{}  │   Then: {}reliary trust .{}               │{}", dim, bold, dim, reset);
    println!("{}  ╰────────────────────────────────────────────╯{}", bold, reset);
    println!();
}

/// Install Claude Code hook scripts (SessionStart reminder + Bash sift).
fn install_claude_hooks(hooks_dir: &PathBuf) -> bool {
    if let Err(e) = fs::create_dir_all(hooks_dir) {
        eprintln!("  Failed to create hooks dir: {}", e);
        return false;
    }
    // Embed hook scripts at compile time
    const REMINDER_SCRIPT: &str = include_str!("../hooks/claude-session-reminder.sh");
    // V14: also install the sift pretooluse hook for bash auto-rewrite (RTK parity).
    const SIFT_PRETOOLUSE: &str = include_str!("../hooks/claude-pretooluse.sh");
    let reminder_path = hooks_dir.join("reliary-session-reminder");
    let sift_path = hooks_dir.join("reliary-sift-pretooluse");
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
        for p in [&reminder_path, &sift_path] {
            if let Ok(meta) = fs::metadata(p) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = fs::set_permissions(p, perms);
            }
        }
    }
    // Register both hooks in ~/.claude/settings.json so they fire automatically.
    // V61: register_claude_hooks returns false when settings.json is malformed —
    // init must not report success with a dead hook.
    if !register_claude_hooks() {
        eprintln!("\u{26A0}\u{FE0F} Failed to register hooks in ~/.claude/settings.json (malformed JSON?)");
        return false;
    }
    true
}

/// Shape of one hook entry to register: event, matcher, command.
struct HookSpec {
    event: &'static str,
    matcher: &'static str,
    command: &'static str,
}

const CLAUDE_HOOKS: &[HookSpec] = &[
    HookSpec {
        event: "PreToolUse",
        matcher: "Bash",
        command: "~/.claude/hooks/reliary-sift-pretooluse",
    },
    HookSpec {
        event: "SessionStart",
        matcher: "startup|resume|clear|compact",
        command: "~/.claude/hooks/reliary-session-reminder",
    },
];

/// V14: Inject the reliary hooks into ~/.claude/settings.json. Idempotent.
/// Generalised from the sift-only version so install and uninstall stay
/// symmetric for every entry this function can add.
fn register_claude_hooks() -> bool {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };
    let settings_path = home.join(".claude/settings.json");
    let mut v: serde_json::Value = match fs::read_to_string(&settings_path) {
        Ok(c) => match serde_json::from_str(&c) {
            Ok(v) => v,
            Err(_) => return false,
        },
        Err(_) => {
            // File doesn't exist — create it with just the hooks.
            serde_json::json!({ "hooks": {} })
        }
    };

    let hooks = v
        .as_object_mut()
        .map(|obj| obj.entry("hooks").or_insert(serde_json::json!({})));
    let Some(hooks) = hooks.and_then(|h| h.as_object_mut()) else {
        return false;
    };

    for spec in CLAUDE_HOOKS {
        let already = hooks
            .get(spec.event)
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter().any(|entry| {
                    entry.get("matcher").and_then(|m| m.as_str()) == Some(spec.matcher)
                        && entry
                            .get("hooks")
                            .and_then(|h| h.as_array())
                            .map(|hs| {
                                hs.iter().any(|h| {
                                    h.get("command")
                                        .and_then(|c| c.as_str())
                                        .map(|c| c == spec.command)
                                        .unwrap_or(false)
                                })
                            })
                            .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if already {
            continue;
        }
        let arr = hooks
            .entry(spec.event)
            .or_insert(serde_json::json!([]));
        if let Some(arr) = arr.as_array_mut() {
            arr.push(serde_json::json!({
                "matcher": spec.matcher,
                "hooks": [{ "type": "command", "command": spec.command }]
            }));
        }
    }

    match serde_json::to_string_pretty(&v) {
        Ok(new_content) => atomic_write(&settings_path.to_string_lossy(), &new_content),
        Err(_) => false,
    }
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

/// Write the reliary MCP server into an OpenCode config.
///
/// OpenCode's `mcp` schema is *not* the Claude/Cline shape: a local server is
/// `{ "type": "local", "command": ["<exe>", "<args>..."] }`. The generic
/// [`inject_mcp_server`] writes `{ "command": "<string>", "args": [...] }`,
/// which fails OpenCode's `command: string[]` decode and does not start the
/// server. This is the OpenCode-specific writer.
///
/// Edits through the JSONC CST so an `opencode.jsonc` (OpenCode's documented
/// default) keeps its comments and formatting. `serde_json` alone cannot even
/// parse a `.jsonc` file, so the generic writer would silently fail on it.
fn inject_opencode_mcp_server(cfg_path: &PathBuf, server_name: &str) -> bool {
    use jsonc_parser::cst::{CstInputValue, CstRootNode};

    let Ok(content) = fs::read_to_string(cfg_path) else { return false };
    let Ok(root) = CstRootNode::parse(&content, &jsonc_parser::ParseOptions::default()) else {
        return false;
    };
    let exe_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("reliary"));
    let exe_str = exe_path.to_string_lossy().to_string();

    let obj = root.object_value_or_set();
    let mcp = obj.object_value_or_set("mcp");
    // Replace any existing entry so a reinstall cannot leave the old malformed
    // shape in place (append would leave two `reliary` keys).
    if let Some(existing) = mcp.get(server_name) {
        existing.remove();
    }
    mcp.append(
        server_name,
        CstInputValue::Object(vec![
            ("type".into(), CstInputValue::String("local".into())),
            (
                "command".into(),
                CstInputValue::Array(vec![
                    CstInputValue::String(exe_str),
                    CstInputValue::String("mcp".into()),
                ]),
            ),
            ("enabled".into(), CstInputValue::Bool(true)),
        ]),
    );
    atomic_write(&cfg_path.to_string_lossy(), &root.to_string())
}

/// Absolute path where the embedded OpenCode plugin is written.
fn opencode_plugin_target() -> Option<PathBuf> {
    get_data_dir().map(|d| d.join("reliary").join(OPENCODE_PLUGIN_FILENAME))
}

/// Whether a `plugin` array element refers to this tool's plugin.
fn is_reliary_plugin_entry(el: &jsonc_parser::cst::CstNode) -> bool {
    el.to_serde_value()
        .and_then(|v| v.as_str().map(String::from))
        .map(|s| s.contains("@reliary/opencode") || s.ends_with(OPENCODE_PLUGIN_FILENAME))
        .unwrap_or(false)
}

/// Materialize the embedded OpenCode plugin and register it in the config.
///
/// The plugin is written to the reliary data dir (not referenced from a source
/// tree), so this works for cargo/npm/tarball/brew installs where no
/// `opencode-plugin/` directory exists. Registration is idempotent and drops
/// any prior reliary plugin path (including the deprecated `@reliary/opencode`
/// package entry). Uses a lossless JSONC edit to preserve comments.
fn inject_opencode_plugin(cfg_path: &PathBuf) -> Result<bool, String> {
    use jsonc_parser::cst::{CstInputValue, CstRootNode};

    let target = opencode_plugin_target().ok_or("could not determine data directory")?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {}", parent.display(), e))?;
    }
    if !atomic_write(&target.to_string_lossy(), EMBEDDED_OPENCODE_PLUGIN) {
        return Err(format!("failed to write plugin to {}", target.display()));
    }
    let plugin_path = target.to_string_lossy().to_string();

    let content = fs::read_to_string(cfg_path)
        .map_err(|e| format!("read {}: {}", cfg_path.display(), e))?;
    let root = CstRootNode::parse(&content, &jsonc_parser::ParseOptions::default())
        .map_err(|e| format!("parse {}: {}", cfg_path.display(), e))?;
    let obj = root.object_value_or_set();

    // Drop every existing reliary entry (stale path, or the old npm package)
    // so a reinstall cannot leave duplicates or a dead path behind.
    if let Some(arr) = obj.array_value("plugin") {
        for el in arr.elements() {
            if is_reliary_plugin_entry(&el) {
                el.remove();
            }
        }
    }

    let arr = obj
        .array_value_or_create("plugin")
        .ok_or("config has a non-array \"plugin\" value")?;
    arr.append(CstInputValue::String(plugin_path));

    if atomic_write(&cfg_path.to_string_lossy(), &root.to_string()) {
        Ok(true)
    } else {
        Err(format!("failed to write {}", cfg_path.display()))
    }
}

/// Remove the reliary MCP server entry from an OpenCode config, using a JSONC
/// edit so an `opencode.jsonc` (with comments) can be parsed and rewritten.
fn remove_opencode_mcp_server(cfg_path: &PathBuf, server_name: &str) -> bool {
    use jsonc_parser::cst::CstRootNode;
    let Ok(content) = fs::read_to_string(cfg_path) else { return false };
    let Ok(root) = CstRootNode::parse(&content, &jsonc_parser::ParseOptions::default()) else {
        return false;
    };
    let removed = root
        .object_value()
        .and_then(|o| o.object_value("mcp"))
        .and_then(|m| m.get(server_name))
        .map(|prop| {
            prop.remove();
            true
        })
        .unwrap_or(false);
    if removed {
        atomic_write(&cfg_path.to_string_lossy(), &root.to_string())
    } else {
        false
    }
}

/// Remove the reliary plugin entry from an OpenCode config and delete the
/// materialized plugin file. Symmetric with [`inject_opencode_plugin`];
/// unrelated plugin entries are preserved.
fn remove_opencode_plugin(cfg_path: &PathBuf) {
    use jsonc_parser::cst::CstRootNode;

    if let Ok(content) = fs::read_to_string(cfg_path) {
        if let Ok(root) =
            CstRootNode::parse(&content, &jsonc_parser::ParseOptions::default())
        {
            let mut changed = false;
            if let Some(obj) = root.object_value() {
                if let Some(arr) = obj.array_value("plugin") {
                    for el in arr.elements() {
                        if is_reliary_plugin_entry(&el) {
                            el.remove();
                            changed = true;
                        }
                    }
                }
            }
            if changed {
                let _ = atomic_write(&cfg_path.to_string_lossy(), &root.to_string());
            }
        }
    }
    if let Some(target) = opencode_plugin_target() {
        let _ = fs::remove_file(&target);
    }
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

    // 3. OpenCode — MCP server and the materialized regen plugin.
    if let Some(cfg_path) = opencode_config_path() {
        if cfg_path.exists() {
            // JSONC-aware removal: a `.jsonc` config with comments cannot be
            // parsed by the generic serde_json-based remover.
            let mcp_removed = remove_opencode_mcp_server(&cfg_path, "reliary");
            // Plugin entry + the file written on install; leaving the entry
            // behind would make OpenCode try to load a path we just deleted.
            remove_opencode_plugin(&cfg_path);
            if mcp_removed {
                ok("Removed Reliary from OpenCode");
                removed_agents += 1;
            }
        }
    }
    
    // 4. Cline
    if let Some(cfg_path) = cline_config_path() {
        if cfg_path.exists() && remove_mcp_server(&cfg_path, "reliary", "mcpServers") {
            ok("Removed Reliary from Cline");
            removed_agents += 1;
        }
    }

    // 5. Claude Code hooks
    println!("Removing Claude Code hooks...");
    if let Some(home) = home_dir() {
        // V61: also strip hook entries from ~/.claude/settings.json —
        // uninstall only removed the hook FILES before, leaving a broken
        // command reference that fires on every Bash call.
        remove_claude_hooks(&home);
        let hooks_dir = home.join(".claude/hooks");
        if hooks_dir.exists() {
            let reminder_path = hooks_dir.join("reliary-session-reminder");
            let sift_path = hooks_dir.join("reliary-sift-pretooluse");
            let mut hooks_removed = 0;
            for path in [reminder_path, sift_path] {
                if path.exists() {
                    let _ = fs::remove_file(&path);
                    hooks_removed += 1;
                }
            }
            // Legacy installs may still carry the removed code-gate file.
            let legacy_gate = hooks_dir.join("reliary-code-gate");
            if legacy_gate.exists() {
                let _ = fs::remove_file(&legacy_gate);
                hooks_removed += 1;
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

    // Short alias shim (mirror of install_rel_shim).
    remove_rel_shim();

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

/// V61: strip every reliary hook entry from ~/.claude/settings.json.
/// Mirror of register_claude_hooks — uninstall must remove what install
/// created (it previously only deleted the sift entry, leaving a broken
/// command reference that fired on every Bash tool call).
fn remove_claude_hooks(home: &std::path::Path) {
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
    let mut removed = false;
    if let Some(hooks) = v.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for spec in CLAUDE_HOOKS {
            let Some(arr) = hooks.get_mut(spec.event).and_then(|e| e.as_array_mut()) else {
                continue;
            };
            let before = arr.len();
            arr.retain(|entry| {
                !entry.get("hooks").and_then(|h| h.as_array())
                    .map(|hs| {
                        hs.iter().any(|h| {
                            h.get("command").and_then(|c| c.as_str())
                                .map(|c| c == spec.command)
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            });
            if arr.len() != before { removed = true; }
        }
        // Drop emptied event arrays to keep settings clean.
        let empty: Vec<String> = hooks.iter()
            .filter(|(_, e)| e.as_array().map(|a| a.is_empty()).unwrap_or(false))
            .map(|(k, _)| k.clone())
            .collect();
        for k in empty { hooks.remove(&k); }
    }
    if !removed { return; }
    // Drop the hooks object itself when nothing is left.
    let hooks_empty = v.get("hooks")
        .and_then(|h| h.as_object())
        .map(|h| h.is_empty())
        .unwrap_or(false);
    if hooks_empty {
        if let Some(obj) = v.as_object_mut() { obj.remove("hooks"); }
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
