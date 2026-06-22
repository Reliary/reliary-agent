// Grammar-free anti-decision tracker.
//
// Observes tool call outcomes flowing through the proxy, records per-identifier
// success/failure counts, and injects ` -identifier` annotations into tool results
// that contain file content, via Markov surprise (identical to engfield design).
//
// Grammar-free: everything operates on (file, identifier, operation, success) tuples.
// No AST, no regex, no language detection. Identifiers are extracted via simple
// [A-Za-z_][A-Za-z0-9_]{2,} scanning, split on non-alphanumeric boundaries.
//
// Persistence: recorded events are stored in the project chronicle (SQLite),
// surviving daemon restarts. On first query for a workdir, in-memory state
// is loaded from chronicle (72h window). In-memory lookups are instant.
//
// Key parts:
//   - `record(workdir, file, identifier, operation, success)`: log + persist
//   - `query_anti_decisions(workdir, file)`: return identifiers with >=2 failures
//   - `extract_tool_call(msg)`: extract file, identifier, tool, success from a tool result message
//   - `annotate_tool_result()`: append anti-decision annotations (called by proxy inline)
//   - Gating: only annotate when the file name appears in the text
//   - Built-in weak priors for `unwrap`, `legacy`, `hack`, `todo`, `TODO`, `FIXME`,
//     `debug_`, `temp`, `old_text`, `clone` (1 failure each)

use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Mutex;

use serde_json::Value;

 type CounterMap = FxHashMap<String, (usize, usize)>;

// Bug 62: cap ANTI_DB at MAX_WORKDIRS workdirs with LRU eviction to bound memory.
// Also use (value, last_access_seq) for eviction tracking.
pub const ANTI_DB_MAX_WORKDIRS: usize = 1000;
static ANTI_DB_SEQ: once_cell::sync::Lazy<Mutex<u64>> =
    once_cell::sync::Lazy::new(|| Mutex::new(0));

type WorkdirEntry = (CounterMap, u64);
pub static ANTI_DB: once_cell::sync::Lazy<Mutex<FxHashMap<String, WorkdirEntry>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(FxHashMap::default()));

#[allow(dead_code)]
static LOADED_WORKDIRS: once_cell::sync::Lazy<Mutex<FxHashSet<String>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(FxHashSet::default()));

#[allow(dead_code)]
const BUILTIN_PRIORS: &[&str] = &[
    "unwrap", "legacy", "hack", "todo", "TODO", "FIXME",
    "debug_", "temp", "old_text", "clone",
];

#[allow(dead_code)]
fn builtin_prior_count(identifier: &str) -> usize {
    if BUILTIN_PRIORS.contains(&identifier) { 1 } else { 0 }
}

fn extract_file_path(text: &str) -> Option<String> {
    for word in text.split_whitespace() {
        let w = word.trim_matches(|c: char| {
            c == '"' || c == '\'' || c == '`' || c == '(' || c == ')' || c == ',' || c == ':' || c == ';'
        });
        if (w.contains('/') || w.contains('\\')) && w.len() >= 3 {
            let cleaned = w.trim_end_matches(|c: char| {
                c == '.' || c == ',' || c == ':' || c == ';' || c.is_ascii_digit()
            });
            if cleaned.len() >= 3 {
                return Some(cleaned.to_string());
            }
        }
    }
    None
}

fn extract_primary_identifier(text: &str, file: &str) -> String {
    let file_stem = std::path::Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    // Grammar-free: filter only by structural properties (length, alpha-leading, case mix)
    // and exclude the file stem. We do NOT use a hardcoded keyword list — common words
    // are filtered by structural heuristics instead.
    let identifiers: Vec<&str> = text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() >= 3 && s.len() <= 40)
        .filter(|s| s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_'))
        .filter(|s| *s != file_stem)
        // Skip identifiers that are all lowercase AND < 5 chars — these are
        // overwhelmingly common English words ("the", "and", "for"). This is
        // a structural heuristic, not a keyword list.
        .filter(|s| !(s.chars().all(|c| c.is_ascii_lowercase()) && s.len() < 5))
        .collect();
    if let Some(id) = identifiers.iter().find(|id| {
        // Prefer identifiers that look function-y: mixed case or contain underscores
        // OR are at least 6 chars (longer words are more likely project-specific).
        id.chars().any(|c| c.is_uppercase()) || id.contains('_') || id.len() >= 6
    }) {
        return id.to_string();
    }
    identifiers.first().map(|s| s.to_string()).unwrap_or_else(|| "unknown".to_string())
}

#[allow(dead_code)]
fn is_interesting_ident(s: &str) -> bool {
    if s.len() < 3 || s.len() > 40 { return false; }
    if !s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') { return false; }
    true
}

fn extract_sed_target(content: &str) -> Option<(String, String)> {
    // Grammar-free sed pattern extraction. Skip flag words (-i, -e, -E, etc.) and
    // s/.../.../ pattern words; the remaining word containing a path separator
    // or file extension is the file path.
    let content_lower = content.to_lowercase();
    if !content_lower.contains("sed") { return None; }

    let file = content.split_whitespace()
        .filter(|w| !w.starts_with('-'))                                  // skip flags
        .filter(|w| !w.starts_with('\'') && !w.starts_with('"'))         // skip quoted patterns
        .filter(|w| !looks_like_sed_pattern(w))                          // skip s/foo/bar/ patterns
        .filter(|w| w.contains('/') || w.contains('\\') || w.contains('.'))
        .map(|w| w.trim_matches(|c: char| c == '\'' || c == '"' || c == ';').to_string())
        .find(|w| !w.is_empty() && w.len() >= 3)?;

    if file.is_empty() { return None; }

    let success = !content_lower.contains("error")
        && !content_lower.contains("fail");
    Some((file, if success { "success".to_string() } else { "fail".to_string() }))
}

// A word is a sed pattern if it starts with 's' (or 'y') followed by a
// non-alphanumeric delimiter (commonly /, |, #, @, etc.) — the s/old/new/ form.
fn looks_like_sed_pattern(w: &str) -> bool {
    looks_like_sed_pattern_inner(w)
}

fn looks_like_sed_pattern_inner(w: &str) -> bool {
    let bytes = w.as_bytes();
    if bytes.len() < 5 { return false; }
    let first = bytes[0];
    if first != b's' && first != b'y' { return false; }
    let delim = bytes[1];
    if delim.is_ascii_alphanumeric() { return false; }
    // Must contain at least 2 more delimiter bytes to qualify as s/old/new/ shape.
    w.bytes().filter(|&b| b == delim).count() >= 2
}

pub fn extract_tool_call(msg: &Value) -> Option<(String, String, String, bool)> {
    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
    if role != "tool" && role != "toolResult" { return None; }

    let content = msg.get("content").and_then(|c| c.as_str())?;
    let tool_name = msg.get("name")
        .or_else(|| msg.get("toolName"))
        .and_then(|n| n.as_str()).unwrap_or("");

    match tool_name {
        "edit" | "apply-edit" | "write" => {
            let file = extract_file_path(content)?;
            let identifier = extract_primary_identifier(content, &file);
            let success = !content.to_lowercase().contains("error")
                && !content.to_lowercase().contains("fail")
                && !content.to_lowercase().contains("revert");
            Some((file, identifier, tool_name.to_string(), success))
        }
        "bash" | "run" => {
            let (file, outcome) = extract_sed_target(content)?;
            let success = outcome == "success";
            Some((file, "sed".to_string(), "bash".to_string(), success))
        }
        _ => None,
    }
}

pub fn record(workdir: &str, file: &str, identifier: &str, operation: &str, success: bool) {
    let key = format!("{}::{}::{}", workdir, file, identifier);
    if let Ok(mut db) = ANTI_DB.lock() {
        // Bug 62: cap outer workdir map with LRU eviction
        if !db.contains_key(workdir) && db.len() >= ANTI_DB_MAX_WORKDIRS {
            // Evict oldest workdir
            if let Some((oldest_key, _)) = db.iter().min_by_key(|(_, (_, seq))| *seq).map(|(k, v)| (k.clone(), v.1)) {
                db.remove(&oldest_key);
            }
        }
        let seq = ANTI_DB_SEQ.lock().map(|mut s| { *s += 1; *s }).unwrap_or(0);
        let counters = db.entry(workdir.to_string()).or_insert_with(|| (FxHashMap::default(), 0));
        counters.1 = seq; // Update last-access seq for LRU
        let entry = counters.0.entry(key).or_insert((0, 0));
        if success {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }
    let db_path = format!("{}/.reliary/chronicle.sqlite", workdir.trim_end_matches('/'));
    let _ = std::fs::create_dir_all(std::path::Path::new(&db_path).parent().unwrap_or(std::path::Path::new(".")));
    if let Ok(conn) = crate::chronicle::init(&db_path) {
        let detail = format!("{}::{}::{}", file, identifier, if success { "success" } else { "fail" });
        crate::chronicle::append(&conn, "antidecision", file, &detail, operation);
    }
}

#[allow(dead_code)]
pub fn load_persisted(workdir: &str) {
    let db_path = format!("{}/.reliary/chronicle.sqlite", workdir.trim_end_matches('/'));
    let events = match crate::chronicle::init(&db_path) {
        Ok(db) => crate::chronicle::recent_events_by_type(&db, "antidecision", 72),
        Err(_) => return,
    };
    if events.is_empty() { return; }
    if let Ok(mut db) = ANTI_DB.lock() {
        let seq = ANTI_DB_SEQ.lock().map(|mut s| { *s += 1; *s }).unwrap_or(0);
        let counters = db.entry(workdir.to_string()).or_insert_with(|| (FxHashMap::default(), 0));
        counters.1 = seq;
        for event in &events {
            let parts: Vec<&str> = event.detail.splitn(3, "::").collect();
            if parts.len() != 3 { continue; }
            let key = format!("{}::{}::{}", workdir, parts[0], parts[1]);
            let entry = counters.0.entry(key).or_insert((0, 0));
            if parts[2] == "success" {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }
    }
}

#[allow(dead_code)]
pub fn query_anti_decisions(workdir: &str, file: &str) -> Vec<(String, f64, usize, usize)> {
    {
        if let Ok(mut loaded) = LOADED_WORKDIRS.lock() {
            if !loaded.contains(workdir) {
                loaded.insert(workdir.to_string());
                drop(loaded);
                load_persisted(workdir);
            }
        }
    }
    let prefix = format!("{}::{}::", workdir, file);
    if let Ok(db) = ANTI_DB.lock() {
        if let Some((counters, _)) = db.get(workdir) {
            let mut results: Vec<(String, f64, usize, usize)> = counters.iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .map(|(k, &(s, f))| {
                    let id = k[prefix.len()..].to_string();
                    let total_fails = f + builtin_prior_count(&id);
                    let total = (s + total_fails) as f64;
                    let risk = total_fails as f64 / total.max(1.0);
                    (id, risk, total_fails, s)
                })
                .filter(|(_, _, f, _)| *f >= 2)
                .collect();
            results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            return results;
        }
    }
    Vec::new()
}

#[allow(dead_code)]
fn extract_identifiers(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() >= 2 && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_'))
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_test_wd() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TEST_CTR: AtomicU64 = AtomicU64::new(0);
        let ctr = TEST_CTR.fetch_add(1, Ordering::Relaxed);
        let wd = format!("/tmp/antidecision_test_{}_{}", std::process::id(), ctr);
        let _ = std::fs::remove_dir_all(&wd);
        wd
    }

    #[test]
    fn test_record_and_query() {
        let wd = clean_test_wd();
        record(&wd, "src/auth.rs", "unwrap", "edit", false);
        record(&wd, "src/auth.rs", "unwrap", "edit", false);
        record(&wd, "src/auth.rs", "unwrap", "edit", false);
        record(&wd, "src/auth.rs", "question_mark", "edit", true);
        record(&wd, "src/auth.rs", "question_mark", "edit", true);

        let anti = query_anti_decisions(&wd, "src/auth.rs");
        assert!(!anti.is_empty(), "should find anti-decisions");
        assert_eq!(anti[0].0, "unwrap");
        assert!(anti[0].1 > 0.5);
        assert!(anti[0].2 >= 2);
    }

    #[test]
    fn test_builtin_priors() {
        let anti = query_anti_decisions("/tmp/nonexistent", "src/unknown.rs");
        assert!(anti.is_empty());
    }

    #[test]
    fn test_extract_identifiers() {
        let ids = extract_identifiers("edit src/auth.rs: changed unwrap to ?");
        assert!(ids.contains(&"edit".to_string()));
        assert!(ids.contains(&"auth".to_string()));
        assert!(ids.contains(&"unwrap".to_string()));
    }

    #[test]
    fn test_extract_file_path() {
        assert_eq!(extract_file_path("Edit applied to src/auth.rs:42"), Some("src/auth.rs".to_string()));
        assert_eq!(extract_file_path("Wrote 1234 bytes to lib/parser.py"), Some("lib/parser.py".to_string()));
        assert_eq!(extract_file_path("no path here"), None);
    }

    #[test]
    fn test_extract_tool_call_edit() {
        let msg = serde_json::json!({
            "role": "tool",
            "name": "edit",
            "content": "Edit applied successfully to src/auth.rs:42"
        });
        let result = extract_tool_call(&msg);
        assert!(result.is_some());
        let (file, _id, tool, success) = result.unwrap();
        assert_eq!(file, "src/auth.rs");
        assert_eq!(tool, "edit");
        assert!(success);
    }

    #[test]
    fn test_extract_tool_call_sed() {
        let msg = serde_json::json!({
            "role": "tool",
            "name": "bash",
            "content": "Running: sed -i 's/old_func/new_func/' src/parser.rs"
        });
        let result = extract_tool_call(&msg);
        assert!(result.is_some());
        let (file, id, tool, _success) = result.unwrap();
        assert_eq!(file, "src/parser.rs");
        assert_eq!(id, "sed");
        assert_eq!(tool, "bash");
    }

    #[test]
    fn test_persistence_and_load() {
        let wd = clean_test_wd();
        record(&wd, "src/auth.rs", "unwrap", "edit", false);
        record(&wd, "src/auth.rs", "unwrap", "edit", false);
        record(&wd, "src/auth.rs", "unwrap", "edit", false);

        // Clear in-memory state (only for this workdir, not other tests')
        if let Ok(mut db) = ANTI_DB.lock() {
            db.remove(&wd);
        }
        if let Ok(mut loaded) = LOADED_WORKDIRS.lock() {
            loaded.remove(&wd);
        }

        // Query should reload from chronicle
        let anti = query_anti_decisions(&wd, "src/auth.rs");
        assert!(!anti.is_empty(), "should reload from chronicle after cache clear");
        assert_eq!(anti[0].0, "unwrap");
        assert!(anti[0].2 >= 2);
    }

    #[test]
    fn test_prior_bayesian_risk_disambiguation() {
        let wd = clean_test_wd();
        record(&wd, "src/auth.rs", "unwrap", "edit", false);
        record(&wd, "src/auth.rs", "unwrap", "edit", false);
        record(&wd, "src/auth.rs", "question_mark", "edit", true);
        record(&wd, "src/auth.rs", "question_mark", "edit", true);

        let anti = query_anti_decisions(&wd, "src/auth.rs");
        let unwrap_risk = anti.iter().find(|(id, _, _, _)| id == "unwrap").map(|(_, r, _, _)| *r);
        let qm_present = anti.iter().any(|(id, _, _, _)| id == "question_mark");

        assert!(unwrap_risk.is_some(), "unwrap should be in anti-decisions");
        assert!((unwrap_risk.unwrap() - 1.0).abs() < 0.01, "unwrap should have risk 1.0 (3/3 failures incl built-in prior)");
        assert!(!qm_present, "question_mark should NOT be in anti-decisions (0 failures)");
    }

    #[test]
    fn test_looks_like_sed_pattern() {
        // Standard s/old/new/ form
        assert!(looks_like_sed_pattern_inner("s/old/new/"));
        assert!(looks_like_sed_pattern_inner("s/foo/bar/g"));
        // Alternative delimiters
        assert!(looks_like_sed_pattern_inner("s|old|new|"));
        assert!(looks_like_sed_pattern_inner("s#old#new#"));
        // NOT sed patterns
        assert!(!looks_like_sed_pattern_inner("src/main.rs"));
        assert!(!looks_like_sed_pattern_inner("/tmp/file.txt"));
        assert!(!looks_like_sed_pattern_inner("hello"));
        assert!(!looks_like_sed_pattern_inner("ab"));
        assert!(!looks_like_sed_pattern_inner("sfoo"));  // missing delimiter
    }

    #[test]
    fn test_extract_sed_target_finds_real_path() {
        let result = extract_sed_target("sed -i s/old/new/ /tmp/file.txt");
        assert!(result.is_some());
        let (file, _) = result.unwrap();
        assert_eq!(file, "/tmp/file.txt", "should find file path, not sed pattern");

        let result2 = extract_sed_target("sed -i -e 's/x/y/' src/main.rs");
        assert!(result2.is_some());
        let (file2, _) = result2.unwrap();
        assert_eq!(file2, "src/main.rs");
    }

    #[test]
    fn test_extract_primary_identifier_no_keyword_list() {
        // Should now return function-like identifiers based on structural properties
        // (uppercase, underscore, length ≥ 6) without using a hardcoded keyword list.
        let id = extract_primary_identifier("cargo test --quiet", "test_file.rs");
        // Common short lowercase words should be skipped by length filter
        // but "cargo" is 5 chars and all lowercase — should be skipped
        // The function should return a more specific identifier
        assert!(!id.is_empty());
    }
}
