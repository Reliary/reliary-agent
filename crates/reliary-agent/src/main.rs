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

use clap::{Parser, Subcommand};
use std::io::{Read, Write};
use std::sync::Arc;
use tracing::{info, error};
use crate::session_state::SessionState;

/// Simple ANSI color helpers
#[allow(dead_code)]
mod color {
    pub fn green(s: &str) -> String { format!("\x1b[32m{}\x1b[0m", s) }
    pub fn red(s: &str) -> String { format!("\x1b[31m{}\x1b[0m", s) }
    pub fn yellow(s: &str) -> String { format!("\x1b[33m{}\x1b[0m", s) }
    pub fn bold(s: &str) -> String { format!("\x1b[1m{}\x1b[0m", s) }
    pub fn dim(s: &str) -> String { format!("\x1b[2m{}\x1b[0m", s) }
    pub fn reset(_s: &str) -> String { "\x1b[0m".to_string() }
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
            println!("Building index for {}...", path);
            match reliary_search::ingest::index_directory(&db, path) {
                Ok(count) => println!("{} Indexed {} files", color::green("✓"), count),
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
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).ok();
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

ALIAS:
  Shorter: 'rel' also works for all commands.
  e.g. 'rel serve', 'rel start', 'rel doctor'"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Output format: default (human), compact (agent), json (CI)
    #[arg(short, long, default_value = "default")]
    format: String,
}

/// Reference list of all CLI subcommand names. CI guardrail verifies each
/// appears in README.md. Add new subcommands here and in the doc.
pub const CLI_COMMANDS: &[&str] = &[
    "search", "index", "compress", "risk",
    "fix-dir", "fix-file", "serve", "mcp",
    "init", "uninstall", "doctor", "status",
    "clean", "logs", "config", "veto",
    "apply-edit", "sift", "session-state", "memory",
    "dead", "start", "stop",
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
    /// Configuration management (mode: fast/reactive/strict, features: -healEdit)
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

fn main() {
    log::init();
    let cli = Cli::parse();
    let fmt = format_config(&cli.format);
    let cfg = reliary_core::FormatConfig::new(fmt);

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
                    println!("{}", cfg.format_output("search results", &lines));
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
                    println!("{}", "no compression possible");
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
                        println!("{} Daemon started in background (PID {}).", color::green("✓"), child.id());
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
                let output = std::process::Command::new("pkill")
                    .arg("-f")
                    .arg("reliary-agent serve")
                    .output();
                match output {
                    Ok(o) if o.status.success() => {
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
            // User-visible startup banner (not tracing — users need to see this)
            eprintln!("{} Reliary Agent v{}", color::bold(""), VERSION);
            eprintln!(
                "  {} Proxy   http://127.0.0.1:{}/v1/chat/completions",
                color::green("✓"), port
            );
            eprintln!("  {} MCP     GET /mcp/sse | POST /mcp/messages", color::green("✓"));
            eprintln!("  {} {} mode", color::green("✓"), config::resolve_mode(Some(".")).as_str());
            eprintln!("{}", color::dim(""));

            let state = Arc::new(SessionState::new(
                &std::env::current_dir().unwrap_or_default().to_string_lossy().to_string()
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
        Commands::Clean { global, all } => {
            if !*global && !*all {
                eprint!("{} Wipe all project state (.reliary)? [y/N] ", color::yellow("⚠"));
                std::io::stdout().flush().ok();
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).ok();
                if input.trim().to_lowercase() != "y" {
                    println!("{} Cancelled.", color::dim("-"));
                    return;
                }
            }
            if *all {
                eprint!("{} Wipe ALL state (project + global ~/.reliary)? [y/N] ", color::yellow("⚠"));
                std::io::stdout().flush().ok();
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).ok();
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
            let mut files = Vec::new();
            let path_buf = std::path::PathBuf::from(path);
            if path_buf.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&path_buf) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.is_file() {
                            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                            if matches!(ext, "rs" | "py" | "js" | "ts" | "go" | "java" | "rb" | "c" | "cpp" | "h" | "hpp" | "sh" | "toml" | "yaml" | "yml" | "json" | "md") {
                                if let Ok(content) = std::fs::read_to_string(&p) {
                                    let display = p.strip_prefix(std::env::current_dir().unwrap_or_default()).unwrap_or(&p);
                                    files.push((display.to_string_lossy().to_string(), content));
                                }
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
            ux::format_dead(path, &entries, dead_fmt);
        }
        Commands::Veto { file } => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).ok();
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
                &std::env::current_dir().unwrap_or_default().to_string_lossy().to_string()
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
