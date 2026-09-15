//! Brace-Graph — the structural tree built from brace counting + role tags.
//!
//! Captures block-level structure WITHOUT parsing. Each node in the brace-graph
//! is a region delimited by matching braces, tagged with a role (function_def,
//! method_call, etc.) and containing child nodes (sub-braced regions).
//!
//! This is what tree-sitter gives but built grammar-free — brace counting replaces
//! parsing, role prediction replaces grammar-based node labeling.

use crate::type_flow::predict_role;
use ahash::AHashMap;
use std::sync::Arc;

/// A node in the brace-graph tree.
#[derive(Clone, Debug)]
pub struct BraceNode {
    pub start_line: i32,
    pub end_line: i32,
    pub role: String,
    pub first_line_text: String,
    pub children: Vec<BraceNode>,
}

impl BraceNode {
    fn new(start_line: i32, role: String, text: String) -> Self {
        BraceNode {
            start_line,
            end_line: start_line,
            role,
            first_line_text: text,
            children: Vec::new(),
        }
    }

    /// Find the deepest ancestor whose line range contains the given line.
    pub fn find_enclosing(&self, line: i32) -> Option<&BraceNode> {
        if line < self.start_line {
            return None;
        }
        // If this node has a child whose range covers the line, recurse into it.
        for child in &self.children {
            if line >= child.start_line && line <= child.end_line {
                return child.find_enclosing(line);
            }
        }
        // If we're the root (no specific end set beyond start), we don't enclose
        // anything specific — return None to indicate "no specific brace scope".
        // Root detection: role == "file".
        if self.role == "file" {
            return None;
        }
        // Leaf-ish node — only return self if line is within its range.
        if line > self.end_line {
            return None;
        }
        Some(self)
    }

    /// Find all nodes in this subtree whose role matches.
    pub fn find_by_role(&self, role: &str) -> Vec<&BraceNode> {
        let mut results = Vec::new();
        self.collect_by_role(role, &mut results);
        results
    }

    fn collect_by_role<'a>(&'a self, role: &str, results: &mut Vec<&'a BraceNode>) {
        if self.role == role {
            results.push(self);
        }
        for child in &self.children {
            child.collect_by_role(role, results);
        }
    }

    /// Total node count in this subtree.
    pub fn node_count(&self) -> usize {
        let mut count = 1;
        for child in &self.children {
            count += child.node_count();
        }
        count
    }

    /// Collect all method_call lines within this subtree (direct children only).
    pub fn method_calls_in(&self) -> Vec<(i32, String)> {
        let mut results = Vec::new();
        self.collect_method_calls(&mut results);
        results
    }

    fn collect_method_calls(&self, results: &mut Vec<(i32, String)>) {
        // M12: doc comment says "direct children only" — don't recurse.
        if self.role == "method_call" {
            results.push((self.start_line, self.first_line_text.clone()));
        }
        for child in &self.children {
            if child.role == "method_call" {
                results.push((child.start_line, child.first_line_text.clone()));
            }
        }
    }
}

/// Build the brace-graph for a file from its lines.
pub fn build_brace_graph(file_lines: &[String]) -> BraceNode {
    let root = BraceNode::new(1, "file".to_string(), file_lines.first().cloned().unwrap_or_default());

    // Use a single mutable root. Stack tracks indices into a flat node list.
    let mut all_nodes: Vec<BraceNode> = vec![root.clone()];
    let mut stack: Vec<usize> = vec![0]; // indices into all_nodes
    let mut depth: i32 = 0;

    // V59 CRITICAL: ignore braces inside comments and string literals.
    // A backtick/quote-wrapped `{` in a doc comment (e.g. "line ends with `{`")
    // used to open a phantom block that never closed — the root swallowed the
    // whole file, children=0, fn_names empty, and every downstream consumer
    // (callgraph, find_definition, methods_on) silently degraded to empty.
    let mut in_block_comment = false;
    for (line_idx, line) in file_lines.iter().enumerate() {
        let line_no = (line_idx + 1) as i32;
        // Strip line comments (// and # outside strings) first.
        let code = crate::structural::strip_line_comment(line);
        let bytes = code.as_bytes();
        let mut in_string = false;
        let mut quote = b'"';
        let mut trimmed_seen = false;
        let mut trimmed = line.trim_start();

        // Walk the CODE portion only; skip string-literal contents and
        // /* */ block-comment regions.
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if in_block_comment {
                if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    in_block_comment = false;
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            if !in_string && !trimmed_seen && !b.is_ascii_whitespace() {
                trimmed = code[i..].trim_start();
                trimmed_seen = true;
            }
            if !in_string && b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                in_block_comment = true;
                i += 2;
                continue;
            }
            // V59: lifetime disambiguation — `'a` in `&'a str` is NOT a
            // char literal. A `'` opens a char literal only when it closes
            // within 3 bytes (`'x'`, `'\\''`). Otherwise skip one byte.
            if !in_string && b == b'\'' {
                if i + 2 < bytes.len() && bytes[i + 2] == b'\''
                    && (bytes[i + 1] != b'\\' || i + 3 < bytes.len()) {
                    // char literal — but only skip when the middle is not
                    // itself a quote char; keep simple: treat as literal.
                    in_string = true;
                    quote = b;
                    i += 1;
                    continue;
                }
                i += 1;
                continue;
            }
            if !in_string && b == b'"' {
                // V61: raw string prefix `r#"..."#` / `r##"..."##` — the inner
                // `"` must NOT close the string early (embedded quotes would
                // corrupt the brace count). Count the hashes, skip to the
                // matching `"#` terminator.
                if i >= 1 && bytes[i - 1] == b'r' {
                    let mut hashes = 0usize;
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j] == b'#' {
                        hashes += 1;
                        j += 1;
                    }
                    // Find `"` + hashes×`#` terminator.
                    let mut k = j;
                    while k < bytes.len() {
                        if bytes[k] == b'"' {
                            let mut m = k + 1;
                            let mut matched = 0usize;
                            while m < bytes.len() && bytes[m] == b'#' && matched < hashes {
                                matched += 1;
                                m += 1;
                            }
                            if matched == hashes {
                                i = m;
                                break;
                            }
                        }
                        k += 1;
                    }
                    if k >= bytes.len() {
                        // Unterminated raw string — treat rest as string.
                        i = bytes.len();
                    }
                    continue;
                }
                in_string = true;
                quote = b;
                i += 1;
                continue;
            }
            if in_string {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == quote { in_string = false; }
                i += 1;
                continue;
            }
            if b == b'{' {
                depth += 1;
                let role = predict_role(trimmed).to_string();
                let node = BraceNode::new(line_no, role, trimmed.to_string());
                all_nodes.push(node);
                stack.push(all_nodes.len() - 1);
            } else if b == b'}' {
                if depth > 0 && stack.len() > 1 {
                    let idx = stack.pop().unwrap();
                    all_nodes[idx].end_line = line_no;
                    // Attach to parent (now at top of stack).
                    if let Some(&parent_idx) = stack.last() {
                        // Performance: use mem::replace (cheap empty node) instead of clone()
                        // which recursively deep-clones all children. For a file with 500
                        // nodes and avg 15 children, clone() allocates ~500-2000KB transient.
                        let node = std::mem::replace(
                            &mut all_nodes[idx],
                            BraceNode::new(0, String::new(), String::new()),
                        );
                        all_nodes[parent_idx].children.push(node);
                    }
                }
                // V65: never let depth go negative — an unmatched `}` (e.g. a
                // brace inside a string that the state machine missed) would
                // desync every subsequent `{`/`}` and silently truncate the
                // graph (nodes created but never attached to the root).
                if depth > 0 { depth -= 1; }
            }
            i += 1;
        }
    }

    all_nodes.into_iter().next().unwrap()
}

/// Find the enclosing scope chain for a given line.
pub fn enclosing_scope_chain(root: &BraceNode, line: i32) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = root.find_enclosing(line);
    while let Some(node) = current {
        chain.push(node.role.clone());
        // Move to parent: walk up by finding which child contains this node.
        current = find_parent(root, node);
    }
    chain
}

/// Find the parent of a node in the brace-graph.
pub fn find_parent<'a>(root: &'a BraceNode, target: &BraceNode) -> Option<&'a BraceNode> {
    if std::ptr::eq(root, target) {
        return None;
    }
    for child in &root.children {
        if std::ptr::eq(child, target) {
            return Some(root);
        }
        if let Some(p) = find_parent(child, target) {
            return Some(p);
        }
    }
    None
}

/// Find the nearest ancestor with a given role for a line.
pub fn find_enclosing_with_role<'a>(root: &'a BraceNode, line: i32, role: &str) -> Option<&'a BraceNode> {
    let chain = collect_enclosing_chain(root, line);
    chain.into_iter().find(|&node| node.role == role).map(|v| v as _)
}

fn collect_enclosing_chain<'a>(root: &'a BraceNode, line: i32) -> Vec<&'a BraceNode> {
    let mut chain = Vec::new();
    if let Some(n) = root.find_enclosing(line) {
        chain.push(n);
        let mut current: &'a BraceNode = n;
        while let Some(parent) = find_parent(root, current) {
            chain.push(parent);
            current = parent;
        }
    }
    chain
}

/// Cache of brace-graphs per file (lazy-loaded). Uses Arc for zero-cost sharing.
static BRACE_CACHE: std::sync::OnceLock<parking_lot::Mutex<AHashMap<String, Arc<BraceNode>>>> =
    std::sync::OnceLock::new();

fn cache() -> &'static parking_lot::Mutex<AHashMap<String, Arc<BraceNode>>> {
    BRACE_CACHE.get_or_init(|| parking_lot::Mutex::new(AHashMap::default()))
}

/// Get or build the brace-graph for a file. Returns an Arc — callers should
/// clone() it (refcount bump) rather than re-reading from disk.
pub fn get_brace_graph(file_path: &str) -> Option<Arc<BraceNode>> {
    {
        let cache = cache().lock();
        if let Some(graph) = cache.get(file_path) {
            return Some(Arc::clone(graph));
        }
    }

    let content = std::fs::read_to_string(file_path).ok()?;
    let lines: Vec<String> = content.lines().map(String::from).collect();
    let graph = Arc::new(build_brace_graph(&lines));

    let mut cache = cache().lock();
    // Prevent unbounded memory growth: when cache exceeds 512 entries,
    // clear half to bound total memory (avg BraceNode ~2-5KB).
    if cache.len() > 512 {
        let keys: Vec<String> = cache.keys().cloned().collect();
        for k in &keys[..keys.len() / 2] {
            cache.remove(k);
        }
    }
    cache.insert(file_path.to_string(), Arc::clone(&graph));
    Some(graph)
}

/// Two lines are in the same scope if they share a deepest enclosing ancestor
/// (excluding the root file node).
pub fn same_scope(root: &BraceNode, line_a: i32, line_b: i32) -> bool {
    let a = root.find_enclosing(line_a);
    let b = root.find_enclosing(line_b);
    match (a, b) {
        (Some(na), Some(nb)) => na.start_line == nb.start_line && na.end_line == nb.end_line,
        _ => false,
    }
}

/// Find the shared ancestor of two lines (their closest common enclosing scope).
pub fn shared_enclosing(root: &BraceNode, line_a: i32, line_b: i32) -> Option<&BraceNode> {
    let chain_a = collect_enclosing_chain(root, line_a);
    let chain_b = collect_enclosing_chain(root, line_b);
    // Walk both chains from deepest to shallowest; first common node is shared.
    for na in &chain_a {
        for nb in &chain_b {
            if na.start_line == nb.start_line && na.end_line == nb.end_line {
                return Some(*na);
            }
        }
    }
    None
}
