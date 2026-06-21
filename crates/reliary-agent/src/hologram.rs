//! Hologram renderer: query a reliary FTS5 index and emit a compact Markdown
//! "hologram" suitable for LLM context injection.
//!
//! Usage as a library:
//!   - [`render`] builds a hologram from a repo path + optional prompt
//!   - [`render_json`] emits JSON instead of Markdown
//!
//! Used by:
//!   - the `hologram` subcommand in `main.rs`
//!   - the `hologram` daemon endpoint in `daemon.rs`
//!   - the `reliary_hologram` MCP tool in `mcp.rs`

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------- Options ----------

pub struct HologramOpts {
    pub path: PathBuf,
    pub prompt: Option<String>,
    pub top_k: usize,
    pub bytes_cap: usize,
    pub min_score: f64,
    pub include_tests: bool,
    pub json: bool,
    pub no_bodies: bool,
}

// ---------- Index ----------

fn open_index(repo: &Path) -> Result<Connection> {
    let db_path = repo.join(".reliary").join("index.sqlite");
    if !db_path.exists() {
        bail!("index not found at {} — run `reliary-agent trust .` first", db_path.display());
    }
    let db = Connection::open(&db_path).with_context(|| format!("open {}", db_path.display()))?;
    db.execute_batch(
        "PRAGMA synchronous = OFF;
         PRAGMA journal_mode = MEMORY;
         PRAGMA cache_size = -200000;
         PRAGMA mmap_size = 268435456;
         PRAGMA temp_store = MEMORY;",
    )?;
    let version: i32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != reliary_search::schema::SCHEMA_VERSION {
        bail!("schema version mismatch: DB has {}, expected {}. Rebuild via `reliary-agent trust .`", version, reliary_search::schema::SCHEMA_VERSION);
    }
    Ok(db)
}

fn total_files(db: &Connection) -> Result<i64> {
    Ok(db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0))?)
}

fn avg_token_len(db: &Connection) -> Result<f64> {
    Ok(db.query_row("SELECT COALESCE(AVG(token_len), 1) FROM file_stats", [], |r| r.get(0))?)
}

// ---------- Entry gathering ----------

#[derive(Debug, Clone)]
struct FileEntry {
    path: String,
    token_len: i64,
    content: Option<String>,
    fts_score: f64,
    lines: usize,
}

fn list_all_files(db: &Connection) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt = db.prepare(
        "SELECT f.id, f.file_path, COALESCE(s.token_len, 0)
         FROM file_map f
         LEFT JOIN file_stats s ON s.file_id = f.id
         ORDER BY s.token_len DESC",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn collect_entries(db: &Connection, _repo: &Path, prompt: Option<&str>, include_tests: bool) -> Result<Vec<FileEntry>> {
    let all_files = list_all_files(db)?;
    let total = all_files.len() as f64;
    let avg_tl = avg_token_len(db)?;

    let mut fts_scores: HashMap<String, f64> = HashMap::new();
    if let Some(q) = prompt.filter(|s| !s.trim().is_empty()) {
        let results = reliary_search::search::search_fts5(db, q, 200);
        for r in results {
            fts_scores.insert(r.file.clone(), r.score);
        }
    }

    let mut entries: Vec<FileEntry> = all_files
        .into_iter()
        .filter(|(_, path, _)| include_tests || !is_test_path(path))
        .map(|(_id, path, token_len)| {
            let fts_score = fts_scores.get(&path).copied().unwrap_or(0.0);
            FileEntry {
                path,
                token_len,
                content: None,
                fts_score,
                lines: 0,
            }
        })
        .collect();

    for e in &mut entries {
        let size_boost = ((e.token_len as f64).ln() / avg_tl.ln()).max(0.0) * 0.1;
        let bm25 = if e.fts_score > 0.0 {
            let idf = reliary_search::bm25_idf(total, 1.0);
            let tf = (e.fts_score + 1.0).ln();
            let doc_len = e.token_len.max(1) as f64;
            reliary_search::bm25_score(idf, tf, doc_len, avg_tl) + size_boost
        } else {
            size_boost * 0.5
        };
        e.fts_score = bm25;
    }

    entries.sort_by(|a, b| {
        b.fts_score
            .partial_cmp(&a.fts_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(entries)
}

pub fn is_test_path(path: &str) -> bool {
    path.starts_with("tests/") || path.starts_with("test/")
        || path.contains("/tests/") || path.contains("/test/")
        || path.ends_with("_test.rs") || path.ends_with("test.rs")
        || path.ends_with(".test.ts") || path.ends_with(".spec.ts")
        || path.ends_with("_test.py")
}

// ---------- File content loading ----------

fn load_top_content(entries: &mut [FileEntry], repo: &Path, top_n: usize) -> Result<()> {
    for e in entries.iter_mut().take(top_n) {
        let full = repo.join(&e.path);
        match std::fs::read_to_string(&full) {
            Ok(content) => {
                e.lines = content.lines().count();
                e.content = Some(content);
            }
            Err(_) => {
                e.content = None;
                e.lines = 0;
            }
        }
    }
    Ok(())
}

// ---------- Signature extraction ----------

#[derive(Debug, Clone)]
pub struct Signature {
    pub name: String,
    pub line: usize,
    pub end_line: usize,
    #[allow(dead_code)]
    pub sig_text: String,
}

pub fn extract_signatures(path: &str, content: &str) -> Vec<Signature> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "rs" => extract_rust(content),
        "py" => extract_python(content),
        "ts" | "tsx" | "js" | "jsx" => extract_js(content),
        "go" => extract_go(content),
        "c" | "cc" | "cpp" | "cxx" | "h" | "hpp" => extract_cpp(content),
        "md" | "txt" => vec![],
        _ => extract_generic(content),
    }
}

fn extract_rust(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("pub fn ").or_else(|| trimmed.strip_prefix("fn ")) {
            if let Some(name) = rest.split('(').next() {
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    sigs.push(Signature {
                        name: name.to_string(),
                        line: i + 1,
                        end_line: 0,
                        sig_text: line.to_string(),
                    });
                }
            }
        }
    }
    finalize_body_ranges(sigs, content)
}

fn extract_python(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        let rest = trimmed
            .strip_prefix("def ")
            .or_else(|| trimmed.strip_prefix("async def "))
            .or_else(|| trimmed.strip_prefix("class "));
        if let Some(rest) = rest {
            let name = rest
                .split(['(', ':', ' ', '<'])
                .next()
                .unwrap_or("");
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                sigs.push(Signature {
                    name: name.to_string(),
                    line: i + 1,
                    end_line: 0,
                    sig_text: line.to_string(),
                });
            }
        }
    }
    finalize_body_ranges(sigs, content)
}

fn extract_js(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        let name = if let Some(rest) = trimmed.strip_prefix("function ") {
            rest.split('(').next().map(|s| s.to_string())
        } else if let Some(rest) = trimmed.strip_prefix("async function ") {
            rest.split('(').next().map(|s| s.to_string())
        } else if let Some(rest) = trimmed.strip_prefix("const ") {
            rest.split_whitespace().next().and_then(|tok| tok.strip_suffix('=').map(|s| s.to_string()))
        } else if let Some(rest) = trimmed.strip_prefix("class ") {
            rest.split(['{', ' ', '<']).next().map(|s| s.to_string())
        } else {
            None
        };
        if let Some(name) = name {
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') {
                sigs.push(Signature {
                    name,
                    line: i + 1,
                    end_line: 0,
                    sig_text: line.to_string(),
                });
            }
        }
    }
    finalize_body_ranges(sigs, content)
}

fn extract_go(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("func ") {
            if let Some(name) = rest.split('(').next() {
                let name = name.split_whitespace().last().unwrap_or(name);
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    sigs.push(Signature {
                        name: name.to_string(),
                        line: i + 1,
                        end_line: 0,
                        sig_text: line.to_string(),
                    });
                }
            }
        }
    }
    finalize_body_ranges(sigs, content)
}

fn extract_cpp(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("class ")
            .or_else(|| trimmed.strip_prefix("struct "))
            .or_else(|| trimmed.strip_prefix("void "))
            .or_else(|| trimmed.strip_prefix("int "))
            .or_else(|| trimmed.strip_prefix("bool "))
            .or_else(|| trimmed.strip_prefix("static "))
        {
            if let Some(name) = rest.split(['(', '{', ' ']).next() {
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    sigs.push(Signature {
                        name: name.to_string(),
                        line: i + 1,
                        end_line: 0,
                        sig_text: line.to_string(),
                    });
                }
            }
        }
    }
    finalize_body_ranges(sigs, content)
}

fn extract_generic(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.contains('(') && (trimmed.ends_with('{') || trimmed.contains(")")) && !trimmed.starts_with("//") {
            if let Some(name) = trimmed.split('(').next().and_then(|s| s.split_whitespace().last()) {
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    sigs.push(Signature {
                        name: name.to_string(),
                        line: i + 1,
                        end_line: 0,
                        sig_text: line.to_string(),
                    });
                }
            }
        }
    }
    finalize_body_ranges(sigs, content)
}

fn finalize_body_ranges(mut sigs: Vec<Signature>, content: &str) -> Vec<Signature> {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    for i in 0..sigs.len() {
        if i + 1 < sigs.len() {
            sigs[i].end_line = sigs[i + 1].line - 1;
        } else {
            sigs[i].end_line = total;
        }
    }
    sigs
}

// ---------- Summary line ----------

pub fn extract_summary(content: &str, first_sig_line: Option<usize>) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = first_sig_line.unwrap_or(0).saturating_sub(5);
    for &line in &lines[start..first_sig_line.unwrap_or(start)] {
        let trimmed = line.trim();
        if trimmed.starts_with("///") || trimmed.starts_with("//!") {
            let t = trimmed.trim_start_matches("///").trim_start_matches("//!").trim();
            if !t.is_empty() {
                return truncate(t, 80);
            }
        }
        if trimmed.starts_with("//") || trimmed.starts_with("#") {
            let t = trimmed.trim_start_matches("//").trim_start_matches('#').trim();
            if !t.is_empty() {
                return truncate(t, 80);
            }
        }
    }
    for line in &lines {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("//!") && !trimmed.starts_with("///") {
            return truncate(trimmed, 80);
        }
    }
    String::new()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

// ---------- Top identifiers ----------

pub fn top_identifiers(content: &str) -> Vec<(String, usize)> {
    let idents = reliary_search::scan_identifiers(content);
    let mut counts: HashMap<String, usize> = HashMap::new();
    for id in idents {
        *counts.entry(id).or_insert(0) += 1;
    }
    let mut v: Vec<_> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(8);
    v
}

// ---------- Rendering ----------

#[derive(Serialize)]
struct JsonEntry {
    path: String,
    lines: usize,
    score: f64,
    defs: Vec<String>,
    top: Vec<[String; 2]>,
    summary: String,
    body_ranges: Vec<[usize; 2]>,
    stale: bool,
}

fn render_entry_markdown(e: &FileEntry, sigs: &[Signature], summary: &str, top: &[(String, usize)], include_bodies: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("## {} ({} L)\n", e.path, e.lines));
    if !sigs.is_empty() {
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).take(8).collect();
        out.push_str(&format!("  defs: {}\n", names.join(", ")));
    }
    if !top.is_empty() {
        let parts: Vec<String> = top.iter().take(5).map(|(k, c)| format!("{}({})", k, c)).collect();
        out.push_str(&format!("  top: {}\n", parts.join(", ")));
    }
    if !summary.is_empty() {
        out.push_str(&format!("  > {}\n", summary));
    }
    if include_bodies {
        for s in sigs.iter().take(6) {
            out.push_str(&format!("  [body: {}:{}]\n", e.path, s.line));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn render_markdown(
    entries: &[FileEntry],
    indexed_files: i64,
    indexed_bodies: i64,
    prompt: Option<&str>,
    total_matches: usize,
    rendered_count: usize,
    repo_root: &Path,
    bytes_cap: usize,
    include_bodies: bool,
    include_tests: bool,
) -> String {
    let mut out = String::new();
    let rel_root = repo_root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    out.push_str(&format!("# hologram: /{}{}\n", rel_root, prompt.map(|p| format!(" (prompt: \"{}\")", p)).unwrap_or_default()));
    out.push_str(&format!(
        "# {} files indexed, {} phrases, body store hashes: {}\n",
        indexed_files, indexed_bodies, "n/a"
    ));
    out.push_str(&format!(
        "# rendered: top {} of {} matches{}\n\n",
        rendered_count,
        total_matches,
        if include_tests { "" } else { ", tests excluded" }
    ));

    let mut bytes_written = out.len();
    let mut actually_rendered = 0;
    for e in entries {
        let entry_md = if let Some(content) = &e.content {
            let sigs = extract_signatures(&e.path, content);
            let first_sig = sigs.first().map(|s| s.line);
            let summary = extract_summary(content, first_sig);
            let top = top_identifiers(content);
            render_entry_markdown(e, &sigs, &summary, &top, include_bodies)
        } else {
            format!("## {} (file not found, content unavailable)\n", e.path)
        };
        if bytes_written + entry_md.len() > bytes_cap {
            out.push_str(&format!("\n[truncated: byte cap {} reached after {} entries]\n", bytes_cap, actually_rendered));
            out.push_str("[hint: --bytes 100000 to allow more, or --top-k 50]\n");
            break;
        }
        out.push_str(&entry_md);
        bytes_written += entry_md.len();
        actually_rendered += 1;
    }
    if entries.len() > actually_rendered {
        out.push_str(&format!("\n[{} more matches not rendered]\n", entries.len() - actually_rendered));
    }
    out
}

fn render_json(
    entries: &[FileEntry],
    indexed_files: i64,
    indexed_bodies: i64,
    prompt: Option<&str>,
    bytes_cap: usize,
    include_bodies: bool,
) -> Result<String> {
    let mut out_entries: Vec<JsonEntry> = Vec::new();
    let mut bytes: usize = 0;
    for e in entries {
        if let Some(content) = &e.content {
            let sigs = extract_signatures(&e.path, content);
            let summary = extract_summary(content, sigs.first().map(|s| s.line));
            let top = top_identifiers(content);
            let body_ranges: Vec<[usize; 2]> = if include_bodies {
                sigs.iter().take(6).map(|s| [s.line, s.end_line]).collect()
            } else {
                vec![]
            };
            let entry = JsonEntry {
                path: e.path.clone(),
                lines: e.lines,
                score: e.fts_score,
                defs: sigs.iter().map(|s| s.name.clone()).collect(),
                top: top.iter().map(|(k, c)| [k.clone(), c.to_string()]).collect(),
                summary,
                body_ranges,
                stale: false,
            };
            let serialized = serde_json::to_string(&entry)?;
            if bytes + serialized.len() > bytes_cap {
                break;
            }
            bytes += serialized.len();
            out_entries.push(entry);
        }
    }
    let result = serde_json::json!({
        "repo": ".",
        "prompt": prompt,
        "indexed_files": indexed_files,
        "indexed_bodies": indexed_bodies,
        "match_count": entries.len(),
        "rendered_count": out_entries.len(),
        "entries": out_entries,
    });
    Ok(serde_json::to_string_pretty(&result)?)
}

// ---------- Public entry points ----------

pub fn render(opts: &HologramOpts) -> Result<String> {
    if !opts.path.is_dir() {
        bail!("not a directory: {}", opts.path.display());
    }

    let db = open_index(&opts.path)?;
    let indexed_files = total_files(&db)?;

    let mut entries = collect_entries(&db, &opts.path, opts.prompt.as_deref(), opts.include_tests)?;
    let total_matches = entries.len();

    if opts.min_score > 0.0 {
        entries.retain(|e| e.fts_score >= opts.min_score);
    }
    let mut top_candidates: Vec<FileEntry> = entries.iter().take(opts.top_k * 3).cloned().collect();
    load_top_content(&mut top_candidates, &opts.path, opts.top_k * 3)?;

    let top_k: Vec<FileEntry> = top_candidates.into_iter().take(opts.top_k).collect();

    let indexed_bodies: i64 = db.query_row(
        "SELECT COALESCE(SUM(unique_def_count), 0) FROM file_stats",
        [],
        |r| r.get(0),
    )?;

    let stdout = if opts.json {
        render_json(
            &top_k,
            indexed_files,
            indexed_bodies,
            opts.prompt.as_deref(),
            opts.bytes_cap,
            !opts.no_bodies,
        )?
    } else {
        render_markdown(
            &top_k,
            indexed_files,
            indexed_bodies,
            opts.prompt.as_deref(),
            total_matches,
            top_k.len(),
            &opts.path,
            opts.bytes_cap,
            !opts.no_bodies,
            opts.include_tests,
        )
    };
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_rust_sigs() {
        let src = "fn main() {}\npub fn hello() -> i32 { 1 }\nfn _priv() {}";
        let sigs = extract_rust(src);
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["main", "hello", "_priv"]);
        assert_eq!(sigs[0].line, 1);
        assert_eq!(sigs[1].line, 2);
    }

    #[test]
    fn test_extract_python_sigs() {
        let src = "def foo():\n    pass\nasync def bar(x):\n    return x\nclass Baz:\n    pass";
        let sigs = extract_python(src);
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["foo", "bar", "Baz"]);
    }

    #[test]
    fn test_body_ranges() {
        let src = "fn a() {}\nfn b() {}\nfn c() {}\n";
        let sigs = extract_rust(src);
        assert_eq!(sigs[0].line, 1);
        assert_eq!(sigs[0].end_line, 1);
        assert_eq!(sigs[1].line, 2);
        assert_eq!(sigs[1].end_line, 2);
        assert_eq!(sigs[2].line, 3);
        assert_eq!(sigs[2].end_line, 3);
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a long string that exceeds the limit", 10), "a long st…");
    }

    #[test]
    fn test_extract_summary_finds_doc_comment() {
        let src = "/// This is a doc comment\n/// spans two lines\nfn foo() {}\n";
        let s = extract_summary(src, Some(3));
        assert!(s.starts_with("This is a doc comment"));
    }

    #[test]
    fn test_top_identifiers() {
        let src = "let foo = 1; let bar = 2; let foo = 3; let baz = 4;";
        let top = top_identifiers(src);
        let foo_count = top.iter().find(|(k, _)| k == "foo").map(|(_, c)| *c).unwrap_or(0);
        assert_eq!(foo_count, 2);
        assert!(top.len() <= 8);
    }

    #[test]
    fn test_is_test_path() {
        assert!(is_test_path("src/foo_test.rs"));
        assert!(is_test_path("tests/integration.rs"));
        assert!(!is_test_path("src/main.rs"));
        assert!(!is_test_path("crates/reliary-search/src/lib.rs"));
    }
}
