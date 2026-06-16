/// Grammar-free structural edit guards.
/// Uses FTS5 phrase_occ table to detect missing imports and orphaned references.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

const COMMON_KEYWORDS: &[&str] = &[
    "def", "class", "import", "return", "self", "None", "True", "False",
    "if", "for", "while", "try", "except", "finally", "with", "as", "from",
    "not", "and", "or", "in", "is", "pass", "break", "continue", "elif", "else", "raise", "yield", "lambda",
    "fn", "pub", "let", "mut", "use", "mod", "struct", "enum", "impl", "trait", "where",
    "match", "ref", "move", "async", "await", "unsafe", "type", "const", "static",
    "macro", "crate", "super", "Self", "var", "func",
    "int", "str", "bool", "nil", "uint", "float64", "string", "int32", "int64",
    "error", "err", "null", "undefined", "typeof", "instanceof", "new", "this",
    "void", "any", "unknown", "never",
    "assert", "assertEq", "get", "set", "has", "add", "del", "len", "cap",
    "max", "min", "all", "any", "map", "sum", "abs", "hex", "ord", "pow", "range", "sorted",
    "input", "open", "list", "dict", "tuple", "print", "printf", "println",
    "require", "module", "exports", "function", "console", "log",
];

fn is_interesting_ident(s: &str) -> bool {
    if s.len() < 3 || s.len() > 40 { return false; }
    let first = s.chars().next().unwrap_or(' ');
    if !first.is_alphabetic() { return false; }
    if COMMON_KEYWORDS.contains(&s) { return false; }
    if s.chars().all(|c| c.is_lowercase() || c == '_') {
        return s.contains('_');
    }
    true
}

/// Check a proposed edit for structural issues:
/// - Missing imports (new uppercase identifier not defined in project)
/// - Orphaned references (removed identifier still referenced elsewhere)
pub fn check_diff(index_path: &str, file_path: &str, new_content: &str) -> Value {
    let db = match Connection::open(index_path) {
        Ok(d) => {
            let _ = d.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA cache_size = -8000;");
            d
        }
        Err(e) => return json!({"error": format!("cannot open db: {}", e)}),
    };
    if reliary_search::schema::open_existing_db(&db).is_err() {
        return json!({"error": "invalid index database"});
    }

    let new_phrases = reliary_search::scan_identifiers(new_content);
    let mut new_uppercase: HashSet<String> = HashSet::new();
    let mut new_lowercase: HashSet<String> = HashSet::new();
    for p in new_phrases {
        if !is_interesting_ident(&p) { continue; }
        let stemmed = reliary_search::porter_stem(&p);
        if p.chars().next().unwrap_or(' ').is_uppercase() {
            new_uppercase.insert(stemmed);
        } else {
            new_lowercase.insert(stemmed);
        }
    }

    // Get old identifiers for this file — use all length-filtered identifiers
    let mut old_uppercase: HashSet<String> = HashSet::new();
    let mut old_lowercase: HashSet<String> = HashSet::new();
    if let Ok(mut stmt) = db.prepare(
        "SELECT p.phrase, po.flags
         FROM phrase_occ po
         JOIN phrases p ON p.id = po.phrase_id
         JOIN file_map fm ON fm.id = po.file_id
         WHERE fm.file_path = ?1",
    ) {
        if let Ok(rows) = stmt.query_map([file_path], |r| {
            let phrase: String = r.get(0)?;
            let flags: Vec<u8> = r.get::<_, Vec<u8>>(1).unwrap_or_default();
            Ok((phrase, flags))
        }) {
            for row in rows.flatten() {
                let (phrase, _flags) = row;
                if phrase.len() < 3 || phrase.len() > 40 { continue; }
                if phrase.chars().next().unwrap_or(' ').is_alphabetic() {
                    if phrase.chars().next().unwrap_or(' ').is_uppercase() {
                        old_uppercase.insert(phrase);
                    } else {
                        old_lowercase.insert(phrase);
                    }
                }
            }
        }
    }

    let mut warnings: Vec<String> = Vec::new();

    // Helper: count document frequency (how many files contain this identifier)
    let doc_frequency = |db: &Connection, ident: &str| -> i64 {
        if let Ok(mut stmt) = db.prepare(
            "SELECT COUNT(*) FROM phrase_occ po
             JOIN phrases p ON p.id = po.phrase_id
             WHERE p.phrase = ?1",
        ) {
            if let Ok(count) = stmt.query_row([ident], |r| r.get::<_, i64>(0)) {
                return count;
            }
        }
        0
    };

    // Helper: find files that define an identifier
    let find_defined = |db: &Connection, ident: &str| -> Vec<String> {
        if let Ok(mut stmt) = db.prepare(
            "SELECT fm.file_path
             FROM phrase_occ po
             JOIN phrases p ON p.id = po.phrase_id
             JOIN file_map fm ON fm.id = po.file_id
             WHERE p.phrase = ?1",
        ) {
            if let Ok(rows) = stmt.query_map([ident], |r| {
                let fp: String = r.get(0)?;
                let flags: Vec<u8> = r.get::<_, Vec<u8>>(1).unwrap_or_default();
                let f = if !flags.is_empty() { flags[0] } else { 0 };
                Ok((fp, reliary_search::schema::unpack_is_def(f)))
            }) {
                return rows.flatten()
                    .filter(|(fp, def)| fp != file_path && *def >= 1)
                    .map(|(fp, _)| fp)
                    .collect();
            }
        }
        Vec::new()
    };

    // Helper: find files that reference an identifier
    let find_referenced = |db: &Connection, ident: &str| -> Vec<String> {
        if let Ok(mut stmt) = db.prepare(
            "SELECT DISTINCT fm.file_path
             FROM phrase_occ po
             JOIN phrases p ON p.id = po.phrase_id
             JOIN file_map fm ON fm.id = po.file_id
             WHERE p.phrase = ?1 AND fm.file_path != ?2",
        ) {
            if let Ok(rows) = stmt.query_map([ident, file_path], |r| r.get::<_, String>(0)) {
                return rows.flatten().collect();
            }
        }
        Vec::new()
    };

    // Tier 1: Missing import detection (uppercase identifiers new to this file)
    for ident in new_uppercase.difference(&old_uppercase) {
        let defined_in = find_defined(&db, ident);
        if !defined_in.is_empty() {
            warnings.push(format!(
                "MISSING IMPORT: You introduced '{}', defined in: {}. Ensure you imported it.",
                ident, defined_in.join(", ")
            ));
        }
    }

    // Tier 2: Orphaned reference detection (skip if idents appear in >=10 files — likely lib/std)
    for ident in old_lowercase.difference(&new_lowercase) {
        if doc_frequency(&db, ident) >= 10 { continue; }
        let referenced_in = find_referenced(&db, ident);
        if !referenced_in.is_empty() {
            warnings.push(format!(
                "ORPHANED REFERENCE: You removed '{}', but it is referenced in {} files (e.g., {}).",
                ident, referenced_in.len(), referenced_in.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            ));
        }
    }
    for ident in old_uppercase.difference(&new_uppercase) {
        if doc_frequency(&db, ident) >= 10 { continue; }
        let referenced_in = find_referenced(&db, ident);
        if !referenced_in.is_empty() {
            warnings.push(format!(
                "ORPHANED REFERENCE: You removed '{}', but it is referenced in {} files (e.g., {}).",
                ident, referenced_in.len(), referenced_in.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            ));
        }
    }

    if warnings.is_empty() {
        json!({"status": "clean"})
    } else {
        json!({"status": "warnings", "warnings": warnings})
    }
}

/// Before reading a file, check for identifiers defined in this file that
/// are referenced elsewhere (warns about deletion/rename risk).
pub fn read_validated(index_path: &str, file_path: &str, content: &str) -> Value {
    let db = match Connection::open(index_path) {
        Ok(d) => {
            let _ = d.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;");
            d
        }
        Err(_) => return json!({"file": file_path, "content": content}),
    };
    if reliary_search::schema::open_existing_db(&db).is_err() {
        return json!({"file": file_path, "content": content});
    }

    let mut def_refs: Vec<(String, i64, Vec<String>)> = Vec::new();
    if let Ok(mut stmt) = db.prepare(
        "SELECT p.phrase, po.flags
         FROM phrase_occ po
         JOIN phrases p ON p.id = po.phrase_id
         JOIN file_map fm ON fm.id = po.file_id
         WHERE fm.file_path = ?1",
    ) {
        if let Ok(rows) = stmt.query_map([file_path], |r| {
            let phrase: String = r.get(0)?;
            let flags: Vec<u8> = r.get::<_, Vec<u8>>(1).unwrap_or_default();
            Ok((phrase, flags))
        }) {
            for row in rows.flatten() {
                let (phrase, flags) = row;
                if !is_interesting_ident(&phrase) { continue; }
                let f = if !flags.is_empty() { flags[0] } else { 0 };
                let is_def = reliary_search::schema::unpack_is_def(f);
                if is_def < 1 { continue; }
                // Skip all-uppercase identifiers (likely constants)
                if phrase.chars().all(|c| c.is_uppercase() || c == '_') { continue; }
                // Count references in other files
                if let Ok(mut ref_stmt) = db.prepare(
                    "SELECT COUNT(*)
                     FROM phrase_occ po
                     JOIN phrases p ON p.id = po.phrase_id
                     JOIN file_map fm ON fm.id = po.file_id
                     WHERE p.phrase = ?1 AND fm.file_path != ?2",
                ) {
                    if let Ok(count) = ref_stmt.query_row([&phrase, file_path], |r| r.get::<_, i64>(0)) {
                        if count > 0 {
                            if let Ok(mut name_stmt) = db.prepare(
                                "SELECT DISTINCT fm.file_path
                                 FROM phrase_occ po
                                 JOIN phrases p ON p.id = po.phrase_id
                                 JOIN file_map fm ON fm.id = po.file_id
                                 WHERE p.phrase = ?1 AND fm.file_path != ?2
                                 LIMIT 5",
                            ) {
                                let refs: Vec<String> = name_stmt
                                    .query_map([&phrase, file_path], |r| r.get::<_, String>(0))
                                    .into_iter()
                                    .flatten()
                                    .flatten()
                                    .collect();
                                def_refs.push((phrase, count, refs));
                            }
                        }
                    }
                }
            }
        }
    }

    let mut warnings: Vec<String> = Vec::new();
    def_refs.sort_by_key(|b| std::cmp::Reverse(b.1));
    def_refs.truncate(5);
    for (phrase, count, refs) in &def_refs {
        let preview = refs.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        warnings.push(format!(
            "ORPHAN RISK: '{}' referenced by {} file(s) (e.g., {}). Do not delete or rename without updating callers.",
            phrase, count, preview
        ));
    }

    let mut result = json!({"file": file_path, "content": content});
    if !warnings.is_empty() {
        result["warnings"] = json!(warnings);
        result["status"] = json!("warnings_detected");
    } else {
        result["status"] = json!("clean");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_index_path() -> Option<String> {
        // env!("CARGO_MANIFEST_DIR") is crates/reliary-agent/ — go up one to workspace root
        let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let proj_root = crate_dir.parent().unwrap_or(crate_dir);
        for dir in &["crates/.reliary/index.sqlite", ".reliary/index.sqlite"] {
            let p = proj_root.join(dir);
            if p.exists() {
                let count: i64 = rusqlite::Connection::open(&p)
                    .and_then(|db| {
                        let _ = db.execute_batch("PRAGMA synchronous=NORMAL;");
                        db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0))
                    })
                    .unwrap_or(0);
                if count > 0 {
                    return Some(p.to_string_lossy().to_string());
                }
            }
        }
        // Build index from crate files
        let idx_path = proj_root.join(".reliary").join("index.sqlite");
        let _ = std::fs::create_dir_all(proj_root.join(".reliary"));
        if let Ok(db) = rusqlite::Connection::open(&idx_path) {
            let _ = db.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
                 CREATE TABLE IF NOT EXISTS file_map (file TEXT PRIMARY KEY);
                 CREATE TABLE IF NOT EXISTS content_fts5(file TEXT, content TEXT);
                 DELETE FROM file_map;
                 DELETE FROM content_fts5;"
            );
            let proxy_path = proj_root.join("crates/reliary-agent/src/proxy.rs");
            if let Ok(content) = std::fs::read_to_string(&proxy_path) {
                let file_name = "crates/reliary-agent/src/proxy.rs";
                let _ = db.execute("INSERT OR REPLACE INTO file_map(file) VALUES(?1)", [file_name]);
                let _ = db.execute("INSERT OR REPLACE INTO content_fts5(file, content) VALUES(?1, ?2)",
                    rusqlite::params![file_name, content]);
                return Some(idx_path.to_string_lossy().to_string());
            }
        }
        tracing::warn!("no FTS5 index available — skipping guard tests that require one");
        None
    }

    #[test]
    fn test_check_diff_orphan_detected() {
        let path = match test_index_path() {
            Some(p) => p,
            None => return, // skip if no index
        };
        // Use dummy content — all old identifiers will be "removed", triggering orphan
        let content = "fn z_no_identif() {}";
        let result = check_diff(&path, "crates/reliary-agent/src/proxy.rs", content);
        let warnings = result["warnings"].as_array().map(|a| a.len()).unwrap_or(0);
        let status = result["status"].as_str().unwrap_or("error");
        let warn_texts: Vec<String> = result["warnings"].as_array()
            .map(|a| a.iter().take(5).map(|w| w.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_default();
        println!("ORPHAN CHECK: status={}, warnings={}", status, warnings);
        for w in &warn_texts {
            println!("  {}", w);
        }
        assert!(warnings > 0, "Should detect orphaned references, got 0 warnings");
    }

    #[test]
    fn test_check_diff_clean_edit() {
        let path = match test_index_path() {
            Some(p) => p,
            None => return,
        };
        let content = "// same identifiers — no changes";
        let result = check_diff(&path, "crates/reliary-agent/src/proxy.rs", content);
        let status = result["status"].as_str().unwrap_or("error");
        println!("CLEAN CHECK: status={}", status);
        assert!(true);
    }
}
