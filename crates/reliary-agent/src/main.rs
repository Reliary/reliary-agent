/// reliary-agent binary. Thin dispatch composing all crates.
mod mcp;
mod daemon;
mod heal;
mod tester;
mod read_summary;
mod chronicle;


use clap::{Parser, Subcommand};

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
#[command(name = "reliary-agent", about = "Grammar-free code intelligence daemon, CLI, and MCP server")]
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
    /// Self-healing apply-edit: apply content from file, test, revert on fail
    ApplyEdit { file: String, tmp_path: String, workdir: String },
    /// [Phase 1] FTS5 Sensory Cortex: structured file summary from index
    ReadSummary { file: String },
    /// [Phase 2] Chronicled Bayesian Prior: project state for session start
    Prior { path: String },
    /// [Phase 4] Multi-candidate beam search: parallel Pi sessions, pick winner
    Beam { task: String, workdir: String },
    /// Batch heal: apply multiple edits, run tests once, revert ALL on failure
    BatchHeal { edits_json: String, workdir: String },
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
            // Ensure .reliary directory exists
            let db_path_str = index_db_path(path);
            if let Some(parent) = std::path::Path::new(&db_path_str).parent() {
                std::fs::create_dir_all(parent).ok();
            }
            // Remove old DB if exists
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
        Commands::Compress { text, gentle } => {
            let input = text.as_deref().unwrap_or("");
            if !input.is_empty() {
                let result = if *gentle {
                    reliary_compress::gentle_compress(input)
                } else {
                    reliary_compress::aggressive_compress(input)
                };
                if let Some(compressed) = result {
                    println!("{}", cfg.format_output("compressed", &[compressed]));
                } else {
                    println!("{}", "no compression possible");
                }
            }
        }
        Commands::Risk { file } => {
            match std::fs::read_to_string(file) {
                Ok(content) => {
                    let risk = reliary_risk::compute_file_risk(file, &content);
                    let lines = vec![
                        format!("file: {}", risk.file),
                        format!("risk: {:?}", risk.risk),
                        format!("reason: {}", risk.reason),
                    ];
                    println!("{}", cfg.format_output("risk analysis", &lines));
                }
                Err(e) => eprintln!("Error reading {}: {}", file, e),
            }
        }
        Commands::FixDir { path } => {
            let entries = match std::fs::read_dir(path) {
                Ok(e) => e,
                Err(e) => { eprintln!("Error reading {}: {}", path, e); return; }
            };
            let mut total = 0;
            for entry in entries.flatten() {
                let fp = entry.path();
                if fp.extension().map(|e| e == "py" || e == "rs" || e == "js").unwrap_or(false) {
                    if let Some(p) = fp.to_str() {
                        if let Ok(content) = std::fs::read_to_string(p) {
                            // Check for patterns (simplified: no memory store in CLI)
                            let patterns = reliary_fix::content_aware_match("'v1' → 'v2' 'old' → 'new'", &content);
                            if !patterns.is_empty() {
                                let (modified, count) = reliary_fix::apply_fixes(&content, &patterns);
                                if count > 0 {
                                    std::fs::write(p, &modified).ok();
                                    total += count;
                                }
                            }
                        }
                    }
                }
            }
            println!("{}", cfg.format_output("fixes applied", &[format!("{} patterns matched", total)]));
        }
        Commands::FixFile { file, old, new } => {
            match std::fs::read_to_string(file) {
                Ok(content) => {
                    let fixes = vec![(old.clone(), new.clone())];
                    let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);
                    if count > 0 {
                        std::fs::write(file, &modified).ok();
                        println!("{}", cfg.format_output("replaced", &[format!("{} → {} (x{})", old, new, count)]));
                    }
                }
                Err(e) => eprintln!("Error reading {}: {}", file, e),
            }
        }
        Commands::Dead { path } => {
            let config = reliary_dead::DeadConfig::default();
            let mut all_candidates = Vec::new();
            let entries = match std::fs::read_dir(path) {
                Ok(e) => e,
                Err(e) => { eprintln!("Error reading {}: {}", path, e); return; }
            };
            for entry in entries.flatten() {
                let fp = entry.path();
                if fp.extension().map(|e| e == "py" || e == "rs" || e == "js").unwrap_or(false) {
                    if let Some(p) = fp.to_str() {
                        if let Ok(content) = std::fs::read_to_string(p) {
                            all_candidates.extend(reliary_dead::analyze_file(p, &content, &config));
                        }
                    }
                }
            }
            let lines: Vec<String> = all_candidates.iter()
                .map(|c| format!("{}:{} — {}{}", c.file, c.line, c.reason,
                    if c.confidence == reliary_dead::Confidence::High { " [HIGH]" } else { "" }))
                .collect();
            println!("{}", cfg.format_output("dead code", &lines));
        }
        Commands::Memory { query } => {
            let mut store = reliary_memory::MemoryStore::new(100);
            eprintln!("Note: in-memory store (no persistence in CLI mode)");
            let results = store.recall(query, 5);
            let lines: Vec<String> = results.iter()
                .map(|sm| format!("score={:.4}: {}", sm.score, sm.memory.content))
                .collect();
            println!("{}", cfg.format_output("memories", &lines));
        }
        Commands::ApplyEdit { file, tmp_path, workdir } => {
            match std::fs::read_to_string(tmp_path) {
                Ok(new_content) => {
                    match crate::heal::heal_edit(file, &new_content, workdir) {
                        Ok(()) => println!("OK: tests pass"),
                        Err(e) => eprintln!("REVERTED: {}", e),
                    }
                }
                Err(e) => eprintln!("ERROR: cannot read tmp file: {}", e),
            }
        }
        Commands::Mcp => {
            eprintln!("Starting MCP server on stdio");
            mcp::serve_stdio();
        }
        Commands::Daemon => {
            match daemon::start(9799) {
                Ok(()) => {},
                Err(e) => eprintln!("Daemon error: {}", e),
            }
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
        Commands::ReadSummary { file } => {
            println!("{}", read_summary::build(file));
        }
        Commands::Prior { path } => {
            println!("{}", chronicle::build_prior(&path));
        }
        Commands::Beam { task, workdir } => {
            // Phase 4: multi-candidate beam search
            eprintln!("Beam search not yet implemented (Phase 4). Task: {}, Workdir: {}", task, workdir);
            println!("beam: not yet implemented");
        }
        Commands::BatchHeal { edits_json, workdir } => {
            let edits: Vec<(String, String, String)> = match serde_json::from_str(&edits_json) {
                Ok(e) => e,
                Err(e) => { eprintln!("JSON parse error: {}", e); return; }
            };
            println!("{}", heal::batch_heal(&edits, &workdir));
        }
    }
}
