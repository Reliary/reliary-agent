/// reliary-agent binary. Thin dispatch composing all crates.
mod mcp;

use clap::{Parser, Subcommand};

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
    /// BM25 search (from stria)
    Search { query: String },
    /// IR reasoning compression (from gate)
    Compress { text: Option<String> },
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
    /// MCP stdio server
    Serve,
    /// TCP daemon
    Daemon,
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
        Commands::Search { query } => {
            let tokens = reliary_search::tokenize(query);
            let results: Vec<String> = tokens.iter()
                .map(|t| format!("{} (stemmed: {})", t, reliary_search::porter_stem(t)))
                .collect();
            println!("{}", cfg.format_output("search tokens", &results));
        }
        Commands::Compress { text } => {
            let input = text.as_deref().unwrap_or("");
            if !input.is_empty() {
                if let Some(compressed) = reliary_compress::compress_reasoning(input) {
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
        Commands::Serve => {
            eprintln!("Starting MCP server on stdio");
            mcp::serve_stdio();
        }
        Commands::Daemon => {
            eprintln!("Daemon mode not yet implemented in v0.1.0");
            eprintln!("Use 'serve' for MCP mode");
        }
    }
}
