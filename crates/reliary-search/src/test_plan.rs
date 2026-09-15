//! V70 P3: `reliary test-plan` — which tests exercise the changed code.
//!
//! Grammar-free mapping from changed files/symbols to test files:
//!   1. callers of changed symbols that live in test paths
//!   2. mirror paths (src/foo.rs -> tests/foo.rs, src/foo/ -> tests/foo/)
//!   3. vocabulary overlap (rare identifiers shared between change and test)
//! Deterministic: sorted output, same index => same plan.

use crate::callgraph_v2::build_call_graph_ext;
use crate::impact::is_test_path;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct TestPlan {
    /// Ordered test files with the reasons they matched.
    pub tests: Vec<(String, Vec<String>)>,
    /// Suggested commands (deduped, sorted).
    pub commands: Vec<String>,
}

/// Map a changed file to candidate test files using mirror-path conventions.
fn mirror_candidates(changed: &str) -> Vec<String> {
    let mut out = Vec::new();
    let path = changed.replace('\\', "/");
    let base = path.rsplit('/').next().unwrap_or(&path);
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    let ext = base.rsplit_once('.').map(|(_, e)| e).unwrap_or("");

    // Normalize for segment search: prefix "/" so "src/..." matches "/src/".
    let anchored = if path.starts_with('/') { path.clone() } else { format!("/{}", path) };

    // src/foo.rs -> tests/foo.rs ; src/foo/bar.rs -> tests/bar.rs
    if let Some(idx) = anchored.find("/src/") {
        let after = &anchored[idx + 5..];
        let tail_base = after.rsplit('/').next().unwrap_or(after);
        out.push(format!("tests/{}", tail_base));
        out.push(format!("test/{}", tail_base));
        if !stem.is_empty() {
            out.push(format!("tests/{}_test.{}", stem, ext));
            out.push(format!("tests/test_{}.{}", stem, ext));
            out.push(format!("tests/{}.test.{}", stem, ext));
        }
    }
    // crates/*/src/foo.rs -> crates/*/tests/foo.rs
    if let Some(idx) = anchored.find("/crates/") {
        let rest = &anchored[idx + 1..];
        if let Some((head, _)) = rest.split_once("/src/") {
            out.push(format!("{}/tests/{}", head, base));
            if !stem.is_empty() {
                out.push(format!("{}/tests/{}.rs", head, stem));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Extract rare identifiers (length >= 5) from a file's changed symbols for
/// vocabulary-overlap matching. Grammar-free token scan.
fn rare_identifiers(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            if cur.len() >= 5 && cur.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
                out.insert(cur.clone());
            }
            cur.clear();
        }
    }
    if cur.len() >= 5 && cur.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
        out.insert(cur);
    }
    out
}

/// Compute the test plan for changed files (and optional changed symbols).
pub fn compute_plan(
    db: &Connection,
    changed_files: &[String],
    changed_symbols: &[String],
    path: &str,
) -> rusqlite::Result<TestPlan> {
    let mut matches: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    // All test files in the index (for mirror/vocabulary matching).
    let mut test_files: Vec<String> = Vec::new();
    {
        let mut stmt = db.prepare_cached(
            "SELECT DISTINCT file_path FROM file_map WHERE is_source = 1 ORDER BY file_path",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let fp: String = r.get(0)?;
            if is_test_path(&fp) {
                test_files.push(fp);
            }
        }
    }

    // 1. Callers of changed symbols that are tests.
    for sym in changed_symbols {
        if let Ok(cg) = build_call_graph_ext(db, sym, path, None, 1, true) {
            for c in &cg.callers {
                if is_test_path(&c.file) {
                    matches
                        .entry(c.file.clone())
                        .or_default()
                        .insert(format!("calls {}", sym));
                }
            }
        }
    }

    // 2. Mirror paths: match by basename, but require the test path's tail
    //    to align with the candidate (V74): basename alone matched same-named
    //    files in ANY crate and the changed file itself.
    for changed in changed_files {
        let base = changed.rsplit('/').next().unwrap_or(changed);
        for cand in mirror_candidates(changed) {
            let cand_base = cand.rsplit('/').next().unwrap_or(&cand);
            let cand_tail: Vec<&str> = cand.split('/').filter(|x| !x.is_empty()).collect();
            for tf in &test_files {
                if tf == changed { continue; }
                let tf_base = tf.rsplit('/').next().unwrap_or(tf);
                let tf_tail: Vec<&str> = tf.split('/').filter(|x| !x.is_empty()).collect();
                // Match when the filenames agree AND at least the last two
                // path segments overlap (or both are single-segment names).
                let name_match = tf_base == cand_base || tf_base == base;
                let tail_match = if cand_tail.len() >= 2 && tf_tail.len() >= 2 {
                    cand_tail[cand_tail.len() - 2..] == tf_tail[tf_tail.len() - 2..]
                } else {
                    true
                };
                if name_match && tail_match {
                    matches
                        .entry(tf.clone())
                        .or_default()
                        .insert(format!("mirrors {}", base));
                }
            }
        }
        // 2b. Files in a tests/ dir whose path shares the changed file's
        //     parent module name (e.g. .../symbol.rs -> .../tests/...symbol...)
        let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
        if stem.len() >= 5 {
            for tf in &test_files {
                let tf_base = tf.rsplit('/').next().unwrap_or(tf);
                if tf_base.contains(stem) && tf_base != base {
                    matches
                        .entry(tf.clone())
                        .or_default()
                        .insert(format!("name overlaps {}", stem));
                }
            }
        }
    }

    // 3. Vocabulary overlap: rare identifiers from changed symbols matched
    //    against test file paths is weak — instead check that the test file
    //    references any changed symbol via the occurrence table.
    for sym in changed_symbols {
        let stemmed = crate::stem_identifier(sym);
        if let Ok(Some(pid)) = crate::symbol::phrase_id_for(db, &stemmed) {
            let mut stmt = db.prepare_cached(
                "SELECT DISTINCT f.file_path FROM occurrence o
                 JOIN file_map f ON f.id = o.file_id
                 WHERE o.phrase_id = ?1",
            )?;
            let mut rows = stmt.query(rusqlite::params![pid])?;
            while let Some(r) = rows.next()? {
                let fp: String = r.get(0)?;
                if is_test_path(&fp) {
                    matches
                        .entry(fp)
                        .or_default()
                        .insert(format!("references {}", sym));
                }
            }
        }
    }

    // Order: by match count desc, then path asc (deterministic).
    let mut tests: Vec<(String, Vec<String>)> = matches
        .into_iter()
        .map(|(f, reasons)| (f, reasons.into_iter().collect()))
        .collect();
    tests.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));

    // Commands: language-agnostic — cargo if Cargo.toml in repo root, pytest if
    // any .py test, go test if any _test.go.
    let mut commands: BTreeSet<String> = BTreeSet::new();
    let has_cargo = std::path::Path::new(&format!("{}/Cargo.toml", path.trim_end_matches('/'))).exists();
    // V74: choose the runner from THE FILE'S extension, not global flags.
    // The old code emitted `pytest x.js` for any non-Rust test when a single
    // .py existed anywhere, and `go test .//abs/path` for absolute paths.
    // Paths are made relative to the nearest project marker for runnability.
    let rel = |f: &str| -> String {
        for marker in ["/crates/", "/src/", "/tests/", "/test/"] {
            if let Some(idx) = f.find(marker) {
                return f[idx + 1..].to_string();
            }
        }
        // Absolute-looking path with no marker: use the last two segments.
        let segs: Vec<&str> = f.split('/').filter(|x| !x.is_empty()).collect();
        if segs.len() >= 2 {
            format!("{}/{}", segs[segs.len() - 2], segs[segs.len() - 1])
        } else {
            segs.last().copied().unwrap_or(f).to_string()
        }
    };
    for (f, _) in &tests {
        if f.ends_with(".rs") {
            // Try to infer crate from the path (crates/<name>/...).
            if let Some(idx) = f.find("/crates/") {
                let rest = &f[idx + 8..];
                let crate_name = rest.split('/').next().unwrap_or("");
                if !crate_name.is_empty() {
                    commands.insert(format!("cargo test -p {}", crate_name));
                }
            } else {
                commands.insert("cargo test".to_string());
            }
        } else if f.ends_with(".py") {
            commands.insert(format!("pytest {}", rel(f)));
        } else if f.ends_with("_test.go") {
            let dir = f.rsplit_once('/').map(|(d, _)| d).unwrap_or(f);
            commands.insert(format!("go test ./{}", rel(dir)));
        } else if f.ends_with(".js") || f.ends_with(".ts") || f.ends_with(".jsx") || f.ends_with(".tsx") {
            commands.insert(format!("npx jest {}", rel(f)));
        }
    }

    Ok(TestPlan {
        tests,
        commands: commands.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_candidates_src_layout() {
        let c = mirror_candidates("src/foo.rs");
        assert!(c.contains(&"tests/foo.rs".to_string()));
        assert!(c.contains(&"tests/foo_test.rs".to_string()));
    }

    #[test]
    fn mirror_candidates_crate_layout() {
        let c = mirror_candidates("crates/reliary-search/src/symbol.rs");
        assert!(c.iter().any(|x| x == "crates/reliary-search/tests/symbol.rs"));
    }

    #[test]
    fn rare_identifiers_filters_short() {
        let ids = rare_identifiers("let a = classify_structural(b, 42);");
        assert!(ids.contains("classify_structural"));
        assert!(!ids.contains("let"));
        assert!(!ids.contains("a"));
    }

    #[test]
    fn mirror_handles_no_extension() {
        let c = mirror_candidates("src/module");
        assert!(c.is_empty() || c.iter().all(|x| x.starts_with("test")));
    }
}
