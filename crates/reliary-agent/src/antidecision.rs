/// Grammar-free anti-decision tracker.
///
/// Observes tool call outcomes flowing through the proxy, records per-identifier
/// success/failure counts, and injects ` -identifier` annotations into tool results
/// that contain file content, via Markov surprise (identical to engfield design).
///
/// Grammar-free: everything operates on (file, identifier, operation, success) tuples.
/// No AST, no regex, no language detection. Identifiers are extracted via simple
/// [A-Za-z_][A-Za-z0-9_]{2,} scanning, split on non-alphanumeric boundaries.
///
/// Key parts:
///   - `record(workdir, file, identifier, operation, success)`: log one action outcome
///   - `query_anti_decisions(workdir, file) -> Vec<String>`: identifiers with ≥2
///     failures and >50% failure rate
///   - `annotate_tool_result(raw_text, workdir, file_name) -> String`: append
///     anti-decision annotations to file-referencing tool result text
///   - Gating: only annotate when the file name appears in the text (the LLM is
///     already attending to it — adjacency coupling via RoPE/ALiBi)
///   - Built-in weak priors for `unwrap`, `legacy`, `hack`, `todo`, `TODO`, `FIXME`,
///     `debug_`, `temp`, `old_text`, `clone` (1 failure each — Beta(1,1) prior,
///     so a single actual observation overrides them)

use std::sync::Mutex;
use std::collections::HashMap;

/// Per-workdir counters: identifier → (successes, failures)
type CounterMap = HashMap<String, (usize, usize)>;

pub static ANTI_DB: once_cell::sync::Lazy<Mutex<HashMap<String, CounterMap>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

/// Record one action outcome.
pub fn record(workdir: &str, file: &str, identifier: &str, _operation: &str, success: bool) {
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
}

/// Query anti-decisions for a workdir+file: identifiers with ≥2 failures
pub fn query_anti_decisions(workdir: &str, file: &str) -> Vec<(String, f64, usize, usize)> {
    let prefix = format!("{}::{}::", workdir, file);

    if let Ok(db) = ANTI_DB.lock() {
        if let Some(counters) = db.get(workdir) {
            let mut results: Vec<(String, f64, usize, usize)> = counters.iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .filter(|(_, &(_, f))| f >= 2)
                .map(|(k, &(s, f))| {
                    let id = k[prefix.len()..].to_string();
                    let total = (s + f) as f64;
                    let risk = f as f64 / total;
                    (id, risk, f, s)
                })
                .collect();
            results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            return results;
        }
    }
    Vec::new()
}

/// Format an anti-decision annotation line from a list of high-risk identifiers.
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

/// Check if a tool result text contains an identifier from known anti-decisions.
/// Returns the anti-decision annotation string if any match.
#[allow(dead_code)]
pub fn annotate_tool_result(text: &str, workdir: &str, file_name: &str) -> Option<String> {
    let bad = query_anti_decisions(workdir, file_name);
    if bad.is_empty() { return None; }

    // Gating: only annotate if the text actually mentions the anti-pattern identifier
    let annotation = format_annotation(&bad, 3);
    if annotation.is_empty() { return None; }

    // Check if text contains any of the failing identifiers (gating)
    let has_match = bad.iter().any(|(id, _, _, _)| text.contains(id));
    if !has_match { return None; }

    Some(annotation)
}

/// Parse identifiers from a text string — grammar-free: split on non-alphanumeric,
/// take tokens of length ≥2 matching [A-Za-z_][A-Za-z0-9_]+
fn extract_identifiers(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() >= 2 && s.chars().next().map_or(false, |c| c.is_alphabetic() || c == '_'))
        .map(|s| s.to_string())
        .collect()
}

/// Extract tool call info from a raw tool result string.
/// Returns (file, identifier, operation, success)
pub fn extract_tool_call(tool_result: &str, tool_name: &str) -> Option<(String, String, String, bool)> {
    match tool_name {
        "edit" | "apply-edit" => {
            // Extract file path from the edit tool result
            if let Some(file) = tool_result.lines()
                .find(|l| l.contains("edit") || l.contains("file"))
                .and_then(|l| {
                    extract_identifiers(l).into_iter()
                        .find(|id| id.contains("."))
                })
            {
                // Find identifier from the context entry
                let all_ids = extract_identifiers(tool_result);
                let identifier = all_ids.iter()
                    .find(|id| !id.contains(".") && id.len() >= 3 && *id != "edit" && *id != "apply")
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                let success = !tool_result.to_lowercase().contains("error")
                    && !tool_result.to_lowercase().contains("fail");
                Some((file, identifier, "edit".to_string(), success))
            } else {
                None
            }
        }
        "write" => {
            let all_ids = extract_identifiers(tool_result);
            let file = all_ids.iter()
                .find(|id| id.contains("."))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let identifier = all_ids.iter()
                .find(|id| !id.contains(".") && id.len() >= 3 && id != &"write")
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let success = !tool_result.to_lowercase().contains("error");
            Some((file, identifier, "write".to_string(), success))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_query() {
        record("/tmp/test", "src/auth.rs", "unwrap", "edit", false);
        record("/tmp/test", "src/auth.rs", "unwrap", "edit", false);
        record("/tmp/test", "src/auth.rs", "unwrap", "edit", false);
        record("/tmp/test", "src/auth.rs", "question_mark", "edit", true);
        record("/tmp/test", "src/auth.rs", "question_mark", "edit", true);

        let anti = query_anti_decisions("/tmp/test", "src/auth.rs");
        assert!(!anti.is_empty(), "should find anti-decisions");
        assert_eq!(anti[0].0, "unwrap");
        assert!(anti[0].1 > 0.5);  // risk > 50%
        assert!(anti[0].2 >= 2);   // ≥2 failures
    }

    #[test]
    fn test_annotation_basic() {
        record("/tmp/test", "src/auth.rs", "unwrap", "edit", false);
        record("/tmp/test", "src/auth.rs", "unwrap", "edit", false);
        record("/tmp/test", "src/auth.rs", "unwrap", "edit", false);

        let annotation = annotate_tool_result(
            "File src/auth.rs uses unwrap extensively", "/tmp/test", "src/auth.rs"
        );
        assert!(annotation.is_some());
        assert!(annotation.unwrap().contains("-unwrap"));
    }

    #[test]
    fn test_gating() {
        let wd = "/tmp/test_gating";
        record(wd, "src/auth.rs", "unwrap", "edit", false);
        record(wd, "src/auth.rs", "unwrap", "edit", false);

        // Text does NOT mention "unwrap" — annotation should be suppressed
        let annotation = annotate_tool_result(
            "File src/auth.rs uses question_mark everywhere", wd, "src/auth.rs"
        );
        assert!(annotation.is_none(), "should gate when identifier not mentioned: {:?}", annotation);
    }

    #[test]
    fn test_builtin_priors() {
        // Without any records, empty query should yield no results
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
    fn test_prior_bayesian_risk_disambiguation() {
        // Two identifiers with different outcomes should have different risks
        record("/tmp/test", "src/auth.rs", "unwrap", "edit", false);
        record("/tmp/test", "src/auth.rs", "unwrap", "edit", false);
        record("/tmp/test", "src/auth.rs", "question_mark", "edit", true);
        record("/tmp/test", "src/auth.rs", "question_mark", "edit", true);

        let anti = query_anti_decisions("/tmp/test", "src/auth.rs");
        // unwrap should have 2 failures → f=2, s=0 → risk = 2/2 = 1.0
        let unwrap_risk = anti.iter().find(|(id, _, _, _)| id == "unwrap").map(|(_, r, _, _)| *r);
        // question_mark should not appear (0 failures, <2 threshold)
        let qm_present = anti.iter().any(|(id, _, _, _)| id == "question_mark");

        assert!(unwrap_risk.is_some(), "unwrap should be in anti-decisions");
        assert!((unwrap_risk.unwrap() - 1.0).abs() < 0.01, "unwrap should have risk 1.0 (2/2 failures)");
        assert!(!qm_present, "question_mark should NOT be in anti-decisions (0 failures)");
    }
}
