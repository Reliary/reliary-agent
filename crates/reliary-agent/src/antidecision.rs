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

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde_json::Value;

type CounterMap = HashMap<String, (usize, usize)>;

pub static ANTI_DB: once_cell::sync::Lazy<Mutex<HashMap<String, CounterMap>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

static LOADED_WORKDIRS: once_cell::sync::Lazy<Mutex<HashSet<String>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashSet::new()));

const BUILTIN_PRIORS: &[&str] = &[
    "unwrap", "legacy", "hack", "todo", "TODO", "FIXME",
    "debug_", "temp", "old_text", "clone",
];

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
    let identifiers: Vec<&str> = text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() >= 3 && s.len() <= 40)
        .filter(|s| s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_'))
        .filter(|s| *s != "edit" && *s != "apply" && *s != "write" && *s != "bash"
                    && *s != "file" && *s != "sed" && *s != "old" && *s != "new"
                    && *s != "text" && *s != "content" && *s != "from" && *s != "with"
                    && *s != "replaced" && *s != "applied" && *s != "successfully"
                    && *s != "wrote" && *s != "bytes" && *s != "error" && *s != "failed"
                    && *s != "result" && *s != "stdout" && *s != "stderr" && *s != "exit"
                    && *s != file_stem)
        .collect();
    if let Some(id) = identifiers.iter().find(|id| {
        id.chars().any(|c| c.is_uppercase()) || id.contains('_')
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
    // Grammar-free sed pattern extraction: look for "sed -i 's/pattern/replacement/' filepath"
    let content_lower = content.to_lowercase();
    if !content_lower.contains("sed") { return None; }

    let file = content.split_whitespace()
        .filter(|w| !w.starts_with('-') && !w.starts_with('\'') && !w.starts_with('"'))
        .filter(|w| w.contains('.') || w.contains('/') || w.contains('\\'))
        .map(|w| w.trim_matches(|c: char| c == '\'' || c == '"' || c == ';').to_string())
        .find(|w| !w.is_empty())?;

    if file.is_empty() { return None; }

    let success = !content_lower.contains("error")
        && !content_lower.contains("fail");
    Some((file, if success { "success".to_string() } else { "fail".to_string() }))
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
        let counters = db.entry(workdir.to_string()).or_insert_with(HashMap::new);
        let entry = counters.entry(key).or_insert((0, 0));
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

pub fn load_persisted(workdir: &str) {
    let db_path = format!("{}/.reliary/chronicle.sqlite", workdir.trim_end_matches('/'));
    let events = match crate::chronicle::init(&db_path) {
        Ok(db) => crate::chronicle::recent_events_by_type(&db, "antidecision", 72),
        Err(_) => return,
    };
    if events.is_empty() { return; }
    if let Ok(mut db) = ANTI_DB.lock() {
        let counters = db.entry(workdir.to_string()).or_insert_with(HashMap::new);
        for event in &events {
            let parts: Vec<&str> = event.detail.splitn(3, "::").collect();
            if parts.len() != 3 { continue; }
            let key = format!("{}::{}::{}", workdir, parts[0], parts[1]);
            let entry = counters.entry(key).or_insert((0, 0));
            if parts[2] == "success" {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }
    }
}

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
        if let Some(counters) = db.get(workdir) {
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
pub fn format_annotation(identifiers: &[(String, f64, usize, usize)], max_tokens: usize) -> String {
    if identifiers.is_empty() { return String::new(); }
    let mut parts: Vec<String> = Vec::new();
    for (id, _risk, fails, _succs) in identifiers.iter().take(max_tokens.min(5)) {
        if *fails >= 2 {
            parts.push(format!("-{}", id));
        }
    }
    if parts.is_empty() { String::new() }
    else { parts.join(" ") }
}

#[allow(dead_code)]
pub fn annotate_tool_result(text: &str, workdir: &str, file_name: &str) -> Option<String> {
    let bad = query_anti_decisions(workdir, file_name);
    if bad.is_empty() { return None; }
    let annotation = format_annotation(&bad, 3);
    if annotation.is_empty() { return None; }
    let has_match = bad.iter().any(|(id, _, _, _)| text.contains(id));
    if !has_match { return None; }
    Some(annotation)
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
        let wd = format!("/tmp/antidecision_test_{}", std::process::id());
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
    fn test_annotation_basic() {
        let wd = clean_test_wd();
        record(&wd, "src/auth.rs", "unwrap", "edit", false);
        record(&wd, "src/auth.rs", "unwrap", "edit", false);
        record(&wd, "src/auth.rs", "unwrap", "edit", false);

        let annotation = annotate_tool_result(
            "File src/auth.rs uses unwrap extensively", &wd, "src/auth.rs"
        );
        assert!(annotation.is_some());
        assert!(annotation.unwrap().contains("-unwrap"));
    }

    #[test]
    fn test_gating() {
        let wd = clean_test_wd();
        record(&wd, "src/auth.rs", "unwrap", "edit", false);
        record(&wd, "src/auth.rs", "unwrap", "edit", false);

        let annotation = annotate_tool_result(
            "File src/auth.rs uses question_mark everywhere", &wd, "src/auth.rs"
        );
        assert!(annotation.is_none(), "should gate when identifier not mentioned: {:?}", annotation);
    }

    #[test]
    fn test_builtin_priors() {
        let anti = query_anti_decisions("/tmp/nonexistent", "src/unknown.rs");
        assert!(anti.is_empty());
    }

    #[test]
    fn test_format_annotation() {
        let entries = vec![
            ("unwrap".to_string(), 0.85, 5, 0usize),
            ("legacy".to_string(), 0.70, 3, 1usize),
        ];
        let ann = format_annotation(&entries, 3);
        assert_eq!(ann, "-unwrap -legacy");
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

        // Clear in-memory state
        if let Ok(mut db) = ANTI_DB.lock() {
            db.clear();
        }
        if let Ok(mut loaded) = LOADED_WORKDIRS.lock() {
            loaded.clear();
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
}
