//! Compatibility stubs for functions previously provided by deleted research modules.
//! These provide simplified versions that preserve type_flow scoring behavior
//! without the full research implementations.

/// Stub: receiver type inference — returns None (neutral).
pub fn infer_receiver_type(
    _db: &rusqlite::Connection,
    _file_path: &str,
    _line: i32,
    _stem: &str,
) -> Option<String> {
    None
}

/// Stub: type Jaccard — returns 0.0 (neutral).
pub fn type_jaccard(_a: &str, _b: &str) -> f32 {
    0.0
}

/// Module path extraction — returns the directory portion of the path.
pub fn module_path(path: &str) -> String {
    if let Some(idx) = path.rfind('/') {
        path[..idx].to_string()
    } else {
        path.to_string()
    }
}

/// Stub: module Jaccard — simplified word-overlap on path segments.
pub fn module_jaccard(a: &str, b: &str) -> f32 {
    let set_a: std::collections::HashSet<&str> = a.split('/').collect();
    let set_b: std::collections::HashSet<&str> = b.split('/').collect();
    let inter = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    if union == 0 { 0.0 } else { inter as f32 / union as f32 }
}

/// H2: Enclosing function name — uses brace-graph to find the nearest
/// ancestor function_def/method_def for the given line. Returns the
/// function name (e.g., "block_on") or None if no function encloses it.
pub fn enclosing_fn_name(
    file_lines: &[String],
    line: i32,
) -> Option<String> {
    use crate::brace_graph::{build_brace_graph, find_enclosing_with_role};
    let graph = build_brace_graph(file_lines);
    let line1based = if line < 0 { 0 } else { line };
    for role in &["function_def", "method_def"] {
        if let Some(node) = find_enclosing_with_role(&graph, line1based, role) {
            if !node.first_line_text.is_empty() {
                return Some(node.first_line_text.clone());
            }
            // Fallback: extract identifier from node text via scanner.
            let first = node.first_line_text.clone();
            for tok in crate::scan_identifiers(&first) {
                return Some(tok);
            }
        }
    }
    None
}

/// H2: Enclosing impl target — returns the type name that the nearest
/// enclosing impl block is implementing for. E.g., for `impl BufWriter { ... }`,
/// returns "BufWriter". Returns empty string if no impl block encloses the line.
pub fn enclosing_impl_target(
    file_lines: &[String],
    line: i32,
) -> String {
    use crate::brace_graph::{build_brace_graph, find_enclosing_with_role};
    let graph = build_brace_graph(file_lines);
    let line1based = if line < 0 { 0 } else { line };
    // Note: there's no separate "impl_target" role — impl blocks all share
    // the "function_def" role. We check for "impl " keyword in the node text.
    if let Some(node) = find_enclosing_with_role(&graph, line1based, "function_def") {
        let text = &node.first_line_text;
        if text.trim_start().starts_with("impl ") || text.trim_start().starts_with("impl<") {
            if let Some(idx) = text.find(" for ") {
                let after = &text[idx + 5..];
                for tok in crate::scan_identifiers(after) {
                    return tok;
                }
            } else if let Some(idx) = text.find('{') {
                let before = &text[..idx];
                let mut idents = crate::scan_identifiers(before);
                idents.retain(|s| s != "impl" && s != "where" && s != "trait");
                if let Some(last) = idents.last() {
                    return last.clone();
                }
            }
        }
    }
    String::new()
}

/// Stub: Wasserstein transport — returns 0.0 (neutral).
pub fn function_profile_wasserstein(
    _db: &rusqlite::Connection,
    _anchor_file: &str,
    _anchor_line: i32,
    _cand_file: &str,
    _cand_line: i32,
) -> f32 {
    0.0
}

/// Stub: NCD similarity — returns 0.5 (neutral).
pub fn ncd_similarity(_a: &str, _b: &str) -> f32 {
    0.5
}

/// Stub: window extraction — returns empty string.
pub fn extract_window(_lines: &[String], _line: i32, _radius: i32) -> String {
    String::new()
}

/// Stub: callgraph Jaccard on FxHashSet — returns 0.0 (neutral).
pub fn jaccard(_a: &rustc_hash::FxHashSet<i64>, _b: &rustc_hash::FxHashSet<i64>) -> f32 {
    0.0
}

/// Stub: extract scope bindings for a file — returns 0 (no bindings).
pub fn extract_bindings_for_file(
    _db: &rusqlite::Connection,
    _file_id: i32,
    _lines: &[String],
    _line_block: &[i32],
) -> rusqlite::Result<usize> {
    Ok(0)
}

/// Stub: extract methods for a file — returns 0 (no methods).
pub fn extract_methods_for_file(
    _db: &rusqlite::Connection,
    _file_id: i32,
    _lines: &[String],
) -> rusqlite::Result<usize> {
    Ok(0)
}
