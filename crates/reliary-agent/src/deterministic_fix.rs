// reliary fix — deterministic mode.
// No LLM in the loop. Task text is mapped to a fixed recipe; each recipe is a
// sequence of existing library calls + edits + verify. Compiler-error mode
// drives fixes from `cargo check` output directly.

use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum FixTask {
    /// Add a doc comment above a symbol's definition.
    AddDocComment { symbol: String, text: String },
    /// Rename a symbol everywhere (all references).
    Rename { from: String, to: String },
    /// Delete a definition if it has no callers (dead).
    RemoveUnused { symbol: String },
    /// Add an import of a crate/module to the file that uses it.
    AddImport { import: String },
    /// Compiler-driven: parse cargo check output and fix each error.
    CompilerErrors,
    /// Unknown task text — not deterministically fixable.
    Unknown(String),
}

/// Map task text to a FixTask by pattern match (deterministic).
pub fn parse_task(task: &str) -> FixTask {
    let t = task.to_lowercase();
    if t.contains("compiler") || t.contains("build error") || t.contains("cargo check") {
        return FixTask::CompilerErrors;
    }
    if let Some(rest) = t.split("doc comment").nth(1) {
        // "add doc comment to X" / "add a doc comment above X"
        for sep in &[" to ", " above ", " for ", " on "] {
            if let Some(sym) = rest.split(sep).nth(1) {
                let sym = sym.trim().trim_matches('"').trim_matches('`').to_string();
                if !sym.is_empty() {
                    return FixTask::AddDocComment { symbol: sym, text: "Doc comment for this symbol.".to_string() };
                }
            }
        }
    }
    if let Some(rest) = t.split("rename ").nth(1) {
        if let Some(to) = rest.split(" to ").nth(1) {
            let from = rest.split(" to ").next().unwrap_or("").trim().to_string();
            let to = to.trim().trim_matches('"').trim_matches('`').to_string();
            if !from.is_empty() && !to.is_empty() {
                return FixTask::Rename { from, to };
            }
        }
    }
    if t.contains("dead") && t.contains("remove") {
        return FixTask::RemoveUnused { symbol: extract_symbol(&t) };
    }
    if t.contains("import") {
        return FixTask::AddImport { import: extract_import(&t) };
    }
    FixTask::Unknown(t.to_string())
}

fn extract_symbol(t: &str) -> String {
    // "remove unused X" — last token.
    t.split_whitespace().last().unwrap_or("").trim_matches('"').to_string()
}
fn extract_import(t: &str) -> String {
    // "add import of Y" — after " of "
    t.split(" of ").nth(1).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// Parse cargo check output into (file, line, code, message) tuples.
pub fn parse_cargo_errors(out: &str) -> Vec<(String, usize, String, String)> {
    let mut errs = Vec::new();
    let re = regex::Regex::new(r"^(.*?\.rs):(\d+):(\d+): error\[(E\d+)\]: (.*)$").unwrap();
    for line in out.lines() {
        if let Some(c) = re.captures(line) {
            errs.push((
                c[1].to_string(),
                c[2].parse().unwrap_or(0),
                c[4].to_string(),
                c[5].to_string(),
            ));
        }
    }
    errs
}

// ---------------------------------------------------------------------------
// Deterministic recipes
// ---------------------------------------------------------------------------

pub fn run_deterministic(path: &str, task: &str) -> Result<i32, String> {
    match parse_task(task) {
        FixTask::CompilerErrors => run_compiler_loop(path, 5),
        FixTask::AddDocComment { symbol, text } => recipe_add_doc(path, &symbol, &text),
        FixTask::Rename { from, to } => recipe_rename(path, &from, &to),
        FixTask::RemoveUnused { symbol } => recipe_remove_unused(path, &symbol),
        FixTask::AddImport { import } => recipe_add_import(path, &import),
        FixTask::Unknown(t) => Err(format!("no deterministic recipe for task: {}", t)),
    }
}

fn run_compiler_loop(path: &str, max_rounds: usize) -> Result<i32, String> {
    let mut fixed = 0usize;
    for _ in 0..max_rounds {
        let out = check_cmd(path, "cargo", &["check", "--message-format=short"]);
        let errs = parse_cargo_errors(&out);
        if errs.is_empty() {
            return Ok(0); // clean
        }
        let mut applied_any = false;
        for (file, line, code, msg) in &errs {
            let full = PathBuf::from(path).join(file);
            if !full.exists() { continue; }
            let content = match std::fs::read_to_string(&full) { Ok(c) => c, Err(_) => continue };
            let src_line = content.lines().nth(line.saturating_sub(1)).unwrap_or("");
            // Recipe per error code.
            let (old, new) = match code.as_str() {
                "E0425" => fix_e0425(src_line, msg), // cannot find value in this scope
                "E0435" => fix_e0435(src_line),       // cannot use non-constant value
                "E0252" => fix_e0252(src_line, msg), // duplicate import
                "E0255" => fix_e0255(src_line),       // use of undeclared type/module
                "E0404" => fix_e0404(src_line),       // expected trait, found type
                _ => (None, None),
            };
            if let (Some(o), Some(n)) = (old, new) {
                if let Ok(true) = reliary_edit::apply_edit(path, file, &o, &n) {
                    applied_any = true;
                    fixed += 1;
                }
            }
        }
        if !applied_any {
            return Ok(1); // nothing fixable
        }
    }
    Ok(if fixed > 0 { 1 } else { 2 })
}

fn check_cmd(path: &str, bin: &str, args: &[&str]) -> String {
    let out = std::process::Command::new(bin)
        .args(args)
        .current_dir(path)
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr),
        Err(e) => format!("check failed: {}", e),
    }
}

fn recipe_add_doc(path: &str, symbol: &str, text: &str) -> Result<i32, String> {
    // def(symbol) → insert /// text above the definition line.
    let db = open_index(path)?;
    let cands = reliary_search::type_flow::top_candidate_definitions(&db, symbol);
    let Some((fp, ln, _)) = cands.into_iter().next() else {
        return Err(format!("no definition found for {}", symbol));
    };
    let full = PathBuf::from(path).join(&fp);
    let content = std::fs::read_to_string(&full).map_err(|e| format!("read: {}", e))?;
    let mut lines: Vec<&str> = content.lines().collect();
    let idx = (ln as usize).min(lines.len().saturating_sub(1));
    let doc_line = format!("/// {}", text);
    lines.insert(idx, &doc_line);
    let out = lines.join("\n") + "\n";
    std::fs::write(&full, &out).map_err(|e| format!("write: {}", e))?;
    Ok(0)
}

fn recipe_rename(path: &str, from: &str, to: &str) -> Result<i32, String> {
    // refs(from) → replace every occurrence in each file (byte-level exact).
    let db = open_index(path)?;
    let Some(pid) = reliary_search::symbol::phrase_id_for(&db, from).ok().flatten() else {
        return Err(format!("no references found for {}", from));
    };
    let mut stmt = db.prepare(
        "SELECT f.file_path FROM occurrence o JOIN file_map f ON f.id=o.file_id WHERE o.phrase_id=?1",
    ).map_err(|e| e.to_string())?;
    let rows = stmt.query_map(rusqlite::params![pid], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    let mut n = 0;
    for fp in rows {
        let full = PathBuf::from(path).join(&fp);
        let content = std::fs::read_to_string(&full).map_err(|e| e.to_string())?;
        if content.contains(from) {
            let out = content.replace(from, to);
            std::fs::write(&full, &out).map_err(|e| e.to_string())?;
            n += 1;
        }
    }
    Ok(if n > 0 { 0 } else { 2 })
}

fn recipe_remove_unused(path: &str, symbol: &str) -> Result<i32, String> {
    // def(symbol) → confirm 0 callers → delete the function block.
    let db = open_index(path)?;
    let cands = reliary_search::type_flow::top_candidate_definitions(&db, symbol);
    let Some((fp, ln, _)) = cands.into_iter().next() else {
        return Err(format!("no definition found for {}", symbol));
    };
    // Count callers (non-def occurrences).
    let pid = reliary_search::symbol::phrase_id_for(&db, symbol).ok().flatten();
    let Some(pid) = pid else { return Err("no phrase".into()) };
    let callers: i64 = db.query_row(
        "SELECT COUNT(*) FROM occurrence WHERE phrase_id=?1 AND is_def=0",
        rusqlite::params![pid], |r| r.get(0),
    ).unwrap_or(0);
    if callers > 0 {
        return Err(format!("{} has {} callers — not unused", symbol, callers));
    }
    // Delete the block: find function body range via brace graph.
    let full = PathBuf::from(path).join(&fp);
    let content = std::fs::read_to_string(&full).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = content.lines().collect();
    let start = (ln as usize).min(lines.len().saturating_sub(1));
    let mut end = start;
    let mut depth = 0i32;
    let mut started = false;
    for (i, l) in lines.iter().enumerate().skip(start) {
        // V60: count braces outside strings and line comments so
        // `let s = "}";` or `// {` don't corrupt the depth.
        let (o, c) = count_braces_outside_strings(l);
        if o > 0 { started = true; }
        if started { depth += o - c; }
        // Single-line def (`fn foo() { ... }` on one line) closes on itself.
        if started && depth <= 0 && (i > start || (i == start && o > 0 && c >= o)) {
            end = i + 1;
            break;
        }
    }
    if !started {
        return Err(format!("no brace block found for {} at {}:{}", symbol, fp, ln + 1));
    }
    let mut out: Vec<&str> = Vec::new();
    out.extend_from_slice(&lines[..start]);
    out.extend_from_slice(&lines[end..]);
    let mut joined = out.join("\n");
    if content.ends_with('\n') && !joined.ends_with('\n') {
        joined.push('\n');
    }
    std::fs::write(&full, joined).map_err(|e| e.to_string())?;
    Ok(0)
}

/// Count `{` and `}` outside string literals and line comments.
fn count_braces_outside_strings(line: &str) -> (i32, i32) {
    let mut o = 0i32;
    let mut c = 0i32;
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    let mut in_char = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if escaped { escaped = false; }
            else if b == b'\\' { escaped = true; }
            else if b == b'"' { in_str = false; }
        } else if in_char {
            if escaped { escaped = false; }
            else if b == b'\\' { escaped = true; }
            else if b == b'\'' { in_char = false; }
        } else {
            match b {
                b'"' => in_str = true,
                b'\'' => in_char = true,
                b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => break, // line comment
                b'{' => o += 1,
                b'}' => c += 1,
                _ => {}
            }
        }
        i += 1;
    }
    (o, c)
}

fn recipe_add_import(path: &str, import: &str) -> Result<i32, String> {
    // search(import) → first .rs file → insert "use {import};" near the top.
    let db = open_index(path)?;
    let hits = reliary_search::search::search_fts5(&db, import, 8);
    let Some(h) = hits.into_iter().find(|h| h.file.ends_with(".rs")) else {
        return Err(format!("no .rs file mentions {}", import));
    };
    let full = PathBuf::from(path).join(&h.file);
    let content = std::fs::read_to_string(&full).map_err(|e| e.to_string())?;
    if content.contains(&format!("use {}", import)) {
        return Ok(0);
    }
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let insert_at = lines.iter().position(|l| l.starts_with("use ") || l.starts_with("pub use "))
        .unwrap_or(lines.len().min(3));
    lines.insert(insert_at, format!("use {};", import));
    std::fs::write(&full, lines.join("\n") + "\n").map_err(|e| e.to_string())?;
    Ok(0)
}

fn open_index(path: &str) -> Result<rusqlite::Connection, String> {
    let db_path = format!("{}/.reliary/index.sqlite", path);
    rusqlite::Connection::open(&db_path).map_err(|e| format!("cannot open index at {}: {}", db_path, e))
}

/// E0425 (cannot find value in this scope): if the line is an unknown
/// identifier use, prefix with `let _ = ` as a conservative no-op? No —
/// too speculative. Instead return None (unsafe to auto-fix).
fn fix_e0425(_line: &str, _msg: &str) -> (Option<String>, Option<String>) {
    (None, None)
}

/// E0435 (cannot use non-constant value in const): wrap in `const`-safe form is
/// not generic; return None (unsafe).
fn fix_e0435(_line: &str) -> (Option<String>, Option<String>) {
    (None, None)
}

/// E0252 (duplicate import): drop the second duplicate `use` — safe.
fn fix_e0252(line: &str, _msg: &str) -> (Option<String>, Option<String>) {
    if line.trim_start().starts_with("use ") {
        (Some(line.to_string()), Some(String::new()))
    } else {
        (None, None)
    }
}

/// E0255 (use of undeclared type/module): no generic fix.
fn fix_e0255(_line: &str) -> (Option<String>, Option<String>) {
    (None, None)
}

/// E0404 (expected trait, found type): no generic fix.
fn fix_e0404(_line: &str) -> (Option<String>, Option<String>) {
    (None, None)
}
