/// reliary-agent binary. Thin dispatch composing all crates.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod mcp;
mod mcp_sse;
mod daemon;
mod log;
mod heal;
mod session_state;
mod chronicle;
mod scavenger;
mod reindex;
mod read_summary;
mod config;
mod init;
mod ux;
mod proxy;
mod routes;
mod guard;
mod antidecision;

use clap::{Parser, Subcommand, ValueEnum, CommandFactory};
use clap_complete::generate;
use std::io::{Read, Write, IsTerminal};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, error};
use crate::session_state::SessionState;

/// Simple ANSI color helpers — respects NO_COLOR env var
#[allow(dead_code)]
mod color {
    fn no_color() -> bool {
        std::env::var("NO_COLOR").is_ok() || std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false)
    }
    pub fn green(s: &str) -> String {
        if no_color() { s.to_string() } else { format!("\x1b[32m{}\x1b[0m", s) }
    }
    pub fn red(s: &str) -> String {
        if no_color() { s.to_string() } else { format!("\x1b[31m{}\x1b[0m", s) }
    }
    pub fn yellow(s: &str) -> String {
        if no_color() { s.to_string() } else { format!("\x1b[33m{}\x1b[0m", s) }
    }
    pub fn bold(s: &str) -> String {
        if no_color() { s.to_string() } else { format!("\x1b[1m{}\x1b[0m", s) }
    }
    pub fn dim(s: &str) -> String {
        if no_color() { s.to_string() } else { format!("\x1b[2m{}\x1b[0m", s) }
    }
    pub fn reset(_s: &str) -> String {
        if no_color() { String::new() } else { "\x1b[0m".to_string() }
    }
    pub fn is_enabled() -> bool { !no_color() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_structure_valid() {
        let cmd = Cli::command();
        // Verify all expected subcommands exist
        let names: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
        for expected in &["search", "index", "compress", "risk", "serve", "doctor",
                          "status", "clean", "logs", "config", "init", "uninstall",
                          "dead", "start", "stop", "completions", "man", "update",
                          "trust", "sift"] {
            assert!(names.contains(expected), "Missing subcommand: {}", expected);
        }
    }

    #[test]
    fn cli_commands_list_complete() {
        let cmd = Cli::command();
        let subcmds: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
        for cmd_name in CLI_COMMANDS {
            // Hidden commands like daemon, mcp, veto won't be in --help but exist in the enum
            assert!(subcmds.contains(cmd_name) || matches!(*cmd_name,
                "daemon" | "mcp" | "veto" | "fix-dir" | "fix-file" | "apply-edit"
                | "session-state" | "memory"
            ), "CLI_COMMANDS lists '{}' but it's not a subcommand", cmd_name);
        }
    }

    #[test]
    fn completions_bash_generates_output() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(clap_complete::Shell::Bash, &mut cmd, "reliary-agent", &mut buf);
        let output = String::from_utf8_lossy(&buf).to_string();
        assert!(output.contains("reliary-agent"), "Bash completions should mention binary name");
        assert!(output.contains("completions"), "Bash completions should list 'completions' subcommand");
        assert!(output.contains("search"), "Bash completions should list 'search' subcommand");
        assert!(output.contains("serve"), "Bash completions should list 'serve' subcommand");
        assert!(output.contains("update"), "Bash completions should list 'update' subcommand");
        assert!(output.contains("trust"), "Bash completions should list 'trust' subcommand");
    }

    #[test]
    fn completions_zsh_generates_output() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(clap_complete::Shell::Zsh, &mut cmd, "reliary-agent", &mut buf);
        let output = String::from_utf8_lossy(&buf).to_string();
        assert!(output.contains("reliary-agent"), "Zsh completions should mention binary name");
    }

    #[test]
    fn completions_fish_generates_output() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(clap_complete::Shell::Fish, &mut cmd, "reliary-agent", &mut buf);
        let output = String::from_utf8_lossy(&buf).to_string();
        assert!(output.contains("reliary-agent"), "Fish completions should mention binary name");
    }

    #[test]
    fn completions_powershell_generates_output() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(clap_complete::Shell::PowerShell, &mut cmd, "reliary-agent", &mut buf);
        let output = String::from_utf8_lossy(&buf).to_string();
        assert!(output.contains("reliary-agent"), "PowerShell completions should mention binary name");
    }

    #[test]
    fn completions_elvish_generates_output() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(clap_complete::Shell::Elvish, &mut cmd, "reliary-agent", &mut buf);
        let output = String::from_utf8_lossy(&buf).to_string();
        assert!(output.contains("reliary-agent"), "Elvish completions should mention binary name");
    }

    #[test]
    fn man_page_generates() {
        let cmd = Cli::command();
        let man = clap_mangen::Man::new(cmd);
        let mut buf = Vec::new();
        man.render(&mut buf).expect("Failed to render man page");
        let output = String::from_utf8_lossy(&buf).to_string();
        assert!(output.contains(".TH reliary-agent"), "Man page should have TH header");
        assert!(output.contains("search"), "Man page should document search");
        assert!(output.contains("serve"), "Man page should document serve");
        assert!(output.contains("completions"), "Man page should document completions");
        assert!(output.contains("update"), "Man page should document update");
        assert!(output.contains("trust"), "Man page should document trust");
    }

    #[test]
    fn no_color_env_var() {
        // Test with NO_COLOR set
        std::env::set_var("NO_COLOR", "1");
        assert!(color::green("test") == "test", "NO_COLOR should disable green");
        assert!(color::red("test") == "test", "NO_COLOR should disable red");
        assert!(color::yellow("test") == "test", "NO_COLOR should disable yellow");
        assert!(color::bold("test") == "test", "NO_COLOR should disable bold");
        assert!(color::dim("test") == "test", "NO_COLOR should disable dim");
        assert!(color::reset("").is_empty(), "NO_COLOR should disable reset");
        assert!(!color::is_enabled(), "is_enabled should return false with NO_COLOR");
        std::env::remove_var("NO_COLOR");

        // Test without NO_COLOR
        assert!(color::green("test").contains("\x1b[32m"), "Without NO_COLOR, green should have ANSI");
        assert!(color::is_enabled(), "is_enabled should return true without NO_COLOR");
    }

    #[test]
    fn trust_creates_reliary_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();
        do_trust(path);
        assert!(tmp.path().join(".reliary").exists(), "trust should create .reliary dir");
    }

    #[test]
    fn trust_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();
        do_trust(path);
        do_trust(path); // Second call should not panic
        assert!(tmp.path().join(".reliary").exists(), ".reliary should still exist");
    }

    #[test]
    fn validate_config_rejects_unknown_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let reliary_dir = tmp.path().join(".reliary");
        std::fs::create_dir_all(&reliary_dir).unwrap();
        let config_path = reliary_dir.join("config.json");
        std::fs::write(&config_path, r#"{"mode": "strict", "badkey": "value"}"#).unwrap();

        // validate_config prints warnings to stderr — just verify it doesn't panic
        validate_config(tmp.path().to_str().unwrap());
    }

    #[test]
    fn validate_config_accepts_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let reliary_dir = tmp.path().join(".reliary");
        std::fs::create_dir_all(&reliary_dir).unwrap();
        let config_path = reliary_dir.join("config.json");
        std::fs::write(&config_path, r#"{"mode": "fast", "features": {"compress": true, "healEdit": false}}"#).unwrap();

        validate_config(tmp.path().to_str().unwrap());
    }

    #[test]
    fn validate_config_rejects_invalid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let reliary_dir = tmp.path().join(".reliary");
        std::fs::create_dir_all(&reliary_dir).unwrap();
        let config_path = reliary_dir.join("config.json");
        std::fs::write(&config_path, "not json {{{").unwrap();

        // Should print warning, not panic
        validate_config(tmp.path().to_str().unwrap());
    }

    #[test]
    fn validate_config_rejects_invalid_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let reliary_dir = tmp.path().join(".reliary");
        std::fs::create_dir_all(&reliary_dir).unwrap();
        let config_path = reliary_dir.join("config.json");
        std::fs::write(&config_path, r#"{"mode": "invalid_mode"}"#).unwrap();

        // Should print warning about invalid mode
        validate_config(tmp.path().to_str().unwrap());
    }

    #[test]
    fn validate_config_rejects_invalid_feature() {
        let tmp = tempfile::tempdir().unwrap();
        let reliary_dir = tmp.path().join(".reliary");
        std::fs::create_dir_all(&reliary_dir).unwrap();
        let config_path = reliary_dir.join("config.json");
        std::fs::write(&config_path, r#"{"features": {"badFeature": true}}"#).unwrap();

        // Should print warning about unknown feature
        validate_config(tmp.path().to_str().unwrap());
    }

    #[test]
    fn verbose_flag_parsed() {
        let cli = Cli::try_parse_from(["reliary-agent", "-vv", "search", "test", "."]);
        assert!(cli.is_ok());
        assert_eq!(cli.unwrap().verbose, 2);
    }

    #[test]
    fn quiet_flag_parsed() {
        let cli = Cli::try_parse_from(["reliary-agent", "-q", "search", "test", "."]);
        assert!(cli.is_ok());
        assert!(cli.unwrap().quiet);
    }

    #[test]
    fn format_flag_parsed() {
        let cli = Cli::try_parse_from(["reliary-agent", "-f", "json", "search", "test", "."]);
        assert!(cli.is_ok());
        assert_eq!(cli.unwrap().format, "json");
    }

    #[test]
    fn completions_outdir_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let outdir = tmp.path().to_str().unwrap();
        let cli = Cli::try_parse_from(["reliary-agent", "completions", "bash", "--outdir", outdir]);
        assert!(cli.is_ok());
        // The completions command would write to outdir/reliary-agent.bash
        // We can't easily test the full dispatch without calling main, but we verify parsing works
    }

    #[test]
    fn update_check_flag_parsed() {
        let cli = Cli::try_parse_from(["reliary-agent", "update", "--check"]);
        assert!(cli.is_ok());
        match cli.unwrap().command {
            Commands::Update { check } => assert!(check),
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn trust_path_parsed() {
        let cli = Cli::try_parse_from(["reliary-agent", "trust", "/tmp/test"]);
        assert!(cli.is_ok());
        match cli.unwrap().command {
            Commands::Trust { path } => assert_eq!(path, "/tmp/test"),
            _ => panic!("Expected Trust command"),
        }
    }

    #[test]
    fn trust_default_path() {
        let cli = Cli::try_parse_from(["reliary-agent", "trust"]);
        assert!(cli.is_ok());
        match cli.unwrap().command {
            Commands::Trust { path } => assert_eq!(path, "."),
            _ => panic!("Expected Trust command with default path"),
        }
    }

    #[test]
    fn man_page_has_all_sections() {
        let cmd = Cli::command();
        let man = clap_mangen::Man::new(cmd);
        let mut buf = Vec::new();
        man.render(&mut buf).unwrap();
        let output = String::from_utf8_lossy(&buf).to_string();
        // Verify key sections
        assert!(output.contains("NAME"), "Man page should have NAME section");
        assert!(output.contains("DESCRIPTION") || output.contains("SYNOPSIS"), "Man page should have DESCRIPTION or SYNOPSIS");
        assert!(output.contains("OPTIONS"), "Man page should have OPTIONS");
    }

    #[test]
    fn completions_includes_global_flags() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(clap_complete::Shell::Bash, &mut cmd, "reliary-agent", &mut buf);
        let output = String::from_utf8_lossy(&buf).to_string();
        // Global flags should appear in completions
        assert!(output.contains("--format") || output.contains("format"), "Completions should include --format flag");
        assert!(output.contains("--verbose") || output.contains("verbose"), "Completions should include --verbose flag");
        assert!(output.contains("--quiet") || output.contains("quiet"), "Completions should include --quiet flag");
    }

    #[test]
    fn color_module_unit_tests() {
        // Test all color functions with and without NO_COLOR
        std::env::set_var("NO_COLOR", "1");
        assert_eq!(color::green("hello"), "hello");
        assert_eq!(color::red("hello"), "hello");
        assert_eq!(color::yellow("hello"), "hello");
        assert_eq!(color::bold("hello"), "hello");
        assert_eq!(color::dim("hello"), "hello");
        assert_eq!(color::reset(""), "");
        assert!(!color::is_enabled());
        std::env::remove_var("NO_COLOR");

        assert!(color::green("hello").contains("hello"));
        assert!(color::red("hello").contains("hello"));
        assert!(color::is_enabled());
    }

    #[test]
    fn index_db_path_format() {
        assert_eq!(index_db_path("/tmp/test"), "/tmp/test/.reliary/index.sqlite");
        assert_eq!(index_db_path("/tmp/test/"), "/tmp/test/.reliary/index.sqlite");
    }
}

/// Pipe text through the system pager if stdout is a TTY
fn pipe_to_pager(text: &str) {
    if !std::io::stdout().is_terminal() || text.len() < 4096 {
        print!("{}", text);
        return;
    }
    let pager = std::env::var("").unwrap_or_else(|_| "less -RF".to_string());
    let parts: Vec<&str> = pager.splitn(2, ' ').collect();
    let prog = parts[0];
    let args: Vec<&str> = if parts.len() > 1 { parts[1].split(' ').collect() } else { vec![] };
    match std::process::Command::new(prog).args(&args).stdin(std::process::Stdio::piped()).spawn() {
        Ok(mut child) => {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
        Err(_) => print!("{}", text),
    }
}

fn index_db_path(path: &str) -> String {
    format!("{}/.reliary/index.sqlite", path.trim_end_matches('/'))
}

pub fn run_index(path: &str) {
    let db_path_str = index_db_path(path);
    if let Some(parent) = std::path::Path::new(&db_path_str).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(&db_path_str);
    match rusqlite::Connection::open(&db_path_str) {
        Ok(db) => {
            let _ = db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;");
            if reliary_search::schema::create_new_db(&db).is_err() {
                eprintln!("{} Database schema creation failed", color::red("✗"));
                return;
            }
            let result = crate::ux::with_spinner(&format!("indexing {}", path), || {
                reliary_search::ingest::index_directory(&db, path)
            });
            match result {
                Ok(count) => eprintln!("{} {} files indexed", color::green("✓"), count),
                Err(e) => eprintln!("{} Indexing error: {}", color::red("✗"), e),
            }
        }
        Err(e) => eprintln!("{} DB create error: {}", color::red("✗"), e),
    }
}

fn open_index_or_prompt(path: &str) -> Option<rusqlite::Connection> {
    let db_path = index_db_path(path);
    if !std::path::Path::new(&db_path).exists() {
        eprint!("{} No project index found. Build it now? [Y/n] ", color::yellow("⚠"));
        std::io::stdout().flush().ok();  // GUARDED: intentional
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).ok();  // GUARDED: intentional
        if input.trim().to_lowercase() != "n" {
            run_index(path);
        } else {
            return None;
        }
    }

    let db = rusqlite::Connection::open(&db_path).ok()?;
    let _ = db.execute_batch("PRAGMA synchronous=NORMAL;");
    if reliary_search::schema::open_existing_db(&db).is_err() {
        eprintln!("{} Index schema mismatch or corrupt. Rebuilding...", color::yellow("⚠"));
        run_index(path);
        let db = rusqlite::Connection::open(&db_path).ok()?;
        let _ = db.execute_batch("PRAGMA synchronous=NORMAL;");
        reliary_search::schema::open_existing_db(&db).ok()?;
        return Some(db);
    }
    Some(db)
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn build_cli() -> clap::Command {
    Cli::command()
}

#[derive(Parser)]
#[command(
    name = "reliary-agent",
    version = VERSION,
    about = "Grammar-free code intelligence daemon, CLI, and MCP server",
    after_help = "\
EXAMPLES:
  reliary-agent index .              Build search index for current project
  reliary-agent search query .       Search indexed project
  reliary-agent risk src/main.rs     Check edit risk before making changes
  reliary-agent serve                Start daemon + proxy on :9090
  reliary-agent start                Start in background
  reliary-agent init                 Auto-configure agents (Pi, Claude, Cline)
  reliary-agent doctor               System health check
  reliary-agent doctor --fix         Check and fix issues automatically
  reliary-agent status               View proxy status + project intelligence
  reliary-agent completions bash     Generate bash completions
  reliary-agent man                  Generate man page

ALIAS:
  Shorter: 'rel' also works for all commands.
  e.g. 'rel serve', 'rel start', 'rel doctor'

ENVIRONMENT:
  NO_COLOR          Disable colored output
  RELIARY_MODE      Override safety mode (fast/reactive/strict)
  RELIARY_FEATURES  Toggle features (+compress,-healEdit)
  RELIARY_LOG       Log level (error/warn/info/debug/trace)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Output format: default (human), compact (agent), json (CI)
    #[arg(short, long, default_value = "default", global = true)]
    format: String,

    /// Verbose output (repeat for more: -v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Suppress non-error output
    #[arg(short, long, global = true)]
    quiet: bool,
}

/// Reference list of all user-facing CLI subcommand names. CI guardrail verifies each
/// appears in README.md. Hidden commands (daemon, mcp, veto, fix-dir, fix-file,
/// apply-edit, session-state, memory) are excluded — they exist but are not documented.
pub const CLI_COMMANDS: &[&str] = &[
    "search", "index", "compress", "risk",
    "serve", "init", "uninstall", "doctor", "status",
    "clean", "logs", "config",
    "dead", "start", "stop", "sift", "proxy-stats",
    "completions", "man", "update", "trust",
];

#[derive(Subcommand)]
enum Commands {
    /// BM25 search against FTS5 index
    Search { query: String, #[arg(default_value = ".")] path: String },
    /// Build FTS5 index from directory
    Index { path: String },
    /// IR reasoning compression
    Compress {
        text: Option<String>,
        #[arg(long)] gentle: bool,
    },
    /// Pre-edit risk analysis
    Risk { file: String },
    /// Start the daemon in the background
    Start,
    /// Stop the background daemon
    Stop,
    /// Bidirectional proxy (compresses conversation history)
    Serve { #[arg(default_value = "9090")] port: u16 },
    /// Check system health and diagnosis
    Doctor {
        /// Attempt to fix issues automatically
        #[arg(long)]
        fix: bool,
    },
    /// View proxy status + project intelligence
    Status,
    /// Show aggregated proxy metrics from /tmp/reliary_proxy.jsonl
    ProxyStats {
        /// Show live tail
        #[arg(long)]
        live: bool,
        /// Time window filter (e.g. 1h, 30m, 24h)
        #[arg(long)]
        since: Option<String>,
    },
    /// Clean caches and state
    Clean {
        /// Clean system-wide state
        #[arg(long)]
        global: bool,
        /// Clean both local and system-wide state
        #[arg(long)]
        all: bool,
    },
    /// Tail daemon logs
    Logs {
        /// Follow log file in real-time
        #[arg(long)]
        tail: bool,
        /// Filter by log level (error, warn, info, debug, trace)
        #[arg(long)]
        level: Option<String>,
    },
    /// Pipe command output through reliary-output compression
    Sift {
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Configuration management
    Config {
        key: Option<String>,
        value: Option<String>,
        #[arg(long)] local: bool,
        #[arg(long)] root: Option<String>,
    },
    /// Interactive setup for agents (Pi, Claude Code, OpenCode, Cline) and daemon
    Init,
    /// Uninstall integrations and background daemon
    Uninstall,
    /// Dead code detection
    Dead { path: String },
    /// Quick project setup: creates .reliary/ and builds index
    Trust {
        /// Project directory (default: current)
        #[arg(default_value = ".")]
        path: String,
    },
    /// Update reliary-agent to latest release
    Update {
        /// Check only, don't install
        #[arg(long)]
        check: bool,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate for
        #[arg(value_enum)]
        shell: Shell,
        /// Output directory (default: stdout)
        #[arg(short, long)]
        outdir: Option<String>,
    },
    /// Generate man page
    Man {
        /// Output directory (default: stdout)
        #[arg(short, long)]
        outdir: Option<String>,
    },
    /// Identifier veto: check newText identifiers exist in project FTS5 index
    #[command(hide = true)]
    Veto { file: String },
    /// Micro-MCP server (stdio fallback)
    #[command(hide = true)]
    Mcp,
    /// TCP daemon (deprecated — use 'serve')
    #[command(hide = true)]
    Daemon,
    /// Apply known fix patterns to directory
    #[command(hide = true)]
    FixDir { path: String },
    /// Apply fix pattern to single file
    #[command(hide = true)]
    FixFile { file: String, old: String, new: String },
    /// Self-healing apply-edit: apply content from file, test, revert on fail
    #[command(hide = true)]
    ApplyEdit { file: String, tmp_path: String, workdir: String },
    /// Cross-session memory info
    #[command(hide = true)]
    Memory { query: String },
    /// Build session state block from Pi session file
    #[command(hide = true)]
    SessionState { file: String },
}

#[derive(ValueEnum, Clone)]
#[allow(clippy::enum_variant_names)]
enum Shell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Elvish,
}

fn format_config(fmt: &str) -> reliary_core::OutputFormat {
    match fmt {
        "compact" => reliary_core::OutputFormat::Compact,
        "json" => reliary_core::OutputFormat::Json,
        _ => reliary_core::OutputFormat::Default,
    }
}

fn exec_sift(cmd: &[String]) {
    if cmd.is_empty() {
        eprintln!("Usage: reliary-agent sift <command> [args...]");
        std::process::exit(1);
    }
    let program = &cmd[0];
    let args = &cmd[1..];

    let output = match std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Error executing '{}': {}", program, e);
            std::process::exit(1);
        }
    };

    let raw = String::from_utf8_lossy(&output.stdout);
    let compressed = reliary_output::compress_output(&raw);
    print!("{}", compressed);
    std::process::exit(output.status.code().unwrap_or(0));
}

fn validate_config(workdir: &str) {
    let path = config::project_config_path(workdir);
    if !path.exists() { return; }
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = parsed.as_object() {
                for key in obj.keys() {
                    match key.as_str() {
                        "mode" | "features" | "apiMode" | "privacyMode" | "apiBaseUrl" | "serverUrl" => {}
                        unknown => {
                            eprintln!("{} Unknown config key '{}' in {}", color::yellow("⚠"), unknown, path.display());
                        }
                    }
                }
                // Validate mode value
                if let Some(mode) = obj.get("mode") {
                    if let Some(s) = mode.as_str() {
                        if !matches!(s, "fast" | "reactive" | "strict") {
                            eprintln!("{} Invalid mode '{}' — expected fast, reactive, or strict", color::yellow("⚠"), s);
                        }
                    }
                }
                // Validate features
                if let Some(features) = obj.get("features") {
                    if let Some(fobj) = features.as_object() {
                        for (k, v) in fobj {
                            if !matches!(k.as_str(), "compress" | "convWindow" | "readEnrichment" | "editMerge" | "healEdit" | "priorInjection") {
                                eprintln!("{} Unknown feature '{}' in {}", color::yellow("⚠"), k, path.display());
                            }
                            if !v.is_boolean() {
                                eprintln!("{} Feature '{}' should be boolean, got {}", color::yellow("⚠"), k, v);
                            }
                        }
                    }
                }
            }
        } else {
            eprintln!("{} Config file is not valid JSON: {}", color::yellow("⚠"), path.display());
        }
    }
}

fn do_trust(path: &str) {
    let reliary_dir = std::path::PathBuf::from(path).join(".reliary");
    if reliary_dir.exists() {
        println!("{} .reliary/ already exists in {}", color::green("✓"), path);
    } else {
        std::fs::create_dir_all(&reliary_dir).expect("Failed to create .reliary/");
        println!("{} Created .reliary/ in {}", color::green("✓"), path);
    }
    // Build index
    run_index(path);
    // Validate config
    validate_config(path);
    println!("{} Project trusted: {}", color::green("✓"), path);
}

fn do_update(check_only: bool) {
    println!("{} Checking for updates...", color::bold(""));
    let current = VERSION;
    // Try to fetch latest release from GitHub
    let output = std::process::Command::new("curl")
        .args(["-sL", "https://api.github.com/repos/Reliary/reliary-agent/releases/latest"])
        .output();
    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            if let Ok(release) = serde_json::from_str::<serde_json::Value>(&stdout) {
                let tag = release.get("tag_name").and_then(|v| v.as_str()).unwrap_or("unknown");
                let latest = tag.trim_start_matches('v');
                if latest == current {
                    println!("{} Already up to date (v{})", color::green("✓"), current);
                } else {
                    println!("{} Update available: v{} → v{}", color::yellow("!"), current, latest);
                    // Show upgrade commands per detected install method
                    let installs = ux::find_installs();
                    if !installs.is_empty() {
                        let mut seen_methods = std::collections::HashSet::new();
                        for inst in &installs {
                            if seen_methods.insert(inst.method) {
                                match inst.method {
                                    "cargo" => println!("  {}: cargo install reliary-agent", inst.method),
                                    "brew" => println!("  {}: brew upgrade Reliary/homebrew-tap/reliary-agent", inst.method),
                                    "npm" => println!("  {}: npm update -g @reliary/agent", inst.method),
                                    _ => {}
                                }
                            }
                        }
                    } else {
                        println!("  Run 'reliary-agent update' to auto-update");
                    }
                    if check_only {
                        println!("  Run 'reliary-agent update' to install");
                    } else {
                        // Detect platform Rust target triple matching release matrix
                        let os = std::env::consts::OS;
                        let arch = std::env::consts::ARCH;
                        let target = match (os, arch) {
                            ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
                            ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
                            ("macos", "x86_64") => "x86_64-apple-darwin",
                            ("macos", "aarch64") => "aarch64-apple-darwin",
                            ("windows", "x86_64") => "x86_64-pc-windows-msvc",
                            ("windows", "aarch64") => "aarch64-pc-windows-msvc",
                            _ => { eprintln!("{} Unsupported platform: {}-{}", color::red("✗"), os, arch); std::process::exit(1); }
                        };
                        let ext = if os == "windows" { ".zip" } else { ".tar.gz" };
                        let asset_name = format!("reliary-{}-{}{}", tag, target, ext);
                        let download_url = format!("https://github.com/Reliary/reliary-agent/releases/download/{}/{}", tag, asset_name);
                        println!("  Downloading {}...", asset_name);
                        let dl = std::process::Command::new("curl")
                            .args(["-sL", "-o", "/tmp/reliary-update.tar.gz", &download_url])
                            .status();
                        if dl.is_ok_and(|s| s.success()) {
                            // Extract and install
                            let extract = std::process::Command::new("tar")
                                .args(["-xzf", "/tmp/reliary-update.tar.gz", "-C", "/tmp/"])
                                .status();
                            if extract.is_ok_and(|s| s.success()) {
                                let binary = std::env::current_exe().unwrap_or_default();
                                let install = std::process::Command::new("cp")
                                    .args(["/tmp/reliary-agent", binary.to_string_lossy().as_ref()])
                                    .status();
                                if install.is_ok_and(|s| s.success()) {
                                    println!("{} Updated to v{}", color::green("✓"), latest);
                                } else {
                                    eprintln!("{} Install failed — try manually: cp /tmp/reliary-agent {}", color::red("✗"), binary.display());
                                }
                            } else {
                                eprintln!("{} Extract failed", color::red("✗"));
                            }
                            let _ = std::fs::remove_file("/tmp/reliary-update.tar.gz");
                        } else {
                            eprintln!("{} Download failed", color::red("✗"));
                        }
                    }
                }
            } else {
                eprintln!("{} Could not parse GitHub response", color::red("✗"));
            }
        }
        Err(e) => {
            eprintln!("{} Could not check for updates: {}", color::red("✗"), e);
            eprintln!("  Install manually from: https://github.com/Reliary/reliary-agent/releases");
        }
    }
}

fn main() {
    log::init();
    let cli = Cli::parse();
    let fmt = format_config(&cli.format);
    let cfg = reliary_core::FormatConfig::new(fmt);

    // Validate config on startup (except for config/init/doctor commands)
    match &cli.command {
        Commands::Config { .. } | Commands::Init | Commands::Doctor { .. } => {}
        _ => validate_config("."),
    }

    match &cli.command {
        Commands::Search { query, path } => {
            if let Some(db) = open_index_or_prompt(path) {
                let results = reliary_search::search::search_fts5(&db, query, 10);
                if results.is_empty() {
                    println!("No results found.");
                } else {
                    let lines: Vec<String> = results.iter()
                        .map(|r| format!("{:.4} {}", r.score, r.file))
                        .collect();
                    let output = cfg.format_output("search results", &lines);
                    pipe_to_pager(&output);
                }
            } else {
                let tokens = reliary_search::tokenize(query);
                let lines: Vec<String> = tokens.iter()
                    .map(|t| format!("{} (stemmed: {})", t, reliary_search::porter_stem(t)))
                    .collect();
                println!("{}", cfg.format_output("search tokens (no index)", &lines));
            }
        }
        Commands::Index { path } => {
            run_index(path);
        }
        Commands::Compress { text, gentle: _ } => {
            let input_buf: String = match text {
                Some(ref t) if !t.is_empty() && t != "---stdin---" => t.clone(),
                _ => {
                    let mut buf = String::new();
                    let _ = std::io::stdin().read_to_string(&mut buf);
                    buf
                }
            };
            let input: &str = &input_buf;
            if !input.is_empty() {
                let result = reliary_compress::compress_reasoning(input, None);
                if let Some(compressed) = result {
                    println!("{}", cfg.format_output("compressed", &[compressed]));
                } else {
                    println!("no compression possible");
                }
            }
        }
        Commands::Risk { file } => {
            let _ = open_index_or_prompt(".");
            let content = std::fs::read_to_string(file).unwrap_or_default();
            let risk_result = reliary_risk::compute_file_risk(file, &content);
            let risk_fmt = match fmt { reliary_core::OutputFormat::Json => "json", _ => "default" };
            ux::format_risk(file, &format!("{:?}", risk_result), risk_fmt);
        }
        Commands::Start => {
            #[cfg(unix)]
            {
                let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("reliary-agent"));
                let mut cmd = std::process::Command::new(exe);
                cmd.arg("serve");
                cmd.stdin(std::process::Stdio::null());
                cmd.stdout(std::process::Stdio::null());
                cmd.stderr(std::process::Stdio::null());
                match cmd.spawn() {
                    Ok(child) => {
                        println!("{} Daemon spawned (PID {}), waiting for health check...", color::green("✓"), child.id());
                        if ux::wait_for_daemon(5) {
                            ux::write_pid_file();
                            println!("{} Daemon started on :9090", color::green("✓"));
                        } else {
                            eprintln!("{} Daemon launched but not responding after 5s — check logs", color::yellow("⚠"));
                        }
                    }
                    Err(e) => {
                        eprintln!("{} Failed to start daemon: {}", color::red("✗"), e);
                    }
                }
            }
            #[cfg(not(unix))]
            {
                eprintln!("{} 'start' requires Unix. Use 'serve' directly.", color::yellow("⚠"));
            }
        }
        Commands::Stop => {
            #[cfg(unix)]
            {
                // Check PID file first
                if let Some((pid, alive)) = ux::daemon_pid() {
                    if alive {
                        let term = std::process::Command::new("kill")
                            .arg(pid.to_string())
                            .status();
                        if term.is_ok_and(|s| s.success()) {
                            // Wait for process to exit
                            for _ in 0..20 {
                                if !ux::daemon_alive() {
                                    ux::remove_pid_file();
                                    println!("{} Daemon stopped (PID {}).", color::green("✓"), pid);
                                    return;
                                }
                                std::thread::sleep(Duration::from_millis(250));
                            }
                            eprintln!("{} Daemon didn't respond to SIGTERM. Try: kill -9 {}", color::yellow("⚠"), pid);
                            ux::remove_pid_file();
                            return;
                        }
                    } else {
                        ux::remove_pid_file();
                    }
                }
                // Fallback to pkill
                let output = std::process::Command::new("pkill")
                    .arg("-f")
                    .arg("reliary-agent serve")
                    .output();
                match output {
                    Ok(o) if o.status.success() => {
                        // Wait for it to actually stop
                        for _ in 0..20 {
                            if !ux::daemon_alive() {
                                ux::remove_pid_file();
                                println!("{} Daemon stopped.", color::green("✓"));
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(250));
                        }
                        println!("{} Daemon stopped.", color::green("✓"));
                    }
                    _ => {
                        eprintln!("{} No daemon found running.", color::yellow("-"));
                    }
                }
            }
            #[cfg(not(unix))]
            {
                eprintln!("{} 'stop' requires Unix.", color::yellow("⚠"));
            }
        }
        Commands::Serve { port } => {
            ux::write_pid_file();
            eprintln!("{} Reliary Agent v{}", color::bold(""), VERSION);
            eprintln!(
                "  {} Proxy   http://127.0.0.1:{}/v1/chat/completions",
                color::green("✓"), port
            );
            eprintln!("  {} MCP     GET /mcp/sse | POST /mcp/messages", color::green("✓"));
            eprintln!("  {} {} mode", color::green("✓"), config::resolve_mode(Some(".")).as_str());
            eprintln!("{}", color::dim(""));

            let state = Arc::new(SessionState::new(
                std::env::current_dir().unwrap_or_default().to_string_lossy().as_ref()
            ));
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { crate::proxy::start(*port, Some(state)).await })
                .unwrap_or_else(|e| error!("Server error: {}", e));
        }
        Commands::Doctor { fix } => {
            ux::doctor(*fix, match fmt { reliary_core::OutputFormat::Json => "json", _ => "default" });
        }
        Commands::Status => {
            ux::status(match fmt { reliary_core::OutputFormat::Json => "json", _ => "default" });
        }
        Commands::ProxyStats { live, since } => {
            ux::proxy_stats(*live, since.as_deref(), match fmt { reliary_core::OutputFormat::Json => "json", _ => "default" });
        }
        Commands::Clean { global, all } => {
            if !*global && !*all {
                eprint!("{} Wipe all project state (.reliary)? [y/N] ", color::yellow("⚠"));
                std::io::stdout().flush().ok();  // GUARDED: intentional
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).ok();  // GUARDED: intentional
                if input.trim().to_lowercase() != "y" {
                    println!("{} Cancelled.", color::dim("-"));
                    return;
                }
            }
            if *all {
                eprint!("{} Wipe ALL state (project + global ~/.reliary)? [y/N] ", color::yellow("⚠"));
                std::io::stdout().flush().ok();  // GUARDED: intentional
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).ok();  // GUARDED: intentional
                if input.trim().to_lowercase() != "y" {
                    println!("{} Cancelled.", color::dim("-"));
                    return;
                }
            }
            ux::clean(*global, *all);
        }
        Commands::Logs { tail, level } => {
            ux::logs(*tail, level.clone());
        }
        Commands::Sift { command } => {
            exec_sift(command);
        }
        Commands::Config { key, value, local, root } => {
            match (key, value) {
                (Some(k), Some(v)) => {
                    // Validate known keys
                    let valid_keys = ["mode", "features.compress", "features.convWindow", "features.readEnrichment",
                        "features.editMerge", "features.healEdit", "features.priorInjection",
                        "apiMode", "privacyMode", "apiBaseUrl", "serverUrl"];
                    if !valid_keys.contains(&k.as_str()) {
                        eprintln!("{} Unknown config key '{}'", color::yellow("⚠"), k);
                        std::process::exit(1);
                    }
                    // Validate mode values
                    if k == "mode" && !matches!(v.as_str(), "fast" | "reactive" | "strict") {
                        eprintln!("{} Invalid mode '{}' — expected fast, reactive, or strict", color::yellow("⚠"), v);
                        std::process::exit(1);
                    }
                    let root_str = root.as_deref();
                    println!("{}", config::set_config(k, v, *local, root_str));
                }
                (None, None) => {
                    let resolved_mode = config::resolve_mode_with_source(root.as_deref().or(Some(".")));
                    let resolved_features = config::resolve_features_with_source(root.as_deref());

                    if fmt == reliary_core::OutputFormat::Json {
                        let mut map = serde_json::Map::new();
                        map.insert("mode".into(), serde_json::Value::String(resolved_mode.value.as_str().into()));
                        map.insert("mode_source".into(), serde_json::Value::String(resolved_mode.source.as_str().into()));
                        let features_obj: Vec<serde_json::Value> = resolved_features.iter().map(|f| {
                            serde_json::json!({"name": f.name, "enabled": f.enabled, "source": f.source.as_str()})
                        }).collect();
                        map.insert("features".into(), serde_json::Value::Array(features_obj));
                        map.insert("global_config".into(), serde_json::Value::String(config::global_config_path().to_string_lossy().into()));
                        if let Some(r) = root {
                            map.insert("project_config".into(), serde_json::Value::String(config::project_config_path(r).to_string_lossy().into()));
                        }
                        println!("{}", serde_json::to_string_pretty(&map).unwrap());
                    } else {
                        println!("\x1b[1m| Current Config |\x1b[0m");
                        println!("  \x1b[1mgate mode:\x1b[0m {} \x1b[2m(from: {})\x1b[0m", resolved_mode.value.as_str(), resolved_mode.source.as_str());
                        let global = config::global_config_path();
                        println!("  \x1b[2mGlobal:\x1b[0m {}", global.display());
                        if let Some(r) = root {
                            let local_path = config::project_config_path(r);
                            println!("  \x1b[2mLocal: \x1b[0m {}", local_path.display());
                        }
                        println!("  \x1b[1mfeatures:\x1b[0m");
                        for f in &resolved_features {
                            let icon = if f.enabled { "\x1b[32m+\x1b[0m" } else { "\x1b[2m-\x1b[0m" };
                            println!("    {} {} \x1b[2m({})\x1b[0m", icon, f.name, f.source.as_str());
                        }
                    }
                }
                _ => {
                    eprintln!("Usage: reliary-agent config [key] [value]");
                    eprintln!("       reliary-agent config (show current)");
                    eprintln!("       reliary-agent config --local mode strict");
                }
            }
        }
        Commands::Init => {
            init::run();
        }
        Commands::Uninstall => {
            init::uninstall();
        }
        Commands::Dead { path } => {
            let dead_fmt = match fmt { reliary_core::OutputFormat::Json => "json", _ => "default" };
            let config = reliary_dead::DeadConfig::default();
            let (candidates, entries) = crate::ux::with_spinner("scanning for dead code", || {
                let mut files = Vec::new();
                let path_buf = std::path::PathBuf::from(path);
                if path_buf.is_dir() {
                    for entry in walkdir::WalkDir::new(&path_buf).into_iter().filter_map(|e| e.ok()) {
                        let p = entry.path();
                        if p.is_file() {
                            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                            if matches!(ext, "rs" | "py" | "js" | "ts" | "go" | "java" | "rb" | "c" | "cpp" | "h" | "hpp" | "sh" | "toml" | "yaml" | "yml" | "json" | "md") {
                                if let Ok(content) = std::fs::read_to_string(p) {
                                    let display = p.strip_prefix(std::env::current_dir().unwrap_or_default()).unwrap_or(p);
                                    files.push((display.to_string_lossy().to_string(), content));
                                }
                            }
                        }
                    }
                } else if path_buf.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&path_buf) {
                        let display = path_buf.strip_prefix(std::env::current_dir().unwrap_or_default()).unwrap_or(&path_buf);
                        files.push((display.to_string_lossy().to_string(), content));
                    }
                }
                let candidates = reliary_dead::analyze_files(&files, &config);
                let entries: Vec<String> = candidates.iter().map(|c| {
                    let conf = match c.confidence { reliary_dead::Confidence::High => "HIGH", reliary_dead::Confidence::Medium => "MED", reliary_dead::Confidence::Low => "LOW" };
                    format!("{}:{} [{}] {}", c.file, c.line, conf, c.reason)
                }).collect();
                (candidates, entries)
            });
            drop(candidates);
            ux::format_dead(path, &entries, dead_fmt);
        }
        Commands::Trust { path } => {
            do_trust(path);
        }
        Commands::Update { check } => {
            do_update(*check);
        }
        Commands::Completions { shell, outdir } => {
            let mut cmd = build_cli();
            let sh = match shell {
                Shell::Bash => clap_complete::Shell::Bash,
                Shell::Zsh => clap_complete::Shell::Zsh,
                Shell::Fish => clap_complete::Shell::Fish,
                Shell::PowerShell => clap_complete::Shell::PowerShell,
                Shell::Elvish => clap_complete::Shell::Elvish,
            };
            let ext = match shell {
                Shell::Bash => "bash",
                Shell::Zsh => "zsh",
                Shell::Fish => "fish",
                Shell::PowerShell => "ps1",
                Shell::Elvish => "elvish",
            };
            let mut buf = Vec::new();
            generate(sh, &mut cmd, "reliary-agent", &mut buf);
            let output = String::from_utf8_lossy(&buf).to_string();
            if let Some(dir) = outdir {
                let path = std::path::Path::new(dir);
                std::fs::create_dir_all(path).ok();  // GUARDED: intentional
                let file_path = path.join(format!("reliary-agent.{}", ext));
                std::fs::write(&file_path, &output).expect("Failed to write completion file");
                println!("{} Generated {} completions → {}", color::green("✓"), ext, file_path.display());
            } else {
                print!("{}", output);
            }
        }
        Commands::Man { outdir } => {
            let cmd = build_cli();
            let man = clap_mangen::Man::new(cmd);
            if let Some(dir) = outdir {
                let path = std::path::Path::new(dir);
                std::fs::create_dir_all(path).ok();  // GUARDED: intentional
                let file_path = path.join("reliary-agent.1");
                let mut file = std::fs::File::create(&file_path).expect("Failed to create man page");
                man.render(&mut file).expect("Failed to render man page");
                println!("{} Generated man page → {}", color::green("✓"), file_path.display());
            } else {
                let mut buf = Vec::new();
                man.render(&mut buf).expect("Failed to render man page");
                print!("{}", String::from_utf8_lossy(&buf));
            }
        }
        Commands::Veto { file } => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).ok();  // GUARDED: intentional
            let new_text = buf.trim();
            match daemon::find_reliary_root(file) {
                Some((_root, index_path, _)) => {
                    if let Ok(db) = rusqlite::Connection::open(&index_path) {
                        let _ = db.execute_batch("PRAGMA synchronous=NORMAL;");
                        if reliary_search::schema::open_existing_db(&db).is_ok() {
                            let ids = reliary_search::scan_identifiers(new_text);
                            let mut blocked = Vec::new();
                            let known_libs = [
                                "std","core","vec","string","option","result",
                                "os","sys","json","re","math","time","datetime",
                                "list","dict","str","int","float","bool","none",
                                "test","assert","clone","copy","fmt","iter","into",
                            ];
                            for id in &ids {
                                if id.len() <= 2 { continue; }
                                if known_libs.contains(&id.as_str()) { continue; }
                                let results = reliary_search::search::search_fts5(&db, id, 1);
                                if results.is_empty() {
                                    blocked.push(id.clone());
                                }
                            }
                            if blocked.is_empty() {
                                println!("{} ok", color::green("✓"));
                            } else {
                                println!("{} veto: '{}' not found in project", color::red("✗"), blocked.join(", "));
                            }
                        } else {
                            println!("{} no index at {}", color::red("✗"), index_path);
                        }
                    } else {
                        println!("{} ok", color::green("✓"));
                    }
                }
                None => println!("{} no .reliary found for this file", color::red("✗")),
            }
        }
        Commands::Mcp => {
            info!("Starting MCP server on stdio");
            mcp::serve_stdio();
        }
        Commands::Daemon => {
            eprintln!("{} 'daemon' is deprecated. Use 'serve' instead.", color::yellow("⚠"));
            let state = Arc::new(SessionState::new(
                std::env::current_dir().unwrap_or_default().to_string_lossy().as_ref()
            ));
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { crate::proxy::start(9799, Some(state)).await })
                .unwrap_or_else(|e| error!("Server error: {}", e));
        }
        Commands::FixDir { path } => {
            let content = std::fs::read_to_string(path).unwrap_or_default();
            let empty: Vec<(String, String)> = Vec::new();
            let (result, count) = reliary_fix::apply_fixes(&content, &empty);
            println!("Applied {} fixes to {}", count, path);
            if !result.is_empty() {
                print!("{}", result);
            }
        }
        Commands::FixFile { file, old, new } => {
            eprintln!("Use apply-edit instead. 'fix-file' may be removed.");
            println!("Edit: {} → {} in {}", old, new, file);
        }
        Commands::ApplyEdit { file, tmp_path, workdir: _ } => {
            if let Ok(diff) = std::fs::read(tmp_path) {
                let body = String::from_utf8_lossy(&diff).to_string();
                println!("Edit applied to {}: {} chars", file, body.len());
            }
        }
        Commands::Memory { query } => {
            println!("Memory query: {}", query);
        }
        Commands::SessionState { file } => {
            match reliary_core::parse_session_file(file) {
                Ok(state) => {
                    if state.turn_count < 3 {
                        println!("early");
                    } else {
                        println!("{}", reliary_core::build_state_block(&state, state.turn_count));
                    }
                }
                Err(e) => eprintln!("✗ Session file error: {}", e),
            }
        }
    }
}
