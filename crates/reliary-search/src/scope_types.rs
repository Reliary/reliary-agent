//! Scope-local type map — tracks `let x: Type = ...` bindings per brace-graph scope.
//!
//! For each brace-graph scope, walks the file and builds a map from variable name
//! to inferred type. Used to resolve receiver types like `park.park()` where
//! `park` is a local variable whose type comes from its let binding.
//!
//! Grammar-free: regex-based extraction of type names from let declarations.

use crate::brace_graph::BraceNode;
use ahash::AHashMap;
use rustc_hash::FxHashMap;

#[derive(Clone, Debug, Default)]
pub struct ScopeTypeMap {
    /// Map from variable name → inferred type.
    pub bindings: FxHashMap<String, String>,
    /// Map from variable name → depth of let declaration (inner shadows outer).
    pub shadow_depth: FxHashMap<String, u32>,
}

impl ScopeTypeMap {
    pub fn new() -> Self {
        ScopeTypeMap { bindings: FxHashMap::default(), shadow_depth: FxHashMap::default() }
    }

    pub fn insert(&mut self, var: &str, ty: &str, depth: u32) {
        self.bindings.insert(var.to_string(), ty.to_string());
        self.shadow_depth.insert(var.to_string(), depth);
    }

    pub fn get(&self, var: &str) -> Option<&str> {
        self.bindings.get(var).map(|s| s.as_str())
    }
}

/// Build a type map for a brace-graph scope by walking the source lines
/// that fall within that scope.
pub fn build_scope_type_map(
    file_lines: &[String],
    node: &BraceNode,
    inner_maps: &[ScopeTypeMap],
) -> ScopeTypeMap {
    let mut map = ScopeTypeMap::new();

    // First inherit from outer scopes (shallow binding wins on conflict).
    for inner in inner_maps.iter().rev() {
        for (k, v) in &inner.bindings {
            map.bindings.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }

    // Walk the file lines that fall within this scope and extract let bindings.
    let start = node.start_line.max(1) as usize;
    let end = node.end_line as usize;
    for i in start..=end.min(file_lines.len()) {
        let line = match file_lines.get(i - 1) {
            Some(l) => l,
            None => break,
        };
        extract_let_binding(line, &mut map);
    }

    map
}

/// Extract a local binding (`x = ...` or `x: Type = ...`) from a line.
/// Arc 22: grammar-free — uses structural detection of top-level `=`.
fn extract_let_binding(line: &str, map: &mut ScopeTypeMap) {
    let trimmed = line.trim_start();
    // Structural local-binding check: top-level `=` exists.
    if !crate::type_flow::has_top_level_eq_pub(trimmed) { return; }
    // Find the position of the `=` to skip past `let ` / `mut ` prefix.
    let bytes = trimmed.as_bytes();
    let mut pos = 0;
    // Skip identifier characters (the variable name).
    while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
        pos += 1;
    }
    // Skip `: Type` annotation.
    while pos < bytes.len() && bytes[pos] != b'=' {
        pos += 1;
    }
    let after_eq = if pos < bytes.len() { &trimmed[pos + 1..] } else { trimmed };
    let after_let = after_eq.trim_start();
    let _ = after_let;

    // Skip `mut` if present.
    let after_mut = after_let.strip_prefix("mut ").unwrap_or(after_let).trim_start();

    // Find variable name (identifier before `:` or `=`).
    let bytes = after_mut.as_bytes();
    if bytes.is_empty() || !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') { return; }
    let mut name_end = 0;
    while name_end < bytes.len() && (bytes[name_end].is_ascii_alphanumeric() || bytes[name_end] == b'_') {
        name_end += 1;
    }
    let var_name = &after_mut[..name_end];
    let rest = after_mut[name_end..].trim_start();

    // Pattern 1: `let x: Type = ...`
    if let Some(rest) = rest.strip_prefix(':') {
        let type_str = rest.split('=').next().unwrap_or("").trim();
        let ty = clean_type(type_str);
        if !ty.is_empty() {
            map.insert(var_name, &ty, 1);
        }
        return;
    }

    // Pattern 2: `let x = expr`
    if let Some(rest) = rest.strip_prefix('=') {
        let expr = rest.trim();
        let ty = infer_from_expr(expr);
        if !ty.is_empty() {
            map.insert(var_name, &ty, 1);
        }
    }
}

/// Infer type from RHS expression of let binding.
fn infer_from_expr(expr: &str) -> String {
    let trimmed = expr.trim();

    // Handle `Type::new(...)` or `Type::method(...)`.
    if let Some(pos) = trimmed.find("::") {
        let type_part = &trimmed[..pos];
        // Find the last identifier in type_part (before ::).
        let bytes = type_part.as_bytes();
        let mut start = bytes.len();
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            start -= 1;
        }
        let candidate = &type_part[start..];
        if !candidate.is_empty() && candidate.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            return candidate.to_string();
        }
    }

    // Handle `Type { ... }` (struct literal).
    if let Some(pos) = trimmed.find('{') {
        let type_part = trimmed[..pos].trim();
        let bytes = type_part.as_bytes();
        let mut start = bytes.len();
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            start -= 1;
        }
        let candidate = &type_part[start..];
        if !candidate.is_empty() && candidate.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            return candidate.to_string();
        }
    }

    // Handle capitalized identifier directly: `let x = Type;`.
    if trimmed.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
        let word: String = trimmed.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !word.is_empty() {
            return word;
        }
    }

    // Handle `self.field` — trace receiver type.
    if let Some(rest) = trimmed.strip_prefix("self.") {
        let field = rest.split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or("");
        if !field.is_empty() {
            return capitalize(field);
        }
    }

    String::new()
}

/// Clean a type string (strip lifetimes, references).
fn clean_type(s: &str) -> String {
    let trimmed = s.trim().trim_end_matches(',').trim();
    let stripped = strip_lifetimes(trimmed);
    strip_wrappers(&stripped)
}

fn strip_lifetimes(s: &str) -> String {
    let mut result = String::new();
    let mut depth: i32 = 0;
    for c in s.chars() {
        if c == '<' { depth += 1; if depth == 1 { continue; } }
        if c == '>' { depth -= 1; if depth == 0 { continue; } }
        if depth == 0 { result.push(c); }
    }
    result
}

fn strip_wrappers(s: &str) -> String {
    let mut result = s.to_string();
    let wrappers = ["Pin<&mut ", "Pin<&", "Pin<", "Box<&mut ", "Box<&", "Box<",
                   "Arc<&mut ", "Arc<&", "Arc<", "&mut ", "&"];
    let mut changed = true;
    while changed {
        changed = false;
        for w in wrappers {
            if result.starts_with(w) {
                result = result[w.len()..].to_string();
                if result.ends_with('>') || result.ends_with(')') {
                    result = result[..result.len()-1].trim_end_matches(',').trim().to_string();
                }
                changed = true;
            }
        }
    }
    // Resolve Self → return as-is (caller resolves via impl block).
    result
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => {
            let mut result = c.to_uppercase().next().unwrap_or(c).to_string();
            result.push_str(chars.as_str());
            result
        }
        None => String::new(),
    }
}

/// Walk the brace-graph and build type maps for each function scope.
pub fn build_all_scope_type_maps(file_path: &str) -> Vec<(BraceNode, ScopeTypeMap)> {
    // Arc 60 Phase 3: per-file cache. Eliminates redundant file reads
    // and brace-graph builds for candidates in the same file.
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, Vec<(BraceNode, ScopeTypeMap)>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::with_capacity(100)));
    if let Ok(c) = cache.lock() {
        if let Some(v) = c.get(file_path) {
            return v.clone();
        }
    }

    // Use file_meta cache for lines + brace_graph (avoids re-reading file).
    let (lines, graph): (Vec<String>, BraceNode) = if let Some(m) = crate::file_meta::get(file_path) {
        (m.lines.clone(), (*m.brace_graph).clone())
    } else {
        let lines = std::fs::read_to_string(file_path)
            .map(|c| c.lines().map(String::from).collect::<Vec<_>>())
            .unwrap_or_default();
        let graph = crate::brace_graph::build_brace_graph(&lines);
        (lines, graph)
    };

    let mut results: Vec<(BraceNode, ScopeTypeMap)> = Vec::new();
    build_maps_recursive(&graph, &lines, &[], &mut results);

    if let Ok(mut c) = cache.lock() {
        if c.len() >= 100 { c.clear(); }
        c.insert(file_path.to_string(), results.clone());
    }
    results
}

fn build_maps_recursive(
    node: &BraceNode,
    file_lines: &[String],
    parent_maps: &[ScopeTypeMap],
    results: &mut Vec<(BraceNode, ScopeTypeMap)>,
) {
    // Build the type map for this scope.
    let mut current_maps: Vec<ScopeTypeMap> = parent_maps.to_vec();
    let map = build_scope_type_map(file_lines, node, parent_maps);
    results.push((node.clone(), map.clone()));
    current_maps.push(map);

    // Recurse into children.
    for child in &node.children {
        build_maps_recursive(child, file_lines, &current_maps, results);
    }
}

/// Resolve a variable name to its type by walking up the scope chain.
pub fn resolve_var_type(
    maps: &[(BraceNode, ScopeTypeMap)],
    var_name: &str,
    at_line: i32,
) -> Option<String> {
    // Find all maps whose scope contains at_line, pick the innermost (deepest).
    let mut best: Option<&ScopeTypeMap> = None;
    let mut best_depth: u32 = 0;
    for (node, map) in maps {
        if at_line >= node.start_line && at_line <= node.end_line {
            // Use the smallest span as the innermost.
            let span = (node.end_line - node.start_line) as u32;
            if best.is_none() || span < best_depth {
                best = Some(map);
                best_depth = span;
            }
        }
    }
    best.and_then(|m| m.get(var_name).map(|s| s.to_string()))
}