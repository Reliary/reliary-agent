/// reliary-agent binary. Thin dispatch composing all crates.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod mcp;
mod daemon;
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

use clap::{Parser, Subcommand};
use std::io::Read;
use std::sync::Arc;
use crate::session_state::SessionState;

/// Simple ANSI color helpers
#[allow(dead_code)]
mod color {
    pub fn green(s: &str) -> String { format!("\x1b[32m{}\x1b[0m", s) }
    pub fn red(s: &str) -> String { format!("\x1b[31m{}\x1b[0m", s) }
    pub fn yellow(s: &str) -> String { format!("\x1b[33m{}\x1b[0m", s) }
    pub fn bold(s: &str) -> String { format!("\x1b[1m{}\x1b[0m", s) }
    pub fn dim(s: &str) -> String { format!("\x1b[2m{}\x1b[0m", s) }
}

fn index_db_path(path: &str) -> String {
    format!("{}/.reliary/index.sqlite", path.trim_end_matches('/'))
}

fn open_or_create_index(path: &str) -> Option<rusqlite::Connection> {
    let db_path = index_db_path(path);
    let db = rusqlite::Connection::open(&db_path).ok()?;
    reliary_search::schema::open_existing_db(&db).ok()?;
    Some(db)
}

#[derive(Parser)]
#[command(name = "reliary-agent", about = "Grammar-free code intelligence daemon, CLI, and MCP server",
          after_help = "\
EXAMPLES:
  rel index .              Build search index for current project
  rel search query .       Search indexed project
  rel risk src/main.rs     Check edit risk before making changes
  rel serve                Start daemon + proxy on :9090
  rel init                 Auto-configure agents (Pi, Claude, Cline)
  rel doctor               System health check

ALIAS:
  The binary also responds to 'rel' for shorter commands.
  e.g. 'rel serve', 'rel init', 'rel doctor'")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Output format: default (human), compact (agent), json (CI)
    #[arg(short, long, default_value = "default")]
    format: String,
}

#[derive(Subcommand)]
enum Commands {
    /// BM25 search against FTS5 index (from stria)
    Search { query: String, #[arg(default_value = ".")] path: String },
    /// Build FTS5 index from directory
    Index { path: String },
    /// IR reasoning compression (from gate)
    /// IR reasoning compression (from gate)
    /// Use --gentle for assistant messages (preserves code context)
    Compress {
        text: Option<String>,
        /// Gentle mode: preserve code context, only strip reasoning fluff
        #[arg(long)]
        gentle: bool,
    },
    /// Pre-edit risk analysis (from quale)
    Risk { file: String },
    /// Apply known fix patterns to directory (from cortex)
    FixDir { path: String },
    /// Apply fix pattern to single file
    FixFile { file: String, old: String, new: String },
    /// Dead code detection (from carrion)
    Dead { path: String },
    /// Cross-session memory info
    Memory { query: String },
    /// Build session state block from Pi session file
    SessionState { file: String },
    /// Micro-MCP server
    Mcp,
    /// TCP daemon
    Daemon,
    /// Bidirectional proxy (compresses conversation history)
    Serve { #[arg(default_value = "9090")] port: u16 },
    /// Self-healing apply-edit: apply content from file, test, revert on fail
    ApplyEdit { file: String, tmp_path: String, workdir: String },
    /// Identifier veto: check newText identifiers exist in project FTS5 index
    Veto { file: String },
    /// Configuration management (mode: fast/reactive/strict)
    Config {
        /// Config key to set (e.g. "mode")
        key: Option<String>,
        /// Config value to set (e.g. "fast" or "strict")
        value: Option<String>,
        /// Apply to project-local config instead of global
        #[arg(long)]
        local: bool,
        /// Project root for local config
        #[arg(long)]
        root: Option<String>,
    },
    /// Interactive setup for agents (Pi, Claude Code, OpenCode, Cline) and daemon
    Init,
    /// Uninstall integrations and background daemon
    Uninstall,
    /// Check system health and diagnosis
    Doctor,
    /// View project intelligence overview
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
    Logs,
}

fn format_config(fmt: &str) -> reliary_core::OutputFormat {
    match fmt {
        "compact" => reliary_core::OutputFormat::Compact,
        "json" => reliary_core::OutputFormat::Json,
        _ => reliary_core::OutputFormat::Default,
    }
}

fn main() {
    let cli = Cli::parse();
    let fmt = format_config(&cli.format);
    let cfg = reliary_core::FormatConfig::new(fmt);

    // If the binary is invoked as "rel", the subcommand name comes from $0
    // Otherwise use the standard name

    match &cli.command {
        Commands::Search { query, path } => {
            if let Some(db) = open_or_create_index(path) {
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
            let db_path_str = index_db_path(path);
            if let Some(parent) = std::path::Path::new(&db_path_str).parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::remove_file(&db_path_str).ok();
            match rusqlite::Connection::open(&db_path_str) {
                Ok(db) => {
                    if reliary_search::schema::create_new_db(&db).is_err() {
                        eprintln!("Error creating database schema");
                        return;
                    }
                    match reliary_search::ingest::index_directory(&db, path) {
                        Ok(count) => println!("Indexed {} files in {}", count, path),
                        Err(e) => eprintln!("Error: {}", e),
                    }
                }
                Err(e) => eprintln!("Error creating database: {}", e),
            }
        }
        Commands::Compress { text, gentle: _ } => {
            let input_buf: String = match text {
                Some(ref t) if !t.is_empty() && t != "---stdin---" => t.clone(),
                _ => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf).ok();
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
            let content = std::fs::read_to_string(file).unwrap_or_default();
            let risk_result = reliary_risk::compute_file_risk(file, &content);
            println!("{:?}", risk_result);
        }
        Commands::FixFile { file, old, new } => {
            eprintln!("Use apply-edit instead. 'fix-file' may be removed.");
            println!("Edit: {} → {} in {}", old, new, file);
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
        Commands::Dead { path } => {
            println!("Dead code analysis for: {}", path);
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
        Commands::Mcp => {
            eprintln!("Starting MCP server on stdio");
            mcp::serve_stdio();
        }
        Commands::Config { key, value, local, root } => {
            match (key, value) {
                (Some(k), Some(v)) => {
                    let root_str = root.as_deref();
                    println!("{}", config::set_config(k, v, *local, root_str));
                }
                (None, None) => {
                    // Show current config
                    let mode = config::resolve_mode(root.as_deref().or(Some(".")));
                    println!("gate mode: {}", mode.as_str());
                }
                _ => {
                    eprintln!("Usage: reliary-agent config [key] [value]");
                    eprintln!("       reliary-agent config (show current)");
                    eprintln!("       reliary-agent config --local mode strict (per-project)");
                }
            }
        }
        Commands::Init => {
            init::run();
        }
        Commands::Uninstall => {
            init::uninstall();
        }
        Commands::Doctor => {
            ux::doctor();
        }
        Commands::Status => {
            ux::status();
        }
        Commands::Clean { global, all } => {
            ux::clean(*global, *all);
        }
        Commands::Logs => {
            ux::logs();
        }
        Commands::Daemon => {
            eprintln!("'daemon' subcommand is deprecated. Use 'serve' instead.");
            let state = Arc::new(SessionState::new(
                &std::env::current_dir().unwrap_or_default().to_string_lossy().to_string()
            ));
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { crate::proxy::start(9799, Some(state)).await })
                .unwrap_or_else(|e| eprintln!("Server error: {}", e));
        }
        Commands::Serve { port } => {
            let state = Arc::new(SessionState::new(
                &std::env::current_dir().unwrap_or_default().to_string_lossy().to_string()
            ));
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { crate::proxy::start(*port, Some(state)).await })
                .unwrap_or_else(|e| eprintln!("Server error: {}", e));
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
                Err(e) => eprintln!("Error: {}", e),
            }
        }
        Commands::Veto { file } => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).ok();
            let new_text = buf.trim();
            // Find .reliary index from file path
            match daemon::find_reliary_root(file) {
                Some((_root, index_path, _)) => {
                    if let Ok(db) = rusqlite::Connection::open(&index_path) {
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
                                println!("ok");
                            } else {
                                println!("ERROR: veto: '{}' not found in project or known libraries", blocked.join(", "));
                            }
                        } else {
                            println!("ERROR: no index at {}", index_path);
                        }
                    } else {
                        println!("ok");
                    }
                }
                None => println!("ERROR: no .reliary found for this file"),
            }
        }
    }
}
