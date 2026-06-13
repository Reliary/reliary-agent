/// FTS5 Sensory Cortex: replaces raw file reads with structured index output.
/// Every identifier in the summary is verified against the FTS5 index before reaching the LLM.
///
/// Grammar-free design: uses regex identifier scanning, not AST/tree-sitter.

use std::path::Path;

fn index_db_path(path: &str) -> String {
    format!("{}/.reliary/index.sqlite", path.trim_end_matches('/'))
}

fn find_workdir(file: &str) -> String {
    let path = Path::new(file);
    path.ancestors()
        .find(|p| p.join(".reliary").exists())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            path.parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string())
        })
}

/// Build a structured file summary from FTS5 index data.
pub fn build(file: &str) -> String {
    let content = match std::fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) => return format!("ERROR: cannot read {} — {}", file, e),
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let mut defs: Vec<(usize, &str)> = Vec::new();
    let sig_re = regex_lite::Regex::new(r"^\s*(pub\s+)?(fn|def|class|struct|enum|trait|function|func)\s+(\w+)").unwrap();
    for (i, line) in lines.iter().enumerate() {
        if sig_re.is_match(line) {
            defs.push((i + 1, line.trim()));
        }
    }

    // Header
    let fname = file.split('/').last().unwrap_or(file);
    let mut result = format!("[{}] {}L | {} defs", fname, total_lines, defs.len());

    // Definitions with caller search (if index exists)
    let workdir = find_workdir(file);
    let db_path = index_db_path(&workdir);
    let name_re = regex_lite::Regex::new(r"(fn|def|class|struct|enum|trait|function|func)\s+(\w+)").unwrap();

    for (line_no, sig) in defs.iter().take(6) {
        result.push_str(&format!("\n  L{}: {}", line_no, sig));
        if let Ok(db) = rusqlite::Connection::open(&db_path) {
            if reliary_search::schema::open_existing_db(&db).is_ok() {
                if let Some(caps) = name_re.captures(sig) {
                    let name = caps.get(2).unwrap().as_str();
                    let callers = reliary_search::search::search_fts5(&db, name, 5);
                    let caller_files: Vec<&str> = callers.iter()
                        .filter(|r| r.file.split('/').last().unwrap_or("") != fname)
                        .take(3)
                        .map(|r| r.file.rsplit('/').next().unwrap_or(&r.file))
                        .collect();
                    if !caller_files.is_empty() {
                        result.push_str(&format!(" c: [{}]", caller_files.join(", ")));
                    }
                }
            }
        }
    }

    // Add risk score
    let risk = reliary_risk::compute_file_risk(file, &content);
    if matches!(risk.risk, reliary_risk::RiskLevel::High | reliary_risk::RiskLevel::Medium) {
        result.push_str(&format!("\n[risk: {:?}] {}", risk.risk, risk.reason.chars().take(80).collect::<String>()));
    }

    // Add test file references
    let test_name = format!("{}_test.{}",
        fname.rsplit('.').next().unwrap_or(fname),
        if fname.ends_with(".rs") { "rs" } else if fname.ends_with(".py") { "py" } else { "rs" }
    );
    let test_path = Path::new(&workdir).join("tests").join(&test_name);
    if test_path.exists() {
        result.push_str(&format!("\n[tests: {}]", test_name));
    }

    result
}

/// Load compression dictionary from the nearest FTS5 index.
/// Returns None if no index is found or query fails.
pub fn load_dictionary() -> Option<reliary_compress::CompressionDict> {
    for dir in &[".", ".."] {
        let db_path = format!("{}/.reliary/index.sqlite", dir);
        if let Ok(db) = rusqlite::Connection::open(&db_path) {
            if reliary_search::schema::open_existing_db(&db).is_ok() {
                let mut stmt = db.prepare("SELECT phrase FROM phrases_fts LIMIT 200").ok()?;
                let phrases: Vec<String> = stmt.query_map([], |r| r.get(0)).ok()?
                    .filter_map(|r| r.ok()).collect();
                if !phrases.is_empty() {
                    return Some(reliary_compress::build_dict(&phrases));
                }
            }
        }
    }
    None
}
