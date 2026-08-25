//! Arc 57: Per-file metadata cache. Eliminates per-candidate file I/O
//! for fn_name, brace_graph, arity, and impl_target lookups.
//!
//! On first access: read file once, compute brace-graph once, derive ALL
//! per-line metadata. Store in 100-entry LRU. Subsequent candidates from
//! the same file are O(1) array lookups.

use crate::brace_graph::{build_brace_graph, BraceNode};
use crate::structural::classify_structural;
use ahash::AHashMap;
use std::sync::{Arc, OnceLock};
use parking_lot::Mutex;

#[derive(Debug, Clone)]
pub struct FileMeta {
    pub lines: Vec<String>,
    pub fn_names: Vec<String>,
    pub impl_targets: Vec<String>,
    pub arities: Vec<Option<usize>>,
    pub brace_graph: Arc<BraceNode>,
}

fn cache() -> &'static Mutex<(AHashMap<String, Arc<FileMeta>>, std::collections::VecDeque<String>)> {
    static C: OnceLock<Mutex<(AHashMap<String, Arc<FileMeta>>, std::collections::VecDeque<String>)>> = OnceLock::new();
    C.get_or_init(|| Mutex::new((AHashMap::with_capacity(200), std::collections::VecDeque::new())))
}

pub fn get(path: &str) -> Option<Arc<FileMeta>> {
    {
        let c = cache().lock();
        if let Some(m) = c.0.get(path) {
            return Some(Arc::clone(m));
        }
    }
    compute(path)
}

pub fn compute(path: &str) -> Option<Arc<FileMeta>> {
    let content = std::fs::read_to_string(path).ok()?;
    compute_from_content(path, &content)
}

/// P9-2: Compute from already-read content — avoids a second disk read
/// when the caller has the content in memory (e.g., during `trust`).
pub fn compute_from_content(path: &str, content: &str) -> Option<Arc<FileMeta>> {
    let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let n = lines.len();
    let bg = build_brace_graph(&lines);

    let mut fn_names = vec![String::new(); n];
    let mut impl_targets = vec![String::new(); n];
    let mut arities = vec![None; n];

    // Direct line scan for impl_targets (more reliable than brace-graph walk).
    // Track brace depth and current impl type.
    let mut cur_impl = String::new();
    let mut depth: i32 = 0;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with("impl ") || t.starts_with("impl<") {
            if let Some(ty) = extract_impl_type(t) {
                cur_impl = ty;
            }
        }
        if !cur_impl.is_empty() && depth > 0 {
            impl_targets[i] = cur_impl.clone();
        }
        let opens = t.chars().filter(|&c| c == '{').count() as i32;
        let closes = t.chars().filter(|&c| c == '}').count() as i32;
        depth += opens - closes;
        if depth <= 0 {
            cur_impl.clear();
        }
    }

    // Walk brace-graph for fn_names only.
    walk(&bg, &lines, &mut fn_names, &mut String::new());

    // Also compute per-line arity for all lines (cheap — just string scan).
    for (i, line) in lines.iter().enumerate() {
        if arities[i].is_none() {
            arities[i] = call_arity_from_line(line.trim_start());
        }
    }

    let meta = Arc::new(FileMeta { lines, fn_names, impl_targets, arities, brace_graph: Arc::new(bg) });

    // V54: true LRU eviction — track access order in a VecDeque.
    // HashMap iteration order is arbitrary; the VecDeque gives FIFO ordering
    // (push back on insert, pop front on evict) — approximates LRU for the
    // hot working set.
    let mut c = cache().lock();
    if c.0.len() >= 200 {
        while c.0.len() >= 100 && !c.1.is_empty() {
            if let Some(oldest) = c.1.pop_front() {
                c.0.remove(&oldest);
            }
        }
    }
    c.0.insert(path.to_string(), Arc::clone(&meta));
    c.1.push_back(path.to_string());
    Some(meta)
}

fn walk(
    node: &BraceNode,
    lines: &[String],
    fn_names: &mut [String],
    cur_fn: &mut String,
) {
    let t = node.first_line_text.trim_start();
    let has_block = t.ends_with('{') || t.ends_with(':');
    let result = classify_structural(t, 0, has_block, false);

    if result.is_def && result.tag == 1 {
        if let Some(name) = result.defined_name {
            *cur_fn = name.to_string();
        }
    }

    for line in node.start_line..=node.end_line {
        // V26: brace_graph uses 1-indexed lines, fn_names is 0-indexed.
        let li = (line as usize).saturating_sub(1);
        if li < lines.len() {
            if !cur_fn.is_empty() { fn_names[li] = cur_fn.clone(); }
        }
    }

    for child in &node.children {
        let mut child_fn = cur_fn.clone();
        walk(child, lines, fn_names, &mut child_fn);
    }
}

fn call_arity_from_line(t: &str) -> Option<usize> {
    let start = t.find('(')?;
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut found = false;
    for c in t[start..].chars() {
        if c == '(' { depth += 1; found = true; }
        if c == ')' { depth -= 1; if depth == 0 { break; } }
        if depth == 1 && c == ',' { commas += 1; }
    }
    if !found { return None; }
    let inner = &t[start+1..];
    let end = inner.find(')').unwrap_or(inner.len());
    let trimmed = inner[..end].trim();
    if trimmed.is_empty() { Some(0) } else { Some(commas + 1) }
}

fn extract_impl_type(s: &str) -> Option<String> {
    let after_impl = s.strip_prefix("impl")?.trim_start();
    let after_impl = if after_impl.starts_with('<') {
        let mut depth = 1;
        let mut i = 1;
        for c in after_impl[1..].chars() {
            if c == '<' { depth += 1; }
            if c == '>' { depth -= 1; if depth == 0 { break; } }
            i += c.len_utf8();
        }
        after_impl[i+1..].trim_start()
    } else { after_impl };

    if let Some(for_pos) = after_impl.find(" for ") {
        let after_for = after_impl[for_pos + 5..].trim_start();
        let end = after_for.find(|c: char| c == '<' || c == '{' || c == ' ' || c == ';').unwrap_or(after_for.len());
        let type_name = after_for[..end].trim();
        if !type_name.is_empty() { return Some(type_name.to_string()); }
    }

    let end = after_impl.find(|c: char| c == '<' || c == '{' || c == ' ' || c == ';').unwrap_or(after_impl.len());
    let type_name = after_impl[..end].trim();
    if !type_name.is_empty() { Some(type_name.to_string()) } else { None }
}

pub fn invalidate(path: &str) {
    let mut c = cache().lock();
    c.0.remove(path);
    c.1.retain(|p| p != path);
}

pub fn clear() {
    let mut c = cache().lock();
    c.0.clear();
    c.1.clear();
}