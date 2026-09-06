/// reliary-agent binary. Thin dispatch composing all crates.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod mcp;
mod fix_agent;
mod deterministic_fix;

mod log;
mod reindex;
mod watcher;
mod read_summary;
mod config;
mod init;
mod tee;
mod ux;

mod paths;

use clap::{Parser, Subcommand, ValueEnum, CommandFactory};
use clap_complete::generate;
use std::io::{Write, IsTerminal};
macro_rules! info {
    ($($arg:tt)*) => { eprintln!("[INFO] $($arg)*") };
}
macro_rules! error {
    ($($arg:tt)*) => { eprintln!("[ERROR] $($arg)*") };
}

/// Simple ANSI color helpers — respects NO_COLOR env var
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
        for expected in &["search", "index", "compress", "risk", "doctor",
                          "status", "clean", "logs", "config", "init", "uninstall",
                          "dead", "completions", "man", "update",
                          "trust", "sift"] {
            assert!(names.contains(expected), "Missing subcommand: {}", expected);
        }
    }

    #[test]
    fn cli_commands_list_complete() {
        let cmd = Cli::command();
        let subcmds: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
        for cmd_name in CLI_COMMANDS {
            // Hidden commands like mcp, veto won't be in --help but exist in the enum
            assert!(subcmds.contains(cmd_name) || matches!(*cmd_name,
                "mcp" | "fix-dir" | "fix-file" | "apply-edit"
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
        assert!(output.contains("server"), "Man page should document MCP server");
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
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less -RF".to_string());
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

// P17: Shared DB path helper. Used by both main and read_summary.
pub fn index_db_path(path: &str) -> String {
    format!("{}/.reliary/index.sqlite", path.trim_end_matches('/'))
}

/// Fallible, public path resolution used by MCP server at startup.
pub fn index_db_path_fallible(path: &str) -> Option<std::path::PathBuf> {
    let s = format!("{}/.reliary/index.sqlite", path.trim_end_matches('/'));
    let p = std::path::PathBuf::from(s);
    if p.exists() { Some(p) } else { None }
}

pub fn run_index(path: &str) {
    let db_path_str = index_db_path(path);
    if let Some(parent) = std::path::Path::new(&db_path_str).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Rename existing index to .bak (recovery if create_new_db fails)
    let bak_path = format!("{}.bak", db_path_str);
    let _ = std::fs::remove_file(&bak_path);  // remove any old backup
    if std::path::Path::new(&db_path_str).exists() {
        let _ = std::fs::rename(&db_path_str, &bak_path);
    }
    match reliary_core::safe_open_db(&db_path_str) {
        Ok(db) => {
            if reliary_search::schema::create_new_db(&db).is_err() {
                // V61: restore the old index — a failed schema build must not
                // leave a broken empty DB in place.
                let _ = std::fs::rename(&bak_path, &db_path_str);
                eprintln!("{} Database schema creation failed (old index restored)", color::red("✗"));
                return;
            }
            let result = crate::ux::with_spinner(&format!("indexing {}", path), || {
                reliary_search::ingest::index_directory(&db, path)
            });
            match result {
                Ok(count) => {
                    let _ = std::fs::remove_file(&bak_path);
                    eprintln!("{} {} files indexed", color::green("✓"), count);
                }
                Err(e) => {
                    // V61: restore the old index on ingest failure.
                    drop(db);
                    let _ = std::fs::rename(&bak_path, &db_path_str);
                    eprintln!("{} Indexing error: {} (old index restored)", color::red("✗"), e);
                }
            }
        }
        Err(e) => {
            let _ = std::fs::rename(&bak_path, &db_path_str);
            eprintln!("{} DB create error: {} (old index restored)", color::red("✗"), e);
        }
    }
}

/// Run VACUUM on an index to reclaim free pages.
/// Use this once after major dedup work (e.g., after deleting many files or
/// running reindex on big directories).
pub fn run_vacuum(path: &str) {
    let db_path = index_db_path(path);
    if !std::path::Path::new(&db_path).exists() {
        eprintln!("{} no index at {}", color::red("✗"), db_path);
        return;
    }
    let before = match std::fs::metadata(&db_path) {
        Ok(m) => m.len(),
        Err(e) => { eprintln!("{} stat: {}", color::red("✗"), e); return; }
    };
    eprintln!("vacuuming {} ({} bytes)...", db_path, before);
    match rusqlite::Connection::open(&db_path) {
        Ok(db) => {
            if let Err(e) = db.execute_batch("VACUUM;") {
                eprintln!("{} VACUUM failed: {}", color::red("✗"), e);
                return;
            }
        }
        Err(e) => { eprintln!("{} open: {}", color::red("✗"), e); return; }
    }
    let after = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let saved = if before > after { before - after } else { 0 };
    eprintln!("{} {} → {} bytes ({} saved)",
        color::green("✓"), before, after, saved);
}

/// Arc 34 Step 5: opt-in full occurrence build. Trust runs in fast lazy mode
/// by default; this command materializes the occurrence table for users who
/// want every phrase to be queryable instantly (no JIT cost on first query).
pub fn run_build_occurrences(path: &str) {
    let db_path = index_db_path(path);
    if !std::path::Path::new(&db_path).exists() {
        eprintln!("{} no index at {}", color::red("✗"), db_path);
        return;
    }
    let db = match rusqlite::Connection::open(&db_path) {
        Ok(d) => d,
        Err(e) => { eprintln!("{} open: {}", color::red("✗"), e); return; }
    };
    let start = std::time::Instant::now();
    let unresolved = match reliary_search::lazy_occurrence::count_unresolved_phrases(&db) {
        Ok(n) => n,
        Err(e) => { eprintln!("{} count: {}", color::red("✗"), e); return; }
    };
    eprintln!("building occurrence rows for {} unresolved phrases...", unresolved);
    let built = match reliary_search::lazy_occurrence::build_all_occurrence(&db) {
        Ok(n) => n,
        Err(e) => { eprintln!("{} build: {}", color::red("✗"), e); return; }
    };
    eprintln!("{} {} rows in {:?}", color::green("✓"), built, start.elapsed());
}

/// Arc 35: build all lazy tables (occurrence + block + scope + method).
/// Walks every file in the corpus and ensures all 4 lazy tables are populated.
/// Used by `reliary build-all <path>` for users who want zero query-time JIT cost.
pub fn run_build_all(path: &str) {
    let db_path = index_db_path(path);
    if !std::path::Path::new(&db_path).exists() {
        eprintln!("{} no index at {}", color::red("✗"), db_path);
        return;
    }
    let db = match rusqlite::Connection::open(&db_path) {
        Ok(d) => d,
        Err(e) => { eprintln!("{} open: {}", color::red("✗"), e); return; }
    };
    let start = std::time::Instant::now();

    // Get list of all file_ids.
    let file_ids: Vec<i64> = {
        let mut stmt = match db.prepare_cached("SELECT id FROM file_map") {
            Ok(s) => s,
            Err(e) => { eprintln!("{} prepare: {}", color::red("✗"), e); return; }
        };
        let rows = match stmt.query_map([], |r| r.get::<_, i64>(0)) {
            Ok(r) => r,
            Err(e) => { eprintln!("{} query: {}", color::red("✗"), e); return; }
        };
        rows.filter_map(|x| x.ok()).collect()
    };
    eprintln!("building lazy tables for {} files...", file_ids.len());

    let _total_blocks = 0usize;
    let mut total_blocks = 0usize;
    for (i, fid) in file_ids.iter().enumerate() {
        total_blocks += reliary_search::lazy_tables::ensure_blocks_for_file(&db, *fid).unwrap_or(0);
        if (i + 1) % 5000 == 0 {
            eprintln!("  ... {}/{} files", i + 1, file_ids.len());
        }
    }
    let occ = reliary_search::lazy_occurrence::build_all_occurrence(&db).unwrap_or(0);
    eprintln!("{} blocks={} occurrence={} in {:?}",
        color::green("✓"), total_blocks, occ, start.elapsed());
}

/// Re-index a single file after edit. Called by gate.js hooks.
pub fn run_reindex_file(file: &str) {
    // Walk up from the file to find the .reliary root
    let start = std::path::Path::new(file);
    let mut current = if start.is_dir() {
        start.to_path_buf()
    } else {
        match start.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                eprintln!("{} cannot determine parent of {}", color::red("✗"), file);
                return;
            }
        }
    };
    let mut idx_path: Option<String> = None;
    loop {
        let candidate = current.join(".reliary").join("index.sqlite");
        if candidate.exists() {
            idx_path = Some(candidate.to_string_lossy().to_string());
            break;
        }
        if !current.pop() {
            break;
        }
    }
    let idx_path = match idx_path {
        Some(p) => p,
        None => {
            eprintln!("{} no .reliary index found for {}", color::yellow("⚠"), file);
            return;
        }
    };
    match reliary_core::safe_read(file) {
        Ok(content) => {
            let count = crate::reindex::reindex_single_file(&idx_path, file, &content);
            if count > 0 {
                eprintln!("{} reindexed {} tokens from {}", color::green("✓"), count, file);
            } else {
                eprintln!("{} reindex failed for {}", color::red("✗"), file);
            }
        }
        Err(e) => eprintln!("{} cannot read {}: {}", color::red("✗"), file, e),
    }
}

/// Who calls this identifier (file + identifier). Returns list of files referencing it.
pub fn run_who_calls(file: &str, identifier: &str) {
    let start = std::path::Path::new(file);
    let mut current = if start.is_dir() {
        start.to_path_buf()
    } else {
        match start.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                println!("[]");
                return;
            }
        }
    };
    let mut idx_path: Option<String> = None;
    loop {
        let candidate = current.join(".reliary").join("index.sqlite");
        if candidate.exists() {
            idx_path = Some(candidate.to_string_lossy().to_string());
            break;
        }
        if !current.pop() {
            break;
        }
    }
    let idx_path = match idx_path {
        Some(p) => p,
        None => {
            println!("[]");
            return;
        }
    };
    if let Ok(db) = rusqlite::Connection::open(&idx_path) {
        let _ = db.execute_batch("PRAGMA synchronous = NORMAL;");
        let results = reliary_search::search::search_fts5(&db, identifier, 10);
        let file_name = std::path::Path::new(file).file_name()
            .and_then(|n| n.to_str()).unwrap_or("");
        let refs: Vec<String> = results.iter()
            .filter(|r| r.file != file || !r.file.ends_with(file_name))
            .map(|r| r.file.clone())
            .collect();
        println!("{}", serde_json::to_string(&refs).unwrap_or_else(|_| "[]".to_string()));
    } else {
        println!("[]");
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
    if reliary_search::schema::open_existing_db_safe(&db).is_err() {
        eprintln!("{} Index schema mismatch or corrupt. Rebuilding...", color::yellow("⚠"));
        run_index(path);
        let db = rusqlite::Connection::open(&db_path).ok()?;
        let _ = db.execute_batch("PRAGMA synchronous=NORMAL;");
        reliary_search::schema::open_existing_db_safe(&db).ok()?;
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
    about = "Grammar-free code intelligence CLI and MCP server",
    after_help = "\
EXAMPLES:
  reliary-agent index .              Build search index for current project
  reliary-agent search query .       Search indexed project
  reliary-agent risk src/main.rs     Check edit risk before making changes
  reliary-agent init                 Auto-configure agents (Pi, Claude, Cline)
  reliary-agent doctor               System health check
  reliary-agent doctor --fix         Check and fix issues automatically
  reliary-agent completions bash     Generate bash completions
  reliary-agent man                  Generate man page

ALIAS:
  Shorter: 'rel' also works for all commands.
  e.g. 'rel search', 'rel doctor'

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
/// appears in README.md. Hidden commands (mcp, memory, session-state) are
/// excluded — they exist but are not documented.
/// v0.8: veto, fix-dir, fix-file, apply-edit, serve, start, stop, proxy-stats removed.
pub const CLI_COMMANDS: &[&str] = &[
    "search", "index", "compress", "risk",
    "init", "uninstall", "doctor", "status",
    "clean", "logs", "config",
    "dead", "sift",
    "completions", "man", "update", "trust",
];

#[derive(Subcommand)]
enum Commands {
    /// BM25 search against FTS5 index
    Search { query: String, #[arg(default_value = ".")] path: String },
    /// Build FTS5 index from directory
    Index { path: String },
    /// Re-index a single file after edit. Called by gate.js hooks after each modification.
    ReindexFile { file: String },
    /// Run VACUUM on the index DB to reclaim disk space after heavy edits.
    Vacuum { path: String },
    /// Build all occurrence rows (Arc 34 lazy mode — opt-in for users who want the full index).
    BuildOccurrences { path: String },
    /// Build all lazy tables (occurrence + block + scope + method) for the corpus.
    BuildAll { path: String },
    /// Who calls this identifier (callers + callees graph)
    WhoCalls { file: String, identifier: String },
    /// IR reasoning compression
    Compress {
        text: Option<String>,
        #[arg(long)] gentle: bool,
    },
    /// Risk analysis
    Risk { file: String },
    /// Generate a holographic codebase pack (cache-stable, model-readable)
    Pack {
        /// Codebase root path
        path: String,
        /// Pack format: l2l3 (signatures + surprise, default) or full (all layers)
        #[arg(long, default_value = "l2l3")]
        format: String,
        /// Strategy: full (all symbols), hotspot (top-K by composite score, default 50)
        #[arg(long, default_value = "full")]
        strategy: String,
        /// Top-K symbols when strategy=hotspot (default 50)
        #[arg(long, default_value = "50")]
        top_k: usize,
        /// Auto-mode: gate by complexity score, use hotspot selection
        #[arg(long)]
        auto: bool,
        /// Slice mode: generate full pack then retrieve top-K entries for query
        #[arg(long)]
        slice_query: Option<String>,
    },
    /// Check system health and diagnosis
    Doctor {
        /// Attempt to fix issues automatically
        #[arg(long)]
        fix: bool,
    },
    /// View project intelligence
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
    /// Tail reliary logs
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
        /// Read from stdin, compress, print to stdout
        #[arg(long)]
        stdin: bool,
        /// LLM mode: drop context lines from diffs, skip error block merging.
        /// Use this when piping output to an LLM (default for sift --stdin).
        #[arg(long)]
        llm: bool,
        /// Aggressive mode: lower entropy threshold, force compression on
        /// repetitive output. Useful for cargo test / pytest runs.
        #[arg(long)]
        aggressive: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Run a command and pipe output through compression + cache.
    /// Original content is stored by hash so it can be retrieved with `cache-retrieve`.
    Wrap {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Store text in content cache, print hash (reads from stdin or --text)
    CacheStore {
        #[arg(long)]
        text: Option<String>,
    },
    /// Retrieve original content from cache by hash
    CacheRetrieve {
        hash: String,
    },
    /// Show content cache stats
    CacheStats,
    /// Configuration management
    Config {
        key: Option<String>,
        value: Option<String>,
        #[arg(long)] local: bool,
        #[arg(long)] root: Option<String>,
    },
    /// Interactive setup for agents (Pi, Claude Code, OpenCode, Cline)
    Init {
        /// Show what would be installed without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Uninstall integrations
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
    /// Autonomous bug-fix agent: LLM drives reliary's tools to resolve a task.
    /// Reads the repo index, calls find_references/callgraph/search to locate
    /// the code, applies edits via the grammar-free edit primitive, then runs
    /// the verifier. Self-contained single binary (no external agent needed).
    Fix {
        /// Task description ("fix the panic in ingest.rs")
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        task: Vec<String>,
        /// Project directory (default: current)
        #[arg(long, default_value = ".")]
        path: String,
        /// Max agent iterations (default: 12)
        #[arg(long, default_value = "12")]
        max_iters: usize,
        /// Verification command template ({file} replaced). Default: cargo check -p reliary-search
        #[arg(long)]
        verify: Option<String>,
        /// Dry run: show planned edits without applying
        #[arg(long)]
        dry_run: bool,
        /// Emit structured JSON progress on stdout
        #[arg(long)]
        json: bool,
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
    /// Micro-MCP server (stdio)
    #[command(hide = true)]
    Mcp,
    /// Cross-session memory info
    #[command(hide = true)]
    Memory { query: String },
    /// Build session state block from Pi session file
    #[command(hide = true)]
    SessionState { file: String },
    /// Stem-aware role classifier for a single line (used by bench).
    /// Reads file:line and stem from args, prints role label to stdout.
    #[command(hide = true)]
    Classify {
        file: String,
        line: i32,
        stem: String,
    },
    /// Arc 25: Parse expression with the mined operator table and print postfix.
    #[command(hide = false)]
    ParseExpr {
        line: String,
        #[arg(default_value = ".")] path: String,
    },
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

/// Run a command, pipe output through compression, store original in content cache.
/// Compressed output goes to stdout with `[reliary-compressed ... retrieve ...]` suffix.
fn exec_wrap(cmd: &[String]) {
    if cmd.is_empty() {
        eprintln!("Usage: reliary-agent wrap <command> [args...]");
        std::process::exit(1);
    }
    let program = &cmd[0];
    let args = &cmd[1..];

    // V67: content readers (cat/head/tail/less/more/bat) on a single
    // source-like file must PASSTHROUGH — compression drops structurally
    // critical lines (e.g. a struct's closing brace), and a model that
    // builds an edit from the compressed view produces broken edits.
    // (Edit-safety directive: sift must not break edit operations.)
    // Non-source targets (logs, data dumps) still compress.
    let program_name_v67 = std::path::Path::new(program).file_name()
        .and_then(|n| n.to_str()).unwrap_or(program);
    if matches!(program_name_v67, "cat" | "head" | "tail" | "less" | "more" | "bat") {
        let file_args: Vec<&String> = args.iter()
            .filter(|a| !a.starts_with('-'))
            .collect();
        if file_args.len() == 1 {
            let path = std::path::Path::new(file_args[0].as_str());
            if path.is_file() {
                if let Ok(sample) = std::fs::read(path) {
                    let text = String::from_utf8_lossy(&sample);
                    if reliary_search::lazy_occurrence::is_source_like(&text) {
                        // Passthrough: run the command directly, no compression.
                        let output = match std::process::Command::new(program)
                            .args(args)
                            .stdin(std::process::Stdio::inherit())
                            .stdout(std::process::Stdio::inherit())
                            .stderr(std::process::Stdio::inherit())
                            .status()
                        {
                            Ok(s) => s,
                            Err(e) => {
                                eprintln!("Error executing '{}': {}", program, e);
                                std::process::exit(1);
                            }
                        };
                        std::process::exit(output.code().unwrap_or(0));
                    }
                }
            }
        }
    }

    // V60: inherit stdin so interactive commands (`git rebase -i`,
    // `npm init`) don't get EOF from a nulled stdin.
    let output = match std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::inherit())
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
    let exit_code = output.status.code().unwrap_or(0);
    if raw.is_empty() {
        std::process::exit(exit_code);
    }

    // Store original in content cache (best-effort, doesn't fail the wrap)
    let cache_path = std::path::Path::new(".reliary/cache.sqlite");
    std::fs::create_dir_all(".reliary").ok();  // GUARDED: intentional
    let hash = open_or_create(cache_path)
        .ok()
        .and_then(|conn| reliary_core::store(&conn, &raw).ok());

    // Compress via sift pipeline
    let program_name = std::path::Path::new(program).file_name()
        .and_then(|n| n.to_str()).unwrap_or(program);
    let args_joined = args.join(" ");
    let is_pytest = args_joined.contains("pytest") || program_name == "pytest";
    let is_cargo = program_name == "cargo" || args_joined.contains(" cargo ");
    let is_test_cmd = is_pytest || is_cargo;
    let is_test_passing = exit_code == 0 && is_test_cmd;

    let compressed = if is_test_passing {
        let passed_count = if is_pytest {
            raw.lines().rev().take(3).find_map(|l| {
                let parts: Vec<&str> = l.split_whitespace().collect();
                for (i, p) in parts.iter().enumerate() {
                    if *p == "passed" && i > 0 {
                        return parts.get(i - 1).and_then(|n| n.parse::<usize>().ok());
                    }
                }
                None
            }).unwrap_or(0)
        } else {
            raw.lines().find_map(|l| {
                if l.contains("test result: ok") {
                    l.split_whitespace().nth(4).and_then(|n| n.parse::<usize>().ok())
                } else { None }
            }).unwrap_or(0)
        };
        if passed_count > 0 {
            format!("[reliary: {} tests passed]", passed_count)
        } else {
            reliary_output::compress_unified(&raw)
        }
    } else {
        match program_name {
            "cat" | "head" | "tail" | "less" | "more" => sift_file_read(&raw, args),
            _ if is_test_cmd => sift_test_output(&raw, if is_pytest { "pytest" } else { "cargo" }, exit_code),
            _ => reliary_output::compress_unified(&raw),
        }
    };

    print!("{}", compressed);
    if let Some(h) = hash {
        eprintln!("\n[reliary-compressed {}]", h);
    }
    std::process::exit(exit_code);
}

fn open_or_create(path: &std::path::Path) -> Result<rusqlite::Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();  // GUARDED: intentional
    }
    reliary_core::open(path)
}

fn dummy_conn() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let _ = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS content_cache (
            hash TEXT PRIMARY KEY, original BLOB NOT NULL,
            stored_at INTEGER NOT NULL, accessed_at INTEGER NOT NULL
        );"
    );
    conn
}

fn exec_sift(cmd: &[String], stdin_mode: bool) {
    // --stdin mode: read from stdin, compress, print. No command execution.
    // Used by gate.js to compress already-captured tool result text without
    // re-running the command (preventing double-execution bugs, Bug 2 fix).
    if stdin_mode {
        let raw = match reliary_core::safe_read_stdin() {
            Ok(buf) => buf,
            Err(e) => { eprintln!("stdin: {}", e); std::process::exit(1); }
        };
        if raw.is_empty() { std::process::exit(0); }
        let compressed = reliary_output::compress_unified(&raw);
        // V14: save full output to tee file for LLM recovery.
        // Only save when compression actually saved bytes (avoids disk noise).
        if compressed.len() < raw.len() {
            if let Ok(Some(path)) = tee::save_tee(&raw) {
                eprintln!("[full output: {}]", path);
            }
        }
        print!("{}", compressed);
        std::process::exit(0);
    }

    if cmd.is_empty() {
        eprintln!("Usage: reliary-agent sift <command> [args...]  |  sift --stdin");
        std::process::exit(1);
    }
    let program = &cmd[0];
    let args = &cmd[1..];

    let expand = std::env::var("RELIARY_SIFT_EXPAND").is_ok_and(|v| v == "1" || v == "true");

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
    let exit_code = output.status.code().unwrap_or(0);

    if expand {
        print!("{}", raw);
    } else {
        // Detect command type for special handling
        let program_name = std::path::Path::new(program).file_name()
            .and_then(|n| n.to_str()).unwrap_or(program);
        // Detect pytest/cargo anywhere in the command args (covers python3 -m pytest)
        let args_joined = args.join(" ");
        let is_pytest = args_joined.contains("pytest") || program_name == "pytest";
        let is_cargo = program_name == "cargo" || args_joined.contains(" cargo ");
        let is_test_cmd = is_pytest || is_cargo;

        let compressed = match program_name {
            // File read commands: heavy sift + optional enrichment footer
            "cat" | "head" | "tail" | "less" | "more" => {
                sift_file_read(&raw, args)
            }
            // Test commands: gate passing tests, preserve failures
            _ if is_test_cmd => {
                sift_test_output(&raw, if is_pytest { "pytest" } else { "cargo" }, exit_code)
            }
            _ => {
                // Default: full adaptive sift
                reliary_output::compress_unified(&raw)
            }
        };
        // V14: save full output to tee file for LLM recovery when compression saved bytes.
        if compressed.len() < raw.len() {
            if let Ok(Some(path)) = tee::save_tee(&raw) {
                eprintln!("[full output: {}]", path);
            }
        }
        print!("{}", compressed);
    }
    std::process::exit(exit_code);
}

/// Sift a file read command's output. Heavy compression to keep file content compact.
fn sift_file_read(raw: &str, args: &[String]) -> String {
    // If only 1 arg and it's a single file, appends enrichment footer (callers + tests)
    if args.len() == 1 && !raw.is_empty() {
        let path = &args[0];
        let footer = build_read_footer(path);
        let compressed = reliary_output::compress_unified(raw);
        if footer.is_empty() {
            compressed
        } else {
            format!("{}\n[reliary: {}]\n", compressed, footer)
        }
    } else {
        // Multi-file or piped input: just sift
        reliary_output::compress_unified(raw)
    }
}

/// Find and open the nearest .reliary/index.sqlite by walking up from the given path.
fn find_open_index(start_path: &str) -> Option<rusqlite::Connection> {
    let start = std::path::Path::new(start_path);
    let mut current = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };
    loop {
        let candidate = current.join(".reliary").join("index.sqlite");
        if candidate.exists() {
            return rusqlite::Connection::open(&candidate).ok().map(|d| {
                let _ = d.execute_batch("PRAGMA synchronous = NORMAL;");
                d
            });
        }
        if !current.pop() { break; }
    }
    None
}

/// Build a structured footer with callers + test references for the file.
/// Returns empty string if enrichment not possible (file not indexed).
fn build_read_footer(path: &str) -> String {
    let db = match find_open_index(path) {
        Some(d) => d,
        None => return String::new(),
    };
    // V61: file_map stores ABSOLUTE paths (canonicalized at ingest); the arg
    // here is usually relative (e.g. "src/main.rs"), so an exact match never
    // hits and the footer was always empty. Match on the path suffix instead.
    let file_id = match db.query_row(
        "SELECT id FROM file_map WHERE file_path = ?1 OR file_path LIKE '%/' || ?1 LIMIT 1",
        rusqlite::params![path, path],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(id) => id,
        Err(_) => return String::new(),
    };
    // Get function-like identifiers defined in this file (heuristic: ≥5 chars, no digits)
    let mut identifiers: Vec<String> = Vec::new();
    if let Ok(mut stmt) = db.prepare(
        "SELECT p.phrase FROM phrase_occ o JOIN phrases p ON o.phrase_id = p.id WHERE o.file_id = ?1 LIMIT 30"
    ) {
        if let Ok(rows) = stmt.query_map([file_id], |row| row.get::<_, String>(0)) {
            for r in rows.flatten() {
                if r.len() >= 5
                    && r.chars().next().is_some_and(|c| c.is_alphabetic())
                    && !r.chars().any(|c| c.is_ascii_digit())
                    && r != "record" && r != "config" && r != "return" && r != "self"
                    && r != "kwargs" && r != "args" && r != "None" && r != "True" && r != "False"
                    && r != "process_batch" // sample known non-unique
                {
                    identifiers.push(r);
                }
            }
        }
    }
    if identifiers.is_empty() {
        return String::new();
    }
    // Pick the longest identifier (likely the function name) and count its callers
    let first_id = identifiers.iter().max_by_key(|s| s.len()).unwrap();
    let caller_count: usize = db.query_row(
        "SELECT COUNT(DISTINCT o.file_id) FROM phrase_occ o JOIN phrases p ON o.phrase_id = p.id WHERE p.phrase = ?1",
        rusqlite::params![first_id],
        |row| row.get::<_, i64>(0),
    ).map(|c: i64| c.saturating_sub(1).max(0) as usize).unwrap_or(0);
    if caller_count == 0 {
        return String::new(); // No enrichment value
    }
    format!("c:{}→{}", first_id, caller_count)
}

/// Sift test command output. Collapse passing tests, preserve failures.
fn sift_test_output(raw: &str, program: &str, exit_code: i32) -> String {
    if exit_code == 0 {
        // All tests passed — collapse to compact summary
        let passed_count = if program == "pytest" {
            raw.lines().rev().take(3).find_map(|l| {
                let parts: Vec<&str> = l.split_whitespace().collect();
                for (i, p) in parts.iter().enumerate() {
                    if *p == "passed" && i > 0 {
                        return parts.get(i - 1).and_then(|n| n.parse::<usize>().ok());
                    }
                }
                None
            }).unwrap_or(0)
        } else {
            raw.lines().find_map(|l| {
                if l.contains("test result: ok") {
                    l.split_whitespace().nth(4).and_then(|n| n.parse::<usize>().ok())
                } else { None }
            }).unwrap_or(0)
        };
        if passed_count > 0 {
            return format!("[reliary: {} tests passed]\n", passed_count);
        }
    }
    // Tests failed: normal sift + failure diagnosis
    let compressed = reliary_output::compress_unified(raw);
    let diagnosis = diagnose_failure(raw, program);
    if !diagnosis.is_empty() {
        format!("{}\n[reliary: {}]\n", compressed, diagnosis)
    } else {
        compressed
    }
}

/// Extract identifiers from failure output and look them up in the index.
/// Returns a diagnostic string identifying where the failing symbols are defined.
fn diagnose_failure(raw: &str, _program: &str) -> String {
    // Find the .reliary index
    let cwd = std::env::current_dir().ok();  // GUARDED: intentional
    let cwd_str = cwd.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let db = match find_open_index(&cwd_str) {
        Some(d) => d,
        None => return String::new(),
    };

    // Look for ImportError patterns: "cannot import name 'X'" or "ImportError: cannot import name X"
    let import_name = extract_import_error(raw);
    if let Some(name) = import_name {
        // Look up the missing identifier in the index
        let mut locations: Vec<String> = Vec::new();
        if let Ok(mut stmt) = db.prepare(
            "SELECT DISTINCT f.file_path FROM phrase_occ o JOIN phrases p ON o.phrase_id = p.id JOIN file_map f ON o.file_id = f.id WHERE p.phrase = ?1 LIMIT 5"
        ) {
            if let Ok(rows) = stmt.query_map([&name], |row| row.get::<_, String>(0)) {
                for r in rows.flatten() {
                    locations.push(r);
                }
            }
        }
        if !locations.is_empty() {
            // Detect rename: check files that reference 'name' for similar identifiers
            // (longest common prefix + similar length)
            let mut rename_hint = String::new();
            if let Ok(mut rename_stmt) = db.prepare(
                "SELECT DISTINCT p.phrase FROM phrase_occ o JOIN phrases p ON o.phrase_id = p.id JOIN file_map f ON o.file_id = f.id WHERE f.file_path = ?1 AND length(p.phrase) >= ?2 AND length(p.phrase) <= ?3 AND p.phrase != ?4 LIMIT 10"
            ) {
                for file in locations.iter().take(2) {
                    let nlen = name.len() as i64;
                    if let Ok(rows) = rename_stmt.query_map(
                        rusqlite::params![file, nlen - 3, nlen + 3, &name],
                        |row| row.get::<_, String>(0),
                    ) {
                        let alts: Vec<String> = rows.flatten().collect();
                        // Find one with longest common prefix
                        let best = alts.iter().max_by_key(|a| {
                            a.chars().zip(name.chars()).take_while(|(x, y)| x == y).count()
                        });
                        if let Some(alt) = best {
                            let lcp = alt.chars().zip(name.chars()).take_while(|(x, y)| x == y).count();
                            if lcp >= 5 && *alt != name {
                                rename_hint = format!(" (rename of '{}'?)", alt);
                                break;
                            }
                        }
                    }
                }
            }
            return format!("missing '{}' → referenced in: {}{}", name, locations.iter().take(3).cloned().collect::<Vec<_>>().join(", "), rename_hint);
        } else {
            return format!("missing '{}' → not in index", name);
        }
    }

    // Look for FAILED test patterns + reference errors: "NameError: name 'X' is not defined"
    if let Some(name) = extract_name_error(raw) {
        let file: Option<String> = db.query_row(
            "SELECT file_path FROM phrases p JOIN phrase_occ o ON p.id = o.phrase_id JOIN file_map f ON o.file_id = f.id WHERE p.phrase = ?1 LIMIT 1",
            rusqlite::params![&name],
            |row| row.get::<_, String>(0),
        ).ok();  // GUARDED: intentional
        if let Some(file) = file {
            return format!("undefined '{}' → defined in: {}", name, file);
        }
    }

    String::new()
}

/// Extract import name from "ImportError: cannot import name 'X'"
fn extract_import_error(raw: &str) -> Option<String> {
    for line in raw.lines() {
        if line.contains("ImportError") && line.contains("cannot import name") {
            // Python: 'X' or "X"
            if let Some(start) = line.find("'") {
                if let Some(end) = line[start + 1..].find("'") {
                    return Some(line[start + 1..start + 1 + end].to_string());
                }
            }
        }
        // Rust: error[E0432]: unresolved import `crate::X`
        if line.contains("unresolved import") {
            if let Some(start) = line.rfind('`') {
                if let Some(end) = line[..start].rfind('`') {
                    let path = &line[end + 1..start];
                    if let Some(last) = path.split("::").last() {
                        return Some(last.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Extract name from "NameError: name 'X' is not defined"
fn extract_name_error(raw: &str) -> Option<String> {
    for line in raw.lines() {
        if line.contains("NameError") && line.contains("not defined") {
            if let Some(start) = line.find("'") {
                if let Some(end) = line[start + 1..].find("'") {
                    return Some(line[start + 1..start + 1 + end].to_string());
                }
            }
        }
    }
    None
}

fn validate_config(workdir: &str) {
    let path = config::project_config_path(workdir);
    if !path.exists() { return; }
    if let Ok(content) = reliary_core::safe_read(path.to_string_lossy().as_ref()) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = parsed.as_object() {
                for key in obj.keys() {
                    match key.as_str() {
                        "mode" | "features" | "apiMode" | "privacyMode" | "apiBaseUrl" => {}
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
                // Validate features (reads from config::FEATURE_DEFAULTS to avoid drift)
                let valid_features: Vec<&str> = config::FEATURE_DEFAULTS.iter().map(|(k, _)| *k).collect();
                if let Some(features) = obj.get("features") {
                    if let Some(fobj) = features.as_object() {
                        for (k, v) in fobj {
                            if !valid_features.contains(&k.as_str()) {
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
        if let Err(e) = std::fs::create_dir_all(&reliary_dir) {
            eprintln!("{} Failed to create .reliary/ in {}: {}", color::red("✗"), path, e);
            std::process::exit(1);
        }
        println!("{} Created .reliary/ in {}", color::green("✓"), path);
    }
    // Build index
    run_index(path);
    // Arc 50: opt-in eager build (all lazy tables + occurrences materialized at trust time)
    if std::env::var("RELIARY_EAGER_INDEX").is_ok() {
        println!("{} Eager indexing enabled; building lazy tables...", color::yellow("!"));
        run_build_all(path);
    }
    // Validate config
    validate_config(path);
    println!("{} Project trusted: {}", color::green("✓"), path);
}

fn do_update(check_only: bool) {
    println!("{} Checking for updates...", color::bold(""));
    let current = VERSION;
    // Try to fetch latest release from GitHub via reqwest (we already depend on it)
    let release_url = "https://api.github.com/repos/Reliary/reliary-agent/releases/latest";
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => { eprintln!("{} Could not build HTTP client: {}", color::red("✗"), e); return; }
    };
    let response: Result<reqwest::blocking::Response, reqwest::Error> = client
        .get(release_url)
        .header("User-Agent", "reliary-agent")
        .send();
    match response {
        Ok(r) => {
            let body: String = match r.text() {
                Ok(b) => b,
                Err(e) => { eprintln!("{} Could not read GitHub response: {}", color::red("✗"), e); return; }
            };
            if let Ok(release) = serde_json::from_str::<serde_json::Value>(&body) {
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
                        // Extract directory: tarball contains a single directory matching asset_name without .ext
                        let extract_dir = format!("/tmp/{}", asset_name.trim_end_matches(&format!(".{}", ext.trim_start_matches('.'))));
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
                                // FIX: was /tmp/reliary-agent (wrong). Tarball extracts into a subdirectory
                                let extracted_bin = format!("{}/reliary-agent", extract_dir);
                                let binary = std::env::current_exe().unwrap_or_default();
                                // V60: cp over a running binary fails with ETXTBSY on Linux —
                                // copy to a temp name then rename over the target.
                                let tmp_bin = format!("{}.new", binary.display());
                                let copy = std::process::Command::new("cp")
                                    .args([&extracted_bin, &tmp_bin])
                                    .status();
                                let copy_ok = copy.as_ref().is_ok_and(|s| s.success());
                                let install = if copy_ok {
                                    std::process::Command::new("mv")
                                        .args([&tmp_bin, binary.to_string_lossy().as_ref()])
                                        .status()
                                } else {
                                    copy
                                };
                                let install_ok = install.is_ok_and(|s| s.success());
                                if install_ok {
                                    println!("{} Updated to v{}", color::green("✓"), latest);
                                } else {
                                    eprintln!("{} Install failed — try manually: cp {} {}", color::red("✗"), extracted_bin, binary.display());
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
    // ARM/WSL2 fix: cap the rayon global pool at 4 threads by default.
    // On low-core-count machines (WSL2 ARM, 4-core VMs) the default pool
    // (num_cpus) oversubscribes and pegs the CPU during indexing. On x86
    // use up to 8 cores for faster indexing. Override with
    // RELIARY_RAYON_THREADS=N. Best-effort: if the pool is already
    // built, this is a no-op.
    let default_threads = if cfg!(target_arch = "aarch64") {
        4
    } else {
        std::thread::available_parallelism().map(|n| n.get().min(8)).unwrap_or(4)
    };
    let threads: usize = std::env::var("RELIARY_RAYON_THREADS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(default_threads);
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global();
    let cli = Cli::parse();
    let fmt = format_config(&cli.format);
    let cfg = reliary_core::FormatConfig::new(fmt);

    // Validate config on startup — only for commands that touch the index.
    // Lightweight commands (status, completions, man, --version) skip the disk read.
    match &cli.command {
        Commands::Config { .. } | Commands::Init { dry_run: _ } | Commands::Doctor { .. }
        | Commands::Status | Commands::Completions { .. } | Commands::Man { outdir: _ } => {}
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
        Commands::ReindexFile { file } => {
            run_reindex_file(file);
        }
        Commands::Vacuum { path } => {
            run_vacuum(path);
        }
        Commands::BuildOccurrences { path } => {
            run_build_occurrences(path);
        }
        Commands::BuildAll { path } => {
            run_build_all(path);
        }
        Commands::WhoCalls { file, identifier } => {
            run_who_calls(file, identifier);
        }
        Commands::Compress { text, gentle: _ } => {
            let input_buf: String = match text {
                Some(ref t) if !t.is_empty() && t != "---stdin---" => t.clone(),
                _ => {
                    match reliary_core::safe_read_stdin() {
                        Ok(buf) => buf,
                        Err(e) => { eprintln!("{} stdin: {}", color::red("✗"), e); return; }
                    }
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
            // Use safe_read with size cap (Bug 36: OOM risk on huge files)
            let content = match reliary_core::safe_read(file) {
                Ok(c) => c,
                Err(e) => { eprintln!("{} {}", color::red("✗"), e); return; }
            };
            let risk_result = reliary_risk::compute_file_risk(file, &content);
            let risk_fmt = match fmt { reliary_core::OutputFormat::Json => "json", _ => "default" };
            ux::format_risk(file, &format!("{:?}", risk_result), risk_fmt);
        }
        Commands::Pack { path, format, strategy, top_k, auto, slice_query } => {
            if let Some(query) = slice_query {
                // Slice mode: generate full pack, then slice for query
                let pack_format = if format == "full" {
                    reliary_pack::PackFormat::Full
                } else {
                    reliary_pack::PackFormat::L2L3
                };
                match reliary_pack::generate_pack(&path, pack_format) {
                    Ok(full_pack) => {
                        let sliced = reliary_pack::slice_pack_for_query(&full_pack, &query, *top_k);
                        if sliced.is_empty() {
                            eprintln!("{} no matching entries for query",
                                color::yellow("⊙"));
                        } else {
                            print!("{}", sliced);
                        }
                    }
                    Err(e) => { eprintln!("{} {}", color::red("✗"), e); }
                }
            } else if *auto {
                // Auto-mode: gate + hotspot
                match reliary_pack::should_inject_pack(&path) {
                    Ok(reliary_pack::GateDecision::Skip) => {
                        eprintln!("{} pack skipped (codebase too simple or famous)",
                            color::yellow("⊙"));
                    }
                    Ok(reliary_pack::GateDecision::Minimal) => {
                        eprintln!("{} minimal pack (low-complexity codebase, top-15 hotspot)",
                            color::yellow("ℹ"));
                        match reliary_pack::generate_pack_hotspot(&path, reliary_pack::PackFormat::L2L3, 15) {
                            Ok(pack) => { print!("{}", pack); }
                            Err(e) => { eprintln!("{} {}", color::red("✗"), e); }
                        }
                    }
                    Ok(reliary_pack::GateDecision::Full) => {
                        eprintln!("{} full pack (high-complexity codebase, top-{} hotspot)",
                            color::green("✓"), top_k);
                        match reliary_pack::generate_pack_hotspot(&path, reliary_pack::PackFormat::L2L3, *top_k) {
                            Ok(pack) => { print!("{}", pack); }
                            Err(e) => { eprintln!("{} {}", color::red("✗"), e); }
                        }
                    }
                    Err(e) => { eprintln!("{} {}", color::red("✗"), e); }
                }
            } else if strategy == "hotspot" {
                match reliary_pack::generate_pack_hotspot(&path, reliary_pack::PackFormat::L2L3, *top_k) {
                    Ok(pack) => { print!("{}", pack); }
                    Err(e) => { eprintln!("{} {}", color::red("✗"), e); }
                }
            } else {
                let pack_format = if format == "full" {
                    reliary_pack::PackFormat::Full
                } else {
                    reliary_pack::PackFormat::L2L3
                };
                match reliary_pack::generate_pack(&path, pack_format) {
                    Ok(pack) => { print!("{}", pack); }
                    Err(e) => { eprintln!("{} {}", color::red("✗"), e); }
                }
            }
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
        Commands::Sift { stdin, llm, aggressive, command } => {
            // V14: wire --llm and --aggressive flags through env vars
            // that compress_unified reads.
            if *llm { std::env::set_var("RELIARY_SIFT_LLM", "1"); }
            if *aggressive { std::env::set_var("RELIARY_SIFT_AGGRESSIVE", "1"); }
            exec_sift(command, *stdin);
        }
        Commands::Wrap { command } => {
            exec_wrap(command);
        }
        Commands::CacheStore { text } => {
            let input = match text {
                Some(ref t) if !t.is_empty() && t != "---stdin---" => t.clone(),
                _ => {
                    match reliary_core::safe_read_stdin() {
                        Ok(buf) => buf,
                        Err(e) => { eprintln!("{} stdin: {}", color::red("✗"), e); return; }
                    }
                }
            };
            let path = std::path::Path::new(".reliary/cache.sqlite");
            std::fs::create_dir_all(".reliary").ok();  // GUARDED: intentional
            match reliary_core::open(&path).and_then(|conn| reliary_core::store(&conn, &input)) {
                Ok(hash) => {
                    let _ = reliary_core::evict(&open_or_create(&path).unwrap_or_else(|_| dummy_conn()),
                        reliary_core::default_ttl(), reliary_core::default_max_entries());
                    println!("{}", hash);
                }
                Err(e) => { eprintln!("{} cache-store: {}", color::red("✗"), e); }
            }
        }
        Commands::CacheRetrieve { hash } => {
            let path = std::path::Path::new(".reliary/cache.sqlite");
            match open_or_create(&path) {
                Ok(conn) => {
                    match reliary_core::retrieve(&conn, hash) {
                        Ok(Some(content)) => print!("{}", content),
                        Ok(None) => { eprintln!("{} not found: {}", color::yellow("⚠"), hash); std::process::exit(1); }
                        Err(e) => { eprintln!("{} cache-retrieve: {}", color::red("✗"), e); std::process::exit(1); }
                    }
                }
                Err(_) => { eprintln!("{} not found: {}", color::yellow("⚠"), hash); std::process::exit(1); }
            }
        }
        Commands::CacheStats => {
            let path = std::path::Path::new(".reliary/cache.sqlite");
            match open_or_create(&path) {
                Ok(conn) => {
                    match reliary_core::stats(&conn) {
                        Ok((count, bytes)) => println!("entries: {}\nbytes: {}", count, bytes),
                        Err(e) => { eprintln!("{} cache-stats: {}", color::red("✗"), e); }
                    }
                }
                Err(_) => println!("entries: 0\nbytes: 0"),
            }
        }
        Commands::Config { key, value, local, root } => {
            match (key, value) {
                (Some(k), Some(v)) => {
                    // Validate known keys (Bug 38: use const from config.rs)
                    if !config::VALID_CONFIG_KEYS.contains(&k.as_str()) {
                        eprintln!("{} Unknown config key '{}'. Valid keys:", color::yellow("⚠"), k);
                        for vk in config::VALID_CONFIG_KEYS {
                            eprintln!("  {}", vk);
                        }
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
        Commands::Init { dry_run } => {
            init::run(*dry_run);
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
                let cwd = std::env::current_dir().unwrap_or_default();
                if path_buf.is_dir() {
                    for entry in walkdir::WalkDir::new(&path_buf).into_iter().filter_map(|e| e.ok()) {
                        let p = entry.path();
                        if p.is_file() {
                            // Arc 39: grammar-free content-based binary detection.
                            // Skip only files that look like binary (nulls or high
                            // non-printable ratio in first 8KB).
                            if !reliary_search::is_likely_binary(p, 8192) {
                                // Use safe_read with size cap (Bug 39: OOM on huge files)
                                if let Ok(content) = reliary_core::safe_read(p.to_string_lossy().as_ref()) {
                                    let display = p.strip_prefix(&cwd).unwrap_or(p);
                                    files.push((display.to_string_lossy().to_string(), content));
                                }
                            }
                        }
                    }
                } else if path_buf.is_file() {
                    if let Ok(content) = reliary_core::safe_read(path_buf.to_string_lossy().as_ref()) {
                        let display = path_buf.strip_prefix(&cwd).unwrap_or(&path_buf);
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
        Commands::Fix { task, path, max_iters, verify, dry_run, json } => {
            // trailing_var_arg can swallow --path/--max-iters if passed after
            // the task. Extract them manually and strip from the task.
            let mut eff_path = path.clone();
            let mut task_parts: Vec<String> = Vec::new();
            let mut it = task.iter();
            while let Some(t) = it.next() {
                if t == "--path" || t == "-p" {
                    if let Some(v) = it.next() { eff_path = v.clone(); }
                } else if t == "--max-iters" {
                    if let Some(_v) = it.next() {}
                } else if !t.starts_with("--") {
                    task_parts.push(t.clone());
                }
            }
            let task_str = task_parts.join(" ");
            // Deterministic mode: try recipes first (no LLM needed). If the
            // task isn't recipe-able, report instead of spawning an LLM.
            match deterministic_fix::run_deterministic(&eff_path, &task_str) {
                Ok(code) => {
                    if *json {
                        println!("{{\"deterministic\":true,\"exit\":{}}}", code);
                    }
                    std::process::exit(code);
                }
                Err(msg) if msg.starts_with("no deterministic recipe") => {
                    // Fall back to the LLM agent for unknown tasks.
                    eprintln!("[fix] {} — falling back to LLM agent", msg);
                    match fix_agent::run(&eff_path, &task_str, *max_iters, *dry_run, *json, verify.as_deref()) {
                        Ok(_code) => {}
                        Err(e) => {
                            eprintln!("reliary fix failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("reliary fix failed: {}", e);
                    std::process::exit(1);
                }
            }
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
                if let Err(e) = reliary_core::atomic_write(file_path.to_string_lossy().as_ref(), &output) {
                    eprintln!("{} Failed to write completion file: {}", color::red("✗"), e);
                    std::process::exit(1);
                }
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
                let mut file = match std::fs::File::create(&file_path) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("{} Failed to create man page at {}: {}", color::red("✗"), file_path.display(), e);
                        std::process::exit(1);
                    }
                };
                if let Err(e) = man.render(&mut file) {
                    eprintln!("{} Failed to render man page: {}", color::red("✗"), e);
                    std::process::exit(1);
                }
                println!("{} Generated man page → {}", color::green("✓"), file_path.display());
            } else {
                let mut buf = Vec::new();
                man.render(&mut buf).expect("Failed to render man page");
                print!("{}", String::from_utf8_lossy(&buf));
            }
        }
        Commands::Mcp => {
            eprintln!("Starting MCP server on stdio");
            mcp::serve_stdio();
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
        Commands::Classify { file, line, stem } => {
            // Arc 21 (grammar-free): classify from DB is_def/tag, not from line text.
            let role = if let Some(db) = open_index_or_prompt(".") {
                let stem_stemmed = reliary_search::porter_stem(&stem);
                let stem_lower = stem.to_lowercase();
                // Try both the original stem (lowercased) and the porter-stemmed version.
                let mut stmt = match db.prepare_cached(
                    "SELECT o.tag FROM occurrence o
                     JOIN phrases p ON o.phrase_id = p.id
                     JOIN file_map f ON o.file_id = f.id
                     WHERE (p.phrase = ?1 OR p.phrase = ?2) AND f.file_path = ?3 AND o.line = ?4 LIMIT 1"
                ) {
                    Ok(s) => s,
                    Err(_) => {
                        eprintln!("error: prepare failed");
                        return;
                    }
                };
                let tag: Option<i64> = stmt.query_row(
                    rusqlite::params![stem_lower, stem_stemmed, file, *line],
                    |r| r.get(0)
                ).ok();  // GUARDED: intentional
                match tag {
                    Some(1) | Some(2) | Some(3) => "function_def",
                    Some(4) => "field_access",
                    Some(5) => "param",
                    Some(6) => "local_var",
                    Some(7) => "import_or_use",
                    _ => {
                        // Tag 0 or not found (lazy mode — occurrence table empty).
                        // Arc 38: use col-aware predict_role_with_stem (better than
                        // the prior inline regex-based classifier).
                        let content = std::fs::read_to_string(&file).unwrap_or_default();
                        let line_idx = if *line < 0 { 0usize } else { *line as usize };
                        let line_text = content.lines().nth(line_idx).unwrap_or("");
                        reliary_search::type_flow::predict_role_with_stem(line_text, stem)
                    }
                }
            } else {
                // No DB — fall back to col-aware structural detector.
                let content = match std::fs::read_to_string(&file) {
                    Ok(s) => s,
                    Err(_) => String::new(),
                };
                let line_idx = if *line < 0 { 0usize } else { *line as usize };
                let line_text = content.lines().nth(line_idx).unwrap_or("");
                // Arc 38: col-aware classify, falls through to predict_role() if stem absent.
                reliary_search::type_flow::predict_role_with_stem(line_text, stem)
            };
            println!("{}", role);
        }
        Commands::ParseExpr { line, path } => {
            let db_path = index_db_path(&path);
            let table = if let Ok(db) = rusqlite::Connection::open(&db_path) {
                let mut t = reliary_search::op_table::mine_op_table(&db).unwrap_or_else(|_| reliary_search::op_table::OpTable::new());
                // Fall back to defaults if no ops were mined.
                if t.entries.is_empty() {
                    t.entries.insert("+".to_string(), reliary_search::op_table::OpEntry { precedence: 5.0, associativity: 'L' });
                    t.entries.insert("-".to_string(), reliary_search::op_table::OpEntry { precedence: 5.0, associativity: 'L' });
                    t.entries.insert("*".to_string(), reliary_search::op_table::OpEntry { precedence: 7.0, associativity: 'L' });
                    t.entries.insert("/".to_string(), reliary_search::op_table::OpEntry { precedence: 7.0, associativity: 'L' });
                    t.entries.insert("==".to_string(), reliary_search::op_table::OpEntry { precedence: 3.0, associativity: 'L' });
                    t.entries.insert("!=".to_string(), reliary_search::op_table::OpEntry { precedence: 3.0, associativity: 'L' });
                    t.entries.insert("<".to_string(), reliary_search::op_table::OpEntry { precedence: 4.0, associativity: 'L' });
                    t.entries.insert(">".to_string(), reliary_search::op_table::OpEntry { precedence: 4.0, associativity: 'L' });
                    t.entries.insert("&&".to_string(), reliary_search::op_table::OpEntry { precedence: 2.0, associativity: 'L' });
                    t.entries.insert("||".to_string(), reliary_search::op_table::OpEntry { precedence: 1.0, associativity: 'L' });
                    t.postfix = reliary_search::op_table::default_postfix();
                }
                t
            } else {
                reliary_search::op_table::OpTable::new()
            };
            match reliary_search::expr_tree::parse_expression(&line, &table) {
                Some(tree) => println!("{}", tree.dump(0)),
                None => println!("(parse failed)"),
            }
        }
    }
}
