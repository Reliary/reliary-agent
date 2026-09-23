//! Grammar-free call graph v2 — Arc 42 Phase B.
//!
//! Extracts callees (what a function calls) and callers (who calls a function)
//! using only:
//!   - brace-graph for function body boundaries (no AST, no parser)
//!   - occurrence table for tokens inside bodies (already indexed)
//!   - identifier-followed-by-`(` pattern matching
//!   - a universal keyword filter (no per-language code)
//!
//! Compared to the original `callgraph` module which used phrase co-occurrence
//! (noisy, includes false positives from shared vocabulary), this module extracts
//! the EXACT callees of a function by scanning its brace-delimited body for
//! function call patterns. Grammar-free, universal, deterministic.

use rusqlite::{params, Connection};
use serde::Serialize;
use smallvec::SmallVec;

use crate::brace_graph::get_brace_graph;
use crate::symbol::phrase_id_for;
use crate::lazy_occurrence::ensure_occurrence_for_phrase;

/// Universal keyword filter — these are NEVER callees.
/// Languages: Rust, Python, JS, Go, Java, C++. The set is a superset of common
/// reserved words across all of them. Adding a word here affects ALL corpora
/// equally, no per-language branching.
const STOPWORDS: &[&str] = &[
    // Rust
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
    "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop",
    "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self",
    "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while", "yield",
    // Python
    "and", "class", "def", "del", "elif", "except", "finally", "from", "global",
    "import", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "try",
    "with",
    // JS / TS
    "abstract", "case", "catch", "default", "delete", "do", "export", "extends",
    "finally", "function", "instanceof", "new", "switch", "this", "throw",
    "typeof", "var", "void",
    // Go
    "chan", "defer", "fallthrough", "func", "go", "interface", "map", "package",
    "range", "select",
    // Common stdlib type names that appear with `(` but aren't function calls
    "Ok", "Err", "Some", "None", "Result", "Option", "Vec", "String", "Box",
    "Arc", "Mutex", "RwLock", "Pin", "Future", "Send", "Sync", "Cell", "RefCell",
    "Rc", "Weak", "Cow", "HashMap", "BTreeMap", "HashSet", "BTreeSet", "VecDeque",
    "LinkedList", "BinaryHeap",
    // Common trait names that may appear in `where T: Trait` or `impl Trait for T`
    "Clone", "Copy", "Debug", "Display", "Default", "Eq", "Ord", "PartialEq",
    "PartialOrd", "Hash", "Iterator", "IntoIterator", "FromIterator", "From",
    "Into", "TryFrom", "TryInto", "Deref", "Drop", "Sized", "Unpin", "Send",
    "Sync", "Fn", "FnMut", "FnOnce", "ToString", "Error",
    // Attribute/annotation noise (appear as word( in cfg(...), #[doc = ...], etc.)
    "cfg", "cfg_attr", "doc", "test", "bench", "allow", "warn", "deny", "forbid",
    "derive", "repr", "track_caller", "cold", "inline", "noinline",
];

#[derive(Serialize, Clone)]
pub struct Callee {
    pub name: String,
    pub def_file: Option<String>,
    pub def_line: Option<i32>,
    pub call_line: i32,
    pub source: String,
}

#[derive(Serialize, Clone)]
pub struct Caller {
    pub name: String,
    pub file: String,
    pub line: i32,
    pub source: String,
}

#[derive(Serialize)]
pub struct CallGraph {
    pub anchor_name: String,
    pub anchor_file: String,
    pub anchor_line: i32,
    pub source_preview: String,
    pub callees: Vec<Callee>,
    pub callers: Vec<Caller>,
}

/// Extract all identifier-followed-by-`(` patterns from a block of source lines.
///
/// Grammar-free: regex pattern `\b[a-zA-Z_][a-zA-Z0-9_]*\s*\(`. No per-language code.
/// Returns (line_no, identifier, line_text) tuples.
fn extract_call_patterns(start_line: i32, lines: &[String]) -> Vec<(i32, String, String)> {
    let mut out = Vec::new();
    // Pattern: identifier followed by optional whitespace then `(`.
    // We use a manual scan to avoid pulling in regex deps.
    let mut chars_idx = 0;
    // P8-11: removed dead `buf` variable.
    while chars_idx < lines.len() {
        let line = &lines[chars_idx];
        // V60: strip line comments before scanning so `// calls spawn(..)`
        // doc lines don't produce phantom callees.
        let stripped = crate::structural::strip_line_comment(line);
        // Scan within a single line for identifier( or identifier  (
        let bytes = stripped.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // Start of identifier: letter or underscore.
            let is_ident_start = bytes[i] == b'_'
                || bytes[i].is_ascii_alphabetic();
            if is_ident_start {
                let start = i;
                while i < bytes.len() && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric()) {
                    i += 1;
                }
                let end = i;
                // Check what follows: optional whitespace then `(`.
                // M7: accept tab as well as space, and skip generic turbofish `::<...>` before `(`
                let mut j = i;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') { j += 1; }
                // Skip Rust generic call `foo::<T>(`
                if j + 1 < bytes.len() && bytes[j] == b':' && bytes[j+1] == b':' {
                    j += 2;
                    // Skip until matching `>`
                    let mut depth = 0i32;
                    while j < bytes.len() {
                        if bytes[j] == b'<' { depth += 1; }
                        else if bytes[j] == b'>' {
                            depth -= 1;
                            if depth <= 0 { j += 1; break; }
                        }
                        j += 1;
                    }
                    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') { j += 1; }
                }
                if j < bytes.len() && bytes[j] == b'(' {
                    // Exclude `if(` `while(` `for(` `match(` `switch(` etc — handled by STOPWORDS.
                    let ident = &stripped[start..end];
                    // V64: skip method calls on a receiver (`x.trim_start()`,
                    // `self.len()`) — these are std/trait methods, not free
                    // functions in the codebase. A real fn call is at the
                    // start of an expression: preceded by start-of-line,
                    // `(`, `,`, `=`, `&`, or whitespace-after-operator.
                    let prev = if start == 0 { b' ' } else { stripped.as_bytes()[start - 1] };
                    let is_method_call = matches!(prev, b'.' | b'?');
                    if is_method_call { i = end; continue; }
                    let line_no = start_line + chars_idx as i32;
                    // P8-7: only clone the line when a call pattern is actually found.
                    // Was: line.clone() for every line in the function body.
                    out.push((line_no, ident.to_string(), stripped.to_string()));
                }
            } else {
                i += 1;
            }
        }
        chars_idx += 1;
    }
    out
}

/// Find the brace-graph node that contains the function definition at (file, line).
/// Returns the function's body range as (start_line, end_line).
///
/// Rejects the root node (which spans the whole file) since we need an actual
/// function/method brace-block, not the entire file as the "body".
pub fn debug_find_function_body(file_path: &str, anchor_line: i32) -> Option<(i32, i32)> {
    find_function_body(file_path, anchor_line)
}

fn find_function_body(file_path: &str, anchor_line: i32) -> Option<(i32, i32)> {
    let root = crate::file_meta::get(file_path)
        .map(|m| m.brace_graph.clone())
        .or_else(|| get_brace_graph(file_path))?;
    // Walk down the tree to find the deepest block whose start_line is at or
    // after the anchor. Multi-line signatures have `{` after the anchor line.
    find_function_body_recurse(&root, anchor_line)
}

fn find_function_body_recurse(
    node: &crate::brace_graph::BraceNode, anchor_line: i32,
) -> Option<(i32, i32)> {
    // Try to find a child whose range starts at or after anchor_line.
    let mut best: Option<&crate::brace_graph::BraceNode> = None;
    for child in &node.children {
        if child.role == "file" { continue; }
        // Child must start at or after anchor_line and contain anchor_line's general area.
        if child.start_line >= anchor_line && child.start_line <= anchor_line + 5 {
            // Prefer the smallest (deepest) child.
            if best.is_none_or(|b| child.end_line - child.start_line < b.end_line - b.start_line) {
                best = Some(child);
            }
        }
    }
    if let Some(b) = best {
        return Some((b.start_line, b.end_line));
    }
    // Fallback: original behavior.
    let n = node.find_enclosing(anchor_line)?;
    if n.role == "file" { None } else { Some((n.start_line, n.end_line)) }
}

/// Find the brace-graph function definition site for a symbol name in a file.
/// Returns (file_path, line, body_start, body_end) or None.
fn find_definition(db: &Connection, name: &str) -> Option<(String, i32)> {
    // Try the full name first, then the last component.
    // M7: SmallVec avoids heap for the common 1-2 candidate case.
    let candidates: SmallVec<[String; 4]> = if name.contains("::") {
        let last = name.rsplit("::").next().unwrap_or(name).to_string();
        if last == name { vec![name.to_string()].into() } else { vec![name.to_string(), last].into() }
    } else if name.contains('.') {
        let last = name.rsplit('.').next().unwrap_or(name).to_string();
        if last == name { vec![name.to_string()].into() } else { vec![name.to_string(), last].into() }
    } else {
        vec![name.to_string()].into()
    };
    // Extract type prefix from qualified names (e.g., "Runtime::block_on" → "runtime")
    let type_hint: Option<String> = if name.contains("::") {
        let parts: Vec<&str> = name.split("::").collect();
        if parts.len() >= 2 { Some(parts[parts.len() - 2].to_lowercase()) } else { None }
    } else { None };

    for cand in &candidates {
        let phrase_id = match phrase_id_for(db, cand).ok().flatten() {
            Some(id) => id,
            None => continue,
        };

        // V13: try phrase_occ + file_meta fallback BEFORE occurrence table.
        // The occurrence table is populated lazily and may miss some files.
        // file_meta::fn_names is always complete because it's built at trust time.
        let cand_lower = cand.to_ascii_lowercase();
        if let Some(result) = find_definition_via_meta(db, phrase_id, &cand_lower, type_hint.as_deref()) {
            _ = ensure_occurrence_for_phrase(db, phrase_id);
            return Some(result);
        }

        if let Err(e) = ensure_occurrence_for_phrase(db, phrase_id) { eprintln!("[callgraph_v2] JIT occurrence build failed for phrase_id={}: {}", phrase_id, e); }
        let mut stmt = if let Some(ref _hint) = type_hint {
            match db.prepare_cached(
                "SELECT f.file_path, o.line, o.tag FROM occurrence o
                 JOIN file_map f ON f.id = o.file_id
                 WHERE o.phrase_id = ?1 AND o.is_def != 0
                   AND f.file_path NOT LIKE '%.md'
                   AND f.file_path NOT LIKE '%.txt'
                   AND f.file_path NOT LIKE '%.toml'
                  ORDER BY (f.file_path NOT LIKE '%/' || ?2 || '.%') ASC,
                           -- V13: prefer type/DIR across same-named files like runtime/runtime.rs over runtime/context/runtime.rs
                           (f.file_path NOT LIKE '%/' || ?2 || '/' || ?2 || '.%') ASC,
                           (o.tag = 1) DESC,
                           (f.file_path LIKE '%/tests/%') ASC,
                           LENGTH(f.file_path) ASC,
                           o.occ_id LIMIT 20",
            ).ok() {
                Some(s) => s,
                None => continue,
            }
        } else {
            match db.prepare_cached(
                "SELECT f.file_path, o.line, o.tag FROM occurrence o
                 JOIN file_map f ON f.id = o.file_id
                 WHERE o.phrase_id = ?1 AND o.is_def != 0
                   AND f.file_path NOT LIKE '%.md'
                   AND f.file_path NOT LIKE '%.txt'
                   AND f.file_path NOT LIKE '%.toml'
                 ORDER BY (o.tag = 1) DESC,
                          (f.file_path LIKE '%/tests/%') ASC,
                          (f.file_path NOT LIKE '%.rs') ASC,
                          LENGTH(f.file_path) ASC,
                          o.occ_id LIMIT 20",
            ).ok() {
                Some(s) => s,
                None => continue,
            }
        };
        let mut rows = if let Some(ref hint) = type_hint {
            match stmt.query(rusqlite::params![phrase_id, hint]).ok() {
                Some(r) => r,
                None => continue,
            }
        } else {
            match stmt.query(rusqlite::params![phrase_id]).ok() {
                Some(r) => r,
                None => continue,
            }
        };
        // V61: don't swallow row errors as "no definition" — a corrupted row
        // silently killed the whole candidate loop and every caller treated
        // the result as a genuine miss.
        loop {
            match rows.next() {
                Ok(Some(r)) => {
                    let fp: String = match r.get(0) {
                        Ok(v) => v,
                        Err(e) => { eprintln!("[callgraph_v2] find_definition row get: {}", e); continue; }
                    };
                    let ln: i32 = match r.get(1) {
                        Ok(v) => v,
                        Err(e) => { eprintln!("[callgraph_v2] find_definition row get: {}", e); continue; }
                    };
                    if find_function_body(&fp, ln + 1).is_some() {
                        return Some((fp, ln + 1));
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("[callgraph_v2] find_definition next: {}", e);
                    return None;
                }
            }
        }
    }
    None
}

/// V13: Find definition using phrase_occ blob + file_meta::fn_names instead
/// of the occurrence table. This bypasses the JIT bug where some files are
/// missing from occurrence for a phrase. Grammar-free — uses existing indexes.
fn find_definition_via_meta(db: &Connection, phrase_id: i64, name: &str, type_hint: Option<&str>) -> Option<(String, i32)> {
    let file_ids = super::lazy_occurrence::file_ids_for_phrase(db, phrase_id).ok()?;
    let name_lower = name.to_ascii_lowercase();
    let h_lower = type_hint.map(|h| h.to_ascii_lowercase());

    // Order: preferred directory matches first, then same-dir-stem files,
    // then rest. This ensures runtime/runtime.rs (best match for
    // Runtime::block_on) ranks above runtime/context/runtime.rs.
    let mut pref: Vec<(i64, String)> = Vec::new();
    let mut stem_match: Vec<(i64, String)> = Vec::new();
    let mut rest: Vec<(i64, String)> = Vec::new();
    for (fid, fp) in &file_ids {
        let fp_lower = fp.to_ascii_lowercase();
        if let Some(ref hint) = h_lower {
            if fp_lower.contains(&format!("/{}/", hint))
                || fp_lower.contains(&format!("/{}.rs", hint))
            {
                // Check if file stem matches dir name (e.g., runtime/runtime.rs)
                if let Some(stem) = std::path::Path::new(fp)
                    .file_stem().and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase())
                {
                    if stem == *hint {
                        stem_match.push((*fid, fp.clone()));
                        continue;
                    }
                }
                pref.push((*fid, fp.clone()));
                continue;
            }
        }
        rest.push((*fid, fp.clone()));
    }
    let mut candidates: Vec<(i64, String)> = stem_match;
    candidates.extend(pref);
    candidates.extend(rest);
    // V59: rank production source above tests/benches/scripts before the
    // truncate — otherwise an arbitrary blob-order test file wins the anchor
    // and the call graph describes the wrong body.
    candidates.sort_by(|a, b| {
        let rank = |fp: &String| -> u8 {
            let f = fp.as_str();
            if f.contains("/tests/") || f.contains("/test/") || f.contains("/benches/")
                || f.contains("/bench/") || f.contains("/examples/") { 3 }
            else if f.contains("/crates/") { 0 }
            else if f.ends_with(".rs") { 1 }
            else { 2 }
        };
        rank(&a.1).cmp(&rank(&b.1))
    });
    candidates.truncate(50);
    for (_, fp) in &candidates {
        if let Some(meta) = crate::file_meta::get(fp) {
            for (line_idx, fn_name) in meta.fn_names.iter().enumerate() {
                if fn_name.eq_ignore_ascii_case(&name_lower) {
                    // V13: skip doc-comment lines — file_meta tags fn names
                    // from brace-graph walk which may include doc examples.
                    let line_text = meta.lines.get(line_idx)
                        .map(|s| s.trim_start()).unwrap_or("");
                    if line_text.starts_with("//") || line_text.starts_with("/*")
                        || line_text.starts_with("*") || line_text.starts_with("#")
                    {
                        continue;
                    }
                    return Some((fp.clone(), (line_idx + 1) as i32));
                }
            }
        }
    }
    None
}

/// Find definition preferring same-directory as context_file when provided.
pub fn find_definition_near(db: &Connection, name: &str, context_file: &str) -> Option<(String, i32)> {
    // Extract parent directory from context file.
    let context_dir = std::path::Path::new(context_file)
        .parent()?.to_string_lossy().replace('%', r"\%").replace('_', r"\_");

    let candidates: SmallVec<[String; 4]> = if name.contains("::") {
        let last = name.rsplit("::").next().unwrap_or(name).to_string();
        if last == name { vec![name.to_string()].into() } else { vec![name.to_string(), last].into() }
    } else if name.contains('.') {
        let last = name.rsplit('.').next().unwrap_or(name).to_string();
        if last == name { vec![name.to_string()].into() } else { vec![name.to_string(), last].into() }
    } else {
        vec![name.to_string()].into()
    };
    for cand in &candidates {
        let phrase_id = match phrase_id_for(db, cand).ok().flatten() {
            Some(id) => id,
            None => continue,
        };
        if let Err(e) = ensure_occurrence_for_phrase(db, phrase_id) { eprintln!("[callgraph_v2] JIT occurrence build failed for phrase_id={}: {}", phrase_id, e); }
        // Prefer same-directory definitions.
        let mut stmt = match db.prepare_cached(
            "SELECT f.file_path, o.line, o.tag FROM occurrence o
             JOIN file_map f ON f.id = o.file_id
             WHERE o.phrase_id = ?1 AND o.is_def != 0
               AND f.file_path NOT LIKE '%.md'
               AND f.file_path NOT LIKE '%.txt'
               AND f.file_path NOT LIKE '%.toml'
             ORDER BY (f.file_path NOT LIKE '%' || ?2 || '%') ASC,
                      (o.tag = 1) DESC,
                      (f.file_path LIKE '%/tests/%') ASC,
                      LENGTH(f.file_path) ASC,
                      o.occ_id LIMIT 20",
        ).ok() {
            Some(s) => s,
            None => continue,
        };
        let mut rows = match stmt.query(rusqlite::params![phrase_id, &context_dir]).ok() {
            Some(r) => r,
            None => continue,
        };
        // V61: don't swallow row errors as "no definition" — a corrupted row
        // silently killed the whole candidate loop.
        loop {
            match rows.next() {
                Ok(Some(r)) => {
                    let fp: String = match r.get(0) {
                        Ok(v) => v,
                        Err(e) => { eprintln!("[callgraph_v2] find_definition row get: {}", e); continue; }
                    };
                    let ln: i32 = match r.get(1) {
                        Ok(v) => v,
                        Err(e) => { eprintln!("[callgraph_v2] find_definition row get: {}", e); continue; }
                    };
                    if find_function_body(&fp, ln).is_some() {
                        return Some((fp, ln));
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("[callgraph_v2] find_definition next: {}", e);
                    return None;
                }
            }
        }
    }
    // Fall back to unconstrained find_definition.
    find_definition(db, name)
}

/// V13 Fix C: Validate that the anchor line is a real function definition.
/// If not (e.g., anchor points at a doc comment), scan forward up to 50 lines
/// to find the actual definition matching `name`. Grammar-free — uses
/// file_meta::fn_names which is built by the structural classifier.
fn validate_and_resolve_anchor(anchor_file: &str, anchor_line: i32, name: &str) -> i32 {
    let meta = match crate::file_meta::get(anchor_file) {
        Some(m) => m,
        None => return anchor_line,
    };
    let idx = (anchor_line as usize).saturating_sub(1);
    if idx >= meta.fn_names.len() {
        return anchor_line;
    }
    // Check if anchor line is a function definition matching the name.
    let target = name.to_ascii_lowercase();
    if meta.fn_names[idx].eq_ignore_ascii_case(&target) {
        return anchor_line;
    }
    // Anchor is NOT the definition. Scan forward up to 50 lines.
    for offset in 1..=50 {
        let candidate = idx + offset;
        if candidate >= meta.fn_names.len() {
            break;
        }
        if meta.fn_names[candidate].eq_ignore_ascii_case(&target) {
            return (candidate + 1) as i32;
        }
    }
    // Also scan backward (might be above the anchor).
    for offset in 1..=20 {
        if offset > idx {
            break;
        }
        let candidate = idx - offset;
        if meta.fn_names[candidate].eq_ignore_ascii_case(&target) {
            return (candidate + 1) as i32;
        }
    }
    // No better anchor found — return original.
    anchor_line
}

/// Build a complete call graph for a function: callers (who calls it) and
/// callees (what it calls).
///
/// Grammar-free approach:
///   1. Find the function's definition site (auto-anchor from IS_DEF hits)
///   2. Read its brace-delimited body
///   3. Extract every identifier-followed-by-`(` pattern in the body
///   4. Filter out keywords/universally-stopped words
///   5. For each filtered callee name, find its definition site
///   6. For callers, reuse type-flow find_references
///   7. V15: optionally expand delegate callees recursively up to max_depth.
///      Default depth=1 keeps existing behavior. depth=3 traces
///      block_on → block_on_inner → schedule → push/wake/queue.
pub fn build_call_graph(
    db: &Connection, raw_name: &str, _path: &str,
    anchor: Option<(String, i32)>,
    max_depth: usize,
) -> rusqlite::Result<CallGraph> {
    // V74: default excludes test/bench files from callers (production view).
    build_call_graph_ext(db, raw_name, _path, anchor, max_depth, false)
}

/// V74: `include_tests=true` keeps test/bench files in the caller set.
/// impact() and test-plan() need them — they were silently empty because the
/// production filter dropped every test caller.
pub fn build_call_graph_ext(
    db: &Connection, raw_name: &str, _path: &str,
    anchor: Option<(String, i32)>,
    max_depth: usize,
    include_tests: bool,
) -> rusqlite::Result<CallGraph> {
    // V28 Fix 4: Type-aware resolution. If the query is `Type::method`, find
    // the type's file via file-path heuristic, then look for the method in
    // `impl Type` blocks in that file.
    let (anchor_file, anchor_line) = match anchor {
        Some((f, l)) if !f.is_empty() => (f, l),
        _ => {
            if let Some((type_name, method_name)) = raw_name.split_once("::") {
                if let Some(resolved) = resolve_type_method(db, type_name, method_name) {
                    resolved
                } else {
                    // Fall back to regular find_definition.
                    match find_definition(db, raw_name) {
                        Some(x) => x,
                        None => {
                            return Ok(CallGraph {
                                anchor_name: raw_name.to_string(),
                                anchor_file: String::new(),
                                anchor_line: 0,
                                source_preview: format!("(no definition found for '{}' in index)", raw_name),
                                callees: vec![],
                                callers: vec![],
                            });
                        }
                    }
                }
            } else {
                match find_definition(db, raw_name) {
                    Some(x) => x,
                    None => {
                        return Ok(CallGraph {
                            anchor_name: raw_name.to_string(),
                            anchor_file: String::new(),
                            anchor_line: 0,
                            source_preview: format!("(no definition found for '{}' in index)", raw_name),
                            callees: vec![],
                            callers: vec![],
                        });
                    }
                }
            }
        }
    };

    // V13 Fix C: Anchor validation. If the anchor line is NOT a function definition
    // (e.g., it's a doc comment), scan forward to find the real definition.
    let anchor_line = validate_and_resolve_anchor(&anchor_file, anchor_line, raw_name);
    // V59: brace_graph now strips comments/strings/lifetimes — a `{` inside
    // backticks in a doc comment used to open a phantom block, collapsing the
    // whole file into one unterminated root (children=0, fn_names empty) and
    // silently killing callgraphs, find_definition and methods_on.

    // Step 2: Find function body.
    let (body_start, body_end) = match find_function_body(&anchor_file, anchor_line) {
        Some(x) => x,
        None => (anchor_line, anchor_line + 50), // fallback: 50 lines
    };

    // Step 3: Read source file and extract body lines. P2-2: use file_meta cache.
    let all_lines = read_all_lines(&anchor_file).unwrap_or_default();
    let body_lines: Vec<String> = all_lines
        .iter()
        .skip((body_start - 1).max(0) as usize)
        .take((body_end - body_start + 1).max(1) as usize)
        .cloned()
        .collect();

    // Step 3.5: Build source_preview — first 3 lines of the function.
    let source_preview = body_lines.iter().take(3).cloned().collect::<Vec<_>>().join("\n");

    // V59 B2: REVERTED V58 P6b — brace-graph method_calls_in only captures
    // `x.y()` method-call nodes; plain fn calls (`foo(...)`) are not graph
    // nodes, so classify_structural's helpers vanished from callgraphs.
    // Body-line scanning is required for correctness; file_meta already
    // caches the lines so the "re-tokenization" cost was just the scan.
    let patterns: Vec<(i32, String, String)> = extract_call_patterns(body_start, &body_lines);

    // Step 5: Filter and dedupe, look up definitions.
    let mut seen_callees: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut callees: Vec<Callee> = Vec::new();
    for (call_line, ident, source) in patterns {
        if STOPWORDS.contains(&ident.as_str()) { continue; }
        if ident == raw_name { continue; } // skip recursive self-calls in callees
        if seen_callees.contains(&ident) { continue; }
        seen_callees.insert(ident.clone());
        // Try to find a definition for this callee.
        let (def_file, def_line) = find_definition(db, &ident).map(|(f, l)| (Some(f), Some(l))).unwrap_or((None, None));
        // V64: skip std-lib/primitive methods — they have no def in the index
        // (def_file None) or their "definition" resolved into a random indexed
        // file (trim_start → lib.rs:113 noise). Keep only callees that resolve
        // to a real source-file definition with a plausible name match.
        if let (Some(ref df), Some(dl)) = (&def_file, &def_line) {
            // The definition's line must actually contain the identifier —
            // guards against phrase-fallback resolving to unrelated files.
            // V65: def_line is 1-indexed; file_meta.lines is 0-indexed.
            let contains = crate::file_meta::get(df)
                .and_then(|m| m.lines.get(dl.saturating_sub(1) as usize).map(|s| s.contains(&ident)))
                .unwrap_or(false);
            if !contains { continue; }
        } else {
            // No definition anywhere — likely a std method. Drop it.
            continue;
        }
        callees.push(Callee {
            name: ident,
            def_file,
            def_line,
            call_line,
            source,
        });
    }

    // V22: Multi-hop expansion for delegate entry-points (block_on → block_on_inner → ...).
    // When max_depth > 1, find the delegate callee (name starts with anchor name)
    // and recursively expand it up to max_depth levels.
    if max_depth > 1 && !callees.is_empty() {
        let prefix = raw_name.rsplit("::").next().unwrap_or(raw_name);
        // Find the delegate: the callee whose name contains the prefix
        // (e.g., block_on → block_on_inner) or ends with the same suffix.
        let delegate_idx = callees.iter().position(|c| {
            let cn = c.name.rsplit("::").next().unwrap_or(&c.name);
            cn != prefix
                && (cn.starts_with(prefix) || cn.contains(&format!(".{}", prefix)))
        });
        if let Some(idx) = delegate_idx {
            let delegate_name = callees[idx].name.clone();
            let delegate_anchor = callees[idx].def_file.clone()
                .zip(callees[idx].def_line);
            // Recursively get the delegate's callees.
            if let Ok(sub_cg) = build_call_graph(
                db, &delegate_name, _path,
                delegate_anchor.or(Some((anchor_file.clone(), anchor_line))),
                max_depth.saturating_sub(1),
            ) {
                // Merge sub-callees into our callees list, skipping duplicates.
                for sub in sub_cg.callees {
                    if !seen_callees.contains(&sub.name) {
                        seen_callees.insert(sub.name.clone());
                        callees.push(sub);
                    }
                }
            }
        }
    }

    // Sort: defined callees first, then by call_line.
    callees.sort_by(|a, b| {
        match (a.def_file.is_some(), b.def_file.is_some()) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.call_line.cmp(&b.call_line),
        }
    });
    // V35: Filter callee noise.
    // 1. Remove callees with no definition in the codebase (stdlib, macros, type casts).
    // 2. Remove known noisy callees (mem, Box, Pin, size_of, etc.) that are always
    //    type-level operations, not real function calls.
    let defined_count = callees.iter().filter(|c| c.def_file.is_some()).count();
    if defined_count > 0 {
        callees.retain(|c| c.def_file.is_some());
    }
    let noise_set = ["mem", "size_of", "Box", "Pin", "new_unnamed", "NewUnnamed",
        "all", "any", "root", "task", "next", "as_u64", "mutex", "linked_list",
        "trace", "mocks", "dump", "udp", "id", "Trace", "SpawnMeta"];
    callees.retain(|c| !noise_set.contains(&c.name.as_str()));

    // Step 6: Callers — reuse type-flow find_references.
    let callers = build_callers(db, raw_name, &anchor_file, anchor_line, include_tests)?;

    Ok(CallGraph {
        anchor_name: raw_name.to_string(),
        anchor_file,
        anchor_line,
        source_preview,
        callees,
        callers,
    })
}

/// V28 Fix 4: Resolve `Type::method` to a definition site.
/// Strategy: find the type's file via file-path heuristic (lowercase type name),
/// then find the method in an `impl Type` block in that file.
fn resolve_type_method(db: &Connection, type_name: &str, method_name: &str) -> Option<(String, i32)> {
    let lower_type = type_name.to_lowercase();
    // Try common file naming patterns: Handle → handle.rs, Runtime → runtime.rs
    let candidate_paths = vec![
        format!("{}.rs", lower_type),
        format!("{}/mod.rs", lower_type),
    ];
    // Search file_map for files matching the type name.
    if let Ok(mut stmt) = db.prepare_cached(
        "SELECT file_path FROM file_map WHERE file_path LIKE ?1 OR file_path LIKE ?2 ORDER BY LENGTH(file_path) ASC LIMIT 5"
    ) {
        let p1 = format!("%/{}.rs", lower_type);
        let p2 = format!("%/{}/mod.rs", lower_type);
        if let Ok(rows) = stmt.query_map(rusqlite::params![p1, p2], |r| r.get::<_, String>(0)) {
            for row in rows.flatten() {
                // Found a candidate file. Now look for the method in it.
                if let Some((f, l)) = find_method_in_file(db, &row, type_name, method_name) {
                    return Some((f, l));
                }
            }
        }
    }
    let _ = candidate_paths; // suppress unused warning
    None
}

/// V28: Find a method definition within a specific file.
/// Looks for `impl TypeName` blocks and finds the method within.
fn find_method_in_file(db: &Connection, file_path: &str, type_name: &str, method_name: &str) -> Option<(String, i32)> {
    // Get all lines with the method name as is_def=1
    if let Some(meta) = crate::file_meta::get(file_path) {
        for (idx, line) in meta.lines.iter().enumerate() {
            let trimmed = line.trim_start();
            // Check if this line defines the method (pub fn method_name( or fn method_name()
            if (trimmed.starts_with("pub fn ") || trimmed.starts_with("fn ") || trimmed.starts_with("async fn "))
                && trimmed.contains(&format!("fn {}", method_name))
            {
                // Verify this is in an impl block for our type by checking nearby lines.
                // Look backward for `impl TypeName` or `impl ... TypeName`.
                for back in (0..idx.min(200)).rev() {
                    let back_line = &meta.lines[back];
                    let bt = back_line.trim_start();
                    if bt.starts_with("impl ") && (bt.contains(type_name) || bt.contains(&format!("for {}", type_name))) {
                        return Some((file_path.to_string(), (idx + 1) as i32));
                    }
                }
            }
        }
    }
    let _ = db; // suppress unused warning
    None
}
/// Returns up to 20 callers filtered to non-anchor contexts.
/// V28: Use pattern_hybrid instead of type_flow — type_flow requires an
/// exact type-flow anchor match and returns 0 hits for cross-file callers.
/// pattern_hybrid is broader and finds all call sites containing the name.
fn build_callers(
    db: &Connection, raw_name: &str, anchor_file: &str, anchor_line: i32,
    include_tests: bool,
) -> rusqlite::Result<Vec<Caller>> {
    // V29: Cap pattern_hybrid at 30 hits (was 100) to reduce token bloat.
    // V28 fix: pattern_hybrid returns absolute paths. anchor_file may be
    // relative. Compare on path suffix to handle both cases.
    // V28 Fix 4: For Type::method queries, use just the method name.
    // V29 Phase 2: Skip test/example/bench files — they're not production callers.
    let search_name = if let Some((_, m)) = raw_name.split_once("::") { m } else { raw_name };
    let anchor_suffix = std::path::Path::new(anchor_file)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    // V65: callers must NOT be similarity-gated. pattern_hybrid drops real
    // call sites when the call context differs from the def context (def vs
    // call windows rarely match). A call site is a call site — query the
    // occurrence table directly for non-def occurrences of the phrase.
    let phrase_id = match crate::symbol::phrase_id_for(db, search_name)? {
        Some(id) => id,
        None => {
            return Ok(vec![]);
        }
    };
    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id
         WHERE o.phrase_id = ?1 AND f.is_source = 1 AND o.is_def = 0
         ORDER BY o.line LIMIT 200",
    )?;
    let hits: Vec<crate::symbol::OccHit> = stmt.query_map(params![phrase_id], |r| {
        Ok(crate::symbol::OccHit {
            occ_id: r.get(0)?,
            file_id: r.get(1)?,
            file_path: r.get(2)?,
            line: r.get(3)?,
            col: r.get(4)?,
            is_def: r.get::<_, i64>(5)? != 0,
            block_id: r.get(6)?,
            similarity: 1.0,
        })
    })?.filter_map(|r| r.ok()).collect();
    let mut callers = Vec::new();
    let mut deferred_same_file: Vec<Caller> = Vec::new();
    for h in hits.iter() {
        // V51: Skip the anchor definition line itself.
        // h.line is 0-indexed (from occurrence table); anchor_line is 1-indexed (from MCP params).
        let anchor_line_0idx = anchor_line.saturating_sub(1);
        if h.line == anchor_line_0idx
            && (h.file_path == anchor_file
                || h.file_path.ends_with(&anchor_file)
                || (!anchor_suffix.is_empty() && h.file_path.ends_with(&anchor_suffix)))
        {
            continue;
        }
        // V29 Phase 2: Skip test/example/bench files — not production callers.
        // V66c: match "bench/" with or without leading slash (relative corpus
        // paths like "bench/reliary_bench.py" have no leading separator).
        // V74: impact/test-plan pass include_tests=true to keep them.
        let fp = &h.file_path;
        if !include_tests
            && (fp.contains("/tests/") || fp.contains("/test/") || fp.contains("/examples/")
                || fp.contains("/benches/") || fp.contains("/bench/") || fp.starts_with("bench/")
                || fp.starts_with("/bench/") || fp.ends_with("_test.rs")
                || fp.ends_with("_tests.rs"))
        {
            continue;
        }
        // V57e: skip non-source files (markdown plans, configs, docs) — they
        // pollute callers with prose mentions. Grammar-free: extension check.
        let is_source_file = fp.ends_with(".rs") || fp.ends_with(".py") || fp.ends_with(".js")
            || fp.ends_with(".ts") || fp.ends_with(".tsx") || fp.ends_with(".jsx")
            || fp.ends_with(".go") || fp.ends_with(".c") || fp.ends_with(".h")
            || fp.ends_with(".cpp") || fp.ends_with(".hpp") || fp.ends_with(".java")
            || fp.ends_with(".rb") || fp.ends_with(".php") || fp.ends_with(".kt")
            || fp.ends_with(".swift") || fp.ends_with(".scala") || fp.ends_with(".cs");
        if !is_source_file { continue; }
        // Read source line for the hit. h.line is 0-based.
        let line_text = if let Some(meta) = crate::file_meta::get(&h.file_path) {
            meta.lines.get(h.line.max(0) as usize).cloned().unwrap_or_default()
        } else {
            match std::fs::read_to_string(&h.file_path) {
                Ok(content) => content.lines().nth(h.line.max(0) as usize).unwrap_or("").to_string(),
                Err(_) => String::new(),
            }
        };
        // V28: Filter — only include lines that look like a CALL (name followed by `(` or `::`),
        // NOT a definition (starts with `pub fn`/`fn`/`async fn`).
        // Also accept `name_inner` (delegate pattern) and `self.name` as call patterns.
        let trimmed = line_text.trim_start();
        if trimmed.starts_with("pub fn ") || trimmed.starts_with("fn ") || trimmed.starts_with("async fn ") {
            continue; // Skip definitions
        }
        // Check if the line contains the search name followed by `(` or `::` (call pattern).
        // V74: strip trailing comments first — `// foo(x)` is not a call site.
        let code_only = crate::structural::strip_line_comment(&line_text);
        let has_call_pattern = code_only.contains(&format!("{}(", search_name))
            || code_only.contains(&format!("{}::", search_name))
            || code_only.contains(&format!(".{}", search_name));
        if !has_call_pattern { continue; }
        // Reject prefix matches (`contains` would accept `foo_extra(` for
        // `foo`): the char after each candidate position must be a boundary.
        let boundary_ok = {
            let bytes = code_only.as_bytes();
            let mut ok = false;
            let mut from = 0usize;
            while let Some(pos) = code_only[from..].find(search_name) {
                let i = from + pos;
                let after = i + search_name.len();
                let next = bytes.get(after).copied().unwrap_or(b' ');
                let next_ok = matches!(next, b'(' | b':' | b'.' | b' ' | b'\t' | b'<' | b'>' | b'!' | b'?' | b')' | b',' | b';' | b'[' | b']' | b'&' | b'*' | b'=' | b'{' | b'}');
                let prev = if i == 0 { b' ' } else { bytes[i - 1] };
                // V74b: `.` is ALLOWED — `self.foo(` and `Type::foo(` are real
                // call sites (the previous version rejected them, dropping
                // every method-call caller). Only reject when the preceding
                // char makes the match part of a longer identifier.
                let prev_ok = !(prev.is_ascii_alphanumeric() || prev == b'_');
                if next_ok && prev_ok { ok = true; break; }
                from = i + 1;
                if from >= code_only.len() { break; }
            }
            ok
        };
        if !boundary_ok { continue; }
        // V59 C2: include the enclosing function name — "file.rs:123 in fn
        // foo()" is far more actionable for the model than a bare line ref.
        let enclosing = crate::file_meta::get(&h.file_path)
            .and_then(|m| m.fn_names.get(h.line.max(0) as usize).cloned())
            .filter(|n| !n.is_empty());
        // V59 A2b: same-file self-calls are noise for "who calls X" —
        // deprioritize them so cross-file callers surface in top-5.
        let same_file = h.file_path.ends_with(anchor_file)
            || std::path::Path::new(&anchor_file)
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| h.file_path.ends_with(n))
                .unwrap_or(false);
        let caller = Caller {
            name: match enclosing {
                Some(ref f) => format!("{}::{}", raw_name, f),
                None => raw_name.to_string(),
            },
            file: h.file_path.clone(),
            line: h.line + 1,
            source: line_text,
        };
        if same_file { deferred_same_file.push(caller); }
        else { callers.push(caller); }
        if callers.len() >= 20 { break; }
    }
    // V59 A2b: fill remaining slots with same-file calls (still real callers).
    for c in deferred_same_file {
        if callers.len() >= 20 { break; }
        callers.push(c);
    }
    Ok(callers)
}

// ── Phase C: methods_on type ──

#[derive(Serialize, Clone)]
pub struct MethodOn {
    pub name: String,
    pub file: String,
    /// 1-indexed source line, matching `BraceNode.start_line` (see
    /// `build_brace_graph`: `line_no = line_idx + 1`). Display sites must print
    /// this value UNCHANGED — the occurrence table is 0-indexed, and adding one
    /// here produced a systematic +1 on every reported method line (V75).
    pub line: i32,
    pub source: String,
    /// Whether the declaration line carries a visibility qualifier (`pub`,
    /// `pub(crate)`, `pub(super)`, `pub(in ...)`). Grammar-free: derived from
    /// the leading token(s) of the source line, not a language keyword list.
    #[serde(default)]
    pub is_pub: bool,
    /// True when this entry is a struct field (the no-impl-methods fallback),
    /// so callers render it as `name: Type` rather than `fn name`.
    #[serde(default)]
    pub is_field: bool,
}

#[derive(Serialize)]
pub struct MethodsResult {
    pub type_name: String,
    pub methods: Vec<MethodOn>,
    pub impl_blocks_found: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related_types: Vec<String>,
    // V66d: location of the first matching impl block, so callers can cite
    // "impl at file:line" — the impl line is a distinct fact from the methods.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impl_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impl_line: Option<i32>,
}

/// Find all methods declared on a type.
///
/// Grammar-free approach: scan every file in the corpus for `impl ... <type>`
/// brace blocks (using brace-graph for boundaries), extract the function
/// definitions inside each block. Works on Rust, Java, C++, Go (struct methods).
///
/// To handle trait impls (`impl Trait for Type`), we match BOTH the trait name
/// and the target type. If either matches, the methods are returned.
///
/// Also finds sibling types in the same file whose name contains the requested
/// V59 B1: find types implementing a trait (`impl Trait for X`) OR
/// deriving it (`#[derive(..., Trait, ...)]`).
/// Grammar-free: scans brace-graph root lines of indexed .rs files for the
/// pattern `impl <Trait> for <Type>` (optionally generic `impl<T> Trait for X`)
/// and `#[derive(..., Trait, ...)]` followed by a `struct`/`enum`/`union`
/// declaration. Bounded at 400 files like find_methods_on's fallback.
pub struct TraitImpl {
    pub type_name: String,
    pub file: String,
    pub line: i32,
}

pub fn find_trait_impls(db: &Connection, trait_name: &str) -> Vec<TraitImpl> {
    find_trait_impls_scoped(db, trait_name, None)
}

/// Same as [`find_trait_impls`] but restricts results to files whose path
/// contains `path_filter` (when provided). This prevents the trait-impl
/// fallback from leaking types from unrelated crates when the caller
/// supplied a `path_filter`.
pub fn find_trait_impls_scoped(
    db: &Connection,
    trait_name: &str,
    path_filter: Option<&str>,
) -> Vec<TraitImpl> {
    let mut out = Vec::new();
    // Needle excludes the leading `impl` — we locate the trait name then
    // verify what precedes it (`impl ` or `impl<...> `). Including "impl "
    // in the needle made find() match at pos 0, leaving `before` empty.
    let needle = format!("{} for ", trait_name);
    let mut stmt = match db.prepare_cached(
        // V59: no LIMIT — 707 files scanned in ~50ms via cached file_meta;
        // a 400-file alphabetical cut silently excluded valid impl sites.
        "SELECT file_path FROM file_map WHERE file_path LIKE '%.rs' ORDER BY file_path",
    ) {
        Ok(s) => s,
        Err(_) => return out,
    };
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(mut rows) = stmt.query(rusqlite::params![]) {
        while let Ok(Some(r)) = rows.next() {
            if let Ok(fp) = r.get::<_, String>(0) {
                candidates.push(fp);
            }
        }
    }
    for fp in candidates {
        // Scope filter: skip files outside the caller's path_filter.
        if let Some(pf) = path_filter {
            if !pf.is_empty() && !fp.contains(pf) {
                continue;
            }
        }
        let meta = match crate::file_meta::get(&fp) {
            Some(m) => m,
            None => continue,
        };
        // Scan raw lines — impl headers are single-line in ~all Rust code and
        // this avoids walking every brace node.
        let mut i = 0;
        while i < meta.lines.len() {
            let line = &meta.lines[i];
            let t = line.trim_start();
            // Pattern 1: `impl Trait for Type` (existing).
            if t.starts_with("impl ") || t.starts_with("impl<") {
                if let Some(pos) = t.find(&needle) {
                    let before = t[..pos].trim_end();
                    let ok = before == "impl"
                        || (before.starts_with("impl<") && before.ends_with(">"));
                    if ok {
                        let rest = &t[pos + needle.len()..];
                        // Type name = first identifier of rest
                        let ty: String = rest
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '<')
                            .collect();
                        let ty = ty.split('<').next().unwrap_or("").trim().to_string();
                        if !ty.is_empty() {
                            out.push(TraitImpl {
                                type_name: ty,
                                file: fp.clone(),
                                line: i as i32 + 1,
                            });
                        }
                    }
                    i += 1;
                    continue;
                }
            }
            // Pattern 2: `#[derive(..., Trait, ...)]` followed by struct/enum/union.
            if t.starts_with("#[derive(") {
                if let Some(close) = t.find(')') {
                    let inner = &t[t.len().min(t.find("#[derive(").unwrap() + 9)..close.max(9)];
                    // Split the derive list on commas and look for an exact
                    // token match on trait_name (grammar-free: no keyword list).
                    let matched = inner
                        .split(',')
                        .any(|s| s.trim() == trait_name);
                    if matched {
                        // The type declaration is on a subsequent non-attribute,
                        // non-blank line. Look ahead up to 4 lines.
                        for j in (i + 1)..meta.lines.len().min(i + 5) {
                            let nt = meta.lines[j].trim_start();
                            if nt.is_empty() || nt.starts_with('#') || nt.starts_with("//") {
                                continue;
                            }
                            if let Some(ty) = decl_type_name(nt) {
                                out.push(TraitImpl {
                                    type_name: ty,
                                    file: fp.clone(),
                                    line: j as i32 + 1,
                                });
                            }
                            break; // first non-attribute line decides
                        }
                    }
                }
            }
            i += 1;
        }
        if out.len() >= 20 {
            break;
        }
    }
    out
}

/// Grammar-free: extract the identifier following `struct`/`enum`/`union`
/// on a declaration line. Returns None if the line is not a type declaration.
fn decl_type_name(line: &str) -> Option<String> {
    for kw in ["struct ", "enum ", "union "] {
        if let Some(pos) = line.find(kw) {
            let rest = &line[pos + kw.len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

pub fn find_methods_on(db: &Connection, type_name: &str) -> rusqlite::Result<MethodsResult> {
    let mut methods: Vec<MethodOn> = Vec::new();
    let mut impl_blocks_found = 0usize;
    let mut impl_loc: Option<(String, i32)> = None;
    // M7: SmallVec for the internal collection (avoids heap for ≤8 types).
    // Convert to Vec at the end for the Serialize struct field.
    let mut related_types: SmallVec<[String; 8]> = SmallVec::new();

    // Get all files containing the type name (lazy occurrence).
    // Arc 62 bug fix: phrases are stored lowercase. Try lowercase query.
    let lowercase_name = type_name.to_lowercase();
    let phrase_id = phrase_id_for(db, &lowercase_name).ok().flatten();
    let mut files: Vec<String> = Vec::new();
    if let Some(pid) = phrase_id {
        if let Err(e) = ensure_occurrence_for_phrase(db, pid) {
            eprintln!("[callgraph_v2] ensure_occurrence_for_phrase failed for pid={}: {}", pid, e);
        }
        let mut stmt = db.prepare_cached(
            "SELECT DISTINCT f.file_path FROM occurrence o
             JOIN file_map f ON f.id = o.file_id
             WHERE o.phrase_id = ?1 AND f.file_path LIKE '%.rs'
             ORDER BY f.file_path",
        )?;
        let mut rows = stmt.query(rusqlite::params![pid])?;
        while let Some(r) = rows.next()? {
            files.push(r.get(0)?);
        }
    }
    // V57: if the phrase lookup missed (compound names stem-collide at
    // ingest, e.g. StructuralResult -> structur), fall back to scanning
    // .rs files' brace graphs for a `struct <Type>` / `class <Type>:`
    // definition line (grammar-free, no index dependence).
    if files.is_empty() && !type_name.is_empty() {
                if let Ok(mut stmt) = db.prepare_cached(
            "SELECT file_path FROM file_map WHERE file_path LIKE '%.rs' ORDER BY file_path LIMIT 400"
        ) {
            let mut rows = stmt.query(rusqlite::params![])?;
            let mut candidates: Vec<String> = Vec::new();
            while let Some(r) = rows.next()? {
                candidates.push(r.get(0)?);
            }
            for fp in candidates {
                let root = crate::file_meta::get(&fp)
                    .map(|m| m.brace_graph.clone())
                    .or_else(|| get_brace_graph(&fp));
                if let Some(root) = root {
                    if node_mentions_struct(&root, type_name) {
                                                files.push(fp);
                    }
                }
            }
        }
    }

    for fp in &files {
        let root = crate::file_meta::get(fp)
            .map(|m| m.brace_graph.clone())
            .or_else(|| get_brace_graph(fp));
        let root = match root {
            Some(r) => r,
            None => continue,
        };
        // Walk all brace-blocks, find those whose start-line text contains
        // the type name and has `impl` or `for <type>` pattern.
        collect_methods_in_impl_blocks(&root, fp, type_name, &mut methods, &mut impl_blocks_found, &mut impl_loc);
        // Also find sibling types: scan the file for impl blocks whose target
        // type name CONTAINS our type as a stem. Grammar-free string overlap.
        find_sibling_types(&root, fp, type_name, &mut related_types);
    }
    // Dedupe by (name, file, line).
    methods.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    methods.dedup_by(|a, b| a.file == b.file && a.line == b.line);

    // V57: fallback — if a type has no impl methods, list its struct FIELDS
    // (grammar-free: lines of `name: Type` inside the type's definition block).
    if methods.is_empty() {
        let mut fields: Vec<(String, String, String, i64, bool)> = Vec::new();
        for fp in &files {
            // Direct line scan — no brace-graph dependency (the graph can be
            // stale/empty from the file_meta cache for freshly-added files).
            let content = std::fs::read_to_string(fp).unwrap_or_default();
            let lines: Vec<&str> = content.lines().collect();
            let mut in_struct = false;
            let mut struct_depth = 0usize;
            for (li, line) in lines.iter().enumerate() {
                let t = line.trim_start();
                if !in_struct {
                    let is_struct_line = t.starts_with("struct ") || t.starts_with("class ")
                        || t.starts_with("pub struct ") || t.starts_with("pub(crate) struct ");
                    if is_struct_line && t.contains(type_name) {
                        in_struct = true;
                        // V60: start at 0 — the struct line's own `{` (if any)
                        // counts below. A struct without a brace on the decl
                        // line (multi-line `struct Foo\n{`) still enters via
                        // the next line's `{`.
                        struct_depth = 0;
                        continue;
                    }
                    continue;
                }
                // Inside the struct block: track brace depth to find the end.
                let opens = t.matches('{').count();
                let closes = t.matches('}').count();
                if opens > 0 { struct_depth += opens; }
                if closes > 0 {
                    struct_depth = struct_depth.saturating_sub(closes);
                    // V60: exit the struct block entirely when it closes so
                    // later lines (tests, other code) aren't scanned as fields.
                    // The `if !in_struct` branch re-enters for the 2nd+ structs.
                    if struct_depth == 0 {
                        in_struct = false;
                        continue;
                    }
                }
                if opens > 0 { continue; }
                // Field line: `name: Type` (not a method/comment).
                if t.contains(':') && !t.starts_with("//") && !t.starts_with("///")
                    && !t.starts_with('#') && !t.contains("fn ") && !t.contains("impl ")
                {
                    if let Some(colon) = t.find(':') {
                        let before = t[..colon].trim();
                        // Strip visibility prefix: `pub name: T`, `pub(crate) name: T`.
                        let mut toks = before.split_whitespace();
                        let first = toks.next().unwrap_or("");
                        let vis = first == "pub"
                            || first == "pub(crate)"
                            || first == "pub(super)"
                            || first.starts_with("pub(");
                        let name = if vis {
                            toks.next().unwrap_or("")
                        } else {
                            first
                        };
                        if !name.is_empty()
                            && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
                        {
                            // V59g: capture the field TYPE for evidence output.
                            let ftype = t[colon + 1..].trim().trim_end_matches(',').to_string();
                            fields.push((name.to_string(), ftype, fp.clone(), (li + 1) as i64, vis));
                        }
                    }
                }
            }
        }
        // Dedupe by (name, file): a struct scan can see the same block twice
        // (multiple type-name matches), but two DISTINCT fields that happen to
        // share a type (`x: i32` and `y: i32`) must both survive. The old key
        // was (type, file), which silently dropped every same-typed field after
        // the first.
        fields.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)).then(a.3.cmp(&b.3)));
        fields.dedup_by(|a, b| a.0 == b.0 && a.2 == b.2);
        for (fname, ftype, ffile, fline, fpub) in fields {
            methods.push(MethodOn { name: fname, file: ffile, line: fline as i32, source: ftype, is_pub: fpub, is_field: true });
        }
    }

    related_types.sort();
    related_types.dedup();

    Ok(MethodsResult {
        type_name: type_name.to_string(),
        methods,
        impl_blocks_found,
        related_types: related_types.into_vec(),
        impl_file: impl_loc.as_ref().map(|(f, _)| f.clone()),
        impl_line: impl_loc.map(|(_, l)| l),
    })
}

/// V57: does any brace node start with `struct <Type>` / `class <Type>:` /
/// `enum <Type>` (case-insensitive on the type name)?
fn node_mentions_struct(node: &crate::brace_graph::BraceNode, type_name: &str) -> bool {
    let lft = node.first_line_text.trim_start();
    let starts = lft.starts_with("struct ") || lft.starts_with("pub struct ")
        || lft.starts_with("pub(crate) struct ") || lft.starts_with("enum ")
        || lft.starts_with("pub enum ") || lft.starts_with("class ")
        || lft.starts_with("pub class ");
    if starts {
        // Take the identifier immediately after `struct`/`enum`/`class`.
        let after_kw = lft.find("struct ").map(|i| i + 7)
            .or_else(|| lft.find("enum ").map(|i| i + 5))
            .or_else(|| lft.find("class ").map(|i| i + 6));
        if let Some(start) = after_kw {
            let rest = lft[start..].trim_start();
            let name: String = rest.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if name.eq_ignore_ascii_case(type_name) {
                return true;
            }
        }
    }
    for child in &node.children {
        if node_mentions_struct(child, type_name) {
            return true;
        }
    }
    false
}



/// Find sibling types whose name contains `type_name` as a stem.
/// Grammar-free: pure string containment. Scans brace-blocks for impl lines
/// whose target type is a DIFFERENT name but shares the stem.
fn find_sibling_types(
    node: &crate::brace_graph::BraceNode,
    _file_path: &str,
    type_name: &str,
    out: &mut SmallVec<[String; 8]>,
) {
    let lft = &node.first_line_text;
    let lft_trim = lft.trim_start();
    if lft_trim.starts_with("impl") {
        // Extract target type: look for `impl ... for X` or `impl X {` pattern.
        let target_type = extract_impl_target_type(lft);
        if let Some(target) = target_type {
            // Must contain type_name as a stem AND be different from it.
            if target != type_name && target.contains(type_name) && target.len() > type_name.len()
                && !out.contains(&target) {
                    out.push(target);
                }
        }
    }
    // Recurse into children (but not into function bodies — check first line).
    for child in &node.children {
        // Skip recursion into function/method bodies to avoid scanning their
        // internal impl blocks (which don't exist in normal code anyway).
        if child.role != "function_def" && child.role != "method_call" {
            find_sibling_types(child, _file_path, type_name, out);
        } else {
            // For function_def/impl blocks, check THIS node too (it might be
            // an impl block tagged as function_def).
            let child_lft = &child.first_line_text;
            if child_lft.trim_start().starts_with("impl") {
                find_sibling_types(child, _file_path, type_name, out);
            }
        }
    }
}

/// Extract the target type from an impl line.
/// Returns None if no type can be identified.
fn extract_impl_target_type(line: &str) -> Option<String> {
    // Strip `impl` prefix and optional generics/trait prefix.
    let trimmed = line.trim_start();
    if !trimmed.starts_with("impl") { return None; }
    let after_impl = trimmed[4..].trim_start();

    // Case 1: `impl Trait for Type` → Type is after last `for `
    if let Some(for_pos) = after_impl.rfind("for ") {
        let after_for = after_impl[for_pos + 4..].trim_start();
        // The target type starts here and continues until `{`, `<`, `,`, `(`, or whitespace.
        return extract_ident_at_start(after_for);
    }

    // Case 2: `impl Type` → Type is first identifier
    extract_ident_at_start(after_impl)
}

/// Extract the first identifier from a string, stopping at non-ident chars.
fn extract_ident_at_start(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut end = 0;
    let mut in_ident = false;
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'_' || c.is_ascii_alphabetic() {
            if !in_ident { start = i; in_ident = true; }
            end = i + 1;
        } else if in_ident {
            // First non-ident char after start — done.
            return Some(s[start..end].to_string());
        } else if (c as char).is_whitespace() || c == b'(' {
            // Still in leading whitespace.
            continue;
        } else {
            // Non-ident, non-space, non-paren char at start — no identifier.
            return None;
        }
    }
    if in_ident && end > start {
        return Some(s[start..end].to_string());
    }
    None
}

fn collect_methods_in_impl_blocks(
    node: &crate::brace_graph::BraceNode,
    file_path: &str,
    type_name: &str,
    out: &mut Vec<MethodOn>,
    count: &mut usize,
    impl_loc: &mut Option<(String, i32)>,
) {
    // Arc 62 bug fix: DON'T skip function_def nodes — impl blocks are classified
    // as function_def too. Instead, always check the line text for impl keywords.
    let lft = if node.first_line_text.is_empty() {
        read_line(file_path, node.start_line)
    } else {
        node.first_line_text.clone()
    };
    let lft_trim = lft.trim_start();
    let is_impl = lft_trim.starts_with("impl") && lft_trim.len() > 4
        && (lft_trim.chars().nth(4).map(|c| c.is_whitespace()).unwrap_or(false)
            || lft_trim.chars().nth(4) == Some('<'))
        && (lft.contains(type_name) || lft.contains(&format!("for {}", type_name)));
if is_impl {
        *count += 1;
        if impl_loc.is_none() {
            *impl_loc = Some((file_path.to_string(), node.start_line));
        }
        // Extract fn definitions inside this block.
        for child in &node.children {
            // Check if child is a function definition (has `(` after name, or starts with `pub fn`/`fn`/`async fn`).
            let child_lft = if child.first_line_text.is_empty() {
                read_line(file_path, child.start_line)
            } else {
                child.first_line_text.clone()
            };
            let child_trim = child_lft.trim_start();
            let is_fn = child_trim.starts_with("fn ") || child_trim.starts_with("pub fn ")
                || child_trim.starts_with("async fn ") || child_trim.starts_with("pub async fn ")
                || child_trim.starts_with("pub(crate) fn ") || child_trim.starts_with("pub(super) fn ")
                || child_trim.starts_with("const fn ") || child_trim.starts_with("pub const fn ");
            if is_fn {
                // Extract function name from the line text.
                let name = extract_fn_name(&child_lft);
                if !name.is_empty() && name != type_name && name != "Self" {
                    out.push(MethodOn {
                        name,
                        file: file_path.to_string(),
                        line: child.start_line,
                        is_pub: is_pub_decl(&child_lft),
                        is_field: false,
                        source: child_lft,
                    });
                }
            }
        }
    }
    // Always recurse into children to find deeper impl blocks.
    for child in &node.children {
        collect_methods_in_impl_blocks(child, file_path, type_name, out, count, impl_loc);
    }
}

/// Does this declaration line carry a visibility qualifier?
///
/// Grammar-free: we look at the leading token(s) only. `pub`, `pub(crate)`,
/// `pub(super)`, `pub(in path)` all start with the 3 bytes `pub` followed by
/// whitespace or `(`. No language keyword list, no per-language branching.
fn is_pub_decl(text: &str) -> bool {
    let t = text.trim_start();
    let b = t.as_bytes();
    b.len() >= 4
        && &b[..3] == b"pub"
        && (b[3] == b' ' || b[3] == b'\t' || b[3] == b'(')
}

fn extract_fn_name(text: &str) -> String {
    // Skip `pub(crate)` / `pub(super)` / `pub(in path)` qualifiers.
    // Grammar-free: `pub` is a structural prefix, not a keyword. Detection:
    // if line starts with `pub` followed by `(`, skip to the matching `)`.
    let trimmed = text.trim_start();
    let bytes = trimmed.as_bytes();
    let mut start_idx = 0;
    if bytes.len() >= 4 && &bytes[..3] == b"pub" {
        // Check if it's `pub` followed by a qualifier `(...)` or whitespace.
        if bytes.len() > 3 && (bytes[3] == b'(' || bytes[3] == b' ') {
            start_idx = 3;
            if bytes[3] == b'(' {
                // Skip past the `(...)` qualifier
                let mut depth = 1i32;
                for (j, &b) in bytes.iter().enumerate().skip(4) {
                    if b == b'(' { depth += 1; }
                    else if b == b')' {
                        depth -= 1;
                        if depth == 0 { start_idx = j + 1; break; }
                    }
                }
            }
            // Skip whitespace after `pub` or `pub(...)`.
            while start_idx < bytes.len() && (bytes[start_idx] == b' ' || bytes[start_idx] == b'\t') {
                start_idx += 1;
            }
            // Skip `async` if present.
            if start_idx + 5 <= bytes.len() && &bytes[start_idx..start_idx + 5] == b"async" {
                // Make sure it's `async ` followed by fn, not just `async` as a word.
                let after = start_idx + 5;
                if after < bytes.len() && (bytes[after] == b' ' || bytes[after] == b'\t') {
                    start_idx = after + 1;
                    while start_idx < bytes.len() && (bytes[start_idx] == b' ' || bytes[start_idx] == b'\t') {
                        start_idx += 1;
                    }
                }
            }
            // Skip `const` if present.
            if start_idx + 5 <= bytes.len() && &bytes[start_idx..start_idx + 5] == b"const" {
                let after = start_idx + 5;
                if after < bytes.len() && (bytes[after] == b' ' || bytes[after] == b'\t') {
                    start_idx = after + 1;
                    while start_idx < bytes.len() && (bytes[start_idx] == b' ' || bytes[start_idx] == b'\t') {
                        start_idx += 1;
                    }
                }
            }
            // Skip `unsafe` if present.
            if start_idx + 6 <= bytes.len() && &bytes[start_idx..start_idx + 6] == b"unsafe" {
                let after = start_idx + 6;
                if after < bytes.len() && (bytes[after] == b' ' || bytes[after] == b'\t') {
                    start_idx = after + 1;
                    while start_idx < bytes.len() && (bytes[start_idx] == b' ' || bytes[start_idx] == b'\t') {
                        start_idx += 1;
                    }
                }
            }
            // Skip `fn` keyword.
            if start_idx + 2 <= bytes.len() && &bytes[start_idx..start_idx + 2] == b"fn" {
                let after = start_idx + 2;
                if after < bytes.len() && (bytes[after] == b' ' || bytes[after] == b'\t') {
                    start_idx = after + 1;
                }
            }
        }
    }
    // Now find the first identifier starting at start_idx, stop at `(`.
    let mut last_ident_start = 0;
    let mut last_ident_end = 0;
    let mut in_ident = false;
    for (i, &c) in bytes.iter().enumerate().skip(start_idx) {
        if c == b'_' || c.is_ascii_alphanumeric() {
            if !in_ident { last_ident_start = i; in_ident = true; }
            last_ident_end = i + 1;
        } else if c == b'(' {
            if in_ident { return trimmed[last_ident_start..i].to_string(); }
            return String::new();
        } else {
            in_ident = false;
        }
    }
    if last_ident_end > last_ident_start {
        return trimmed[last_ident_start..last_ident_end].to_string();
    }
    String::new()
}

fn read_line(file_path: &str, line: i32) -> String {
    // Use file_meta cache to avoid redundant file reads.
    let li = (line as usize).max(1) - 1;
    if let Some(m) = crate::file_meta::get(file_path) {
        if li < m.lines.len() {
            return m.lines[li].clone();
        }
    }
    if let Ok(content) = std::fs::read_to_string(file_path) {
        content.lines().nth(li).unwrap_or("").to_string()
    } else {
        String::new()
    }
}

/// Read all lines for a file, preferring file_meta cache.
fn read_all_lines(file_path: &str) -> Option<Vec<String>> {
    if let Some(m) = crate::file_meta::get(file_path) {
        return Some(m.lines.clone());
    }
    std::fs::read_to_string(file_path).ok().map(|s| s.lines().map(String::from).collect())
}
#[cfg(test)]
mod v75_tests {
    use super::*;

    #[test]
    fn pub_decl_rejects_private_and_non_declarations() {
        assert!(!is_pub_decl("fn new(start_line: i32) -> Self {"));
        assert!(!is_pub_decl("    fn collect_method_calls(&self) {"));
        assert!(!is_pub_decl("let s = \"pub fn foo()\";"));
        assert!(!is_pub_decl("publish();"));
        assert!(!is_pub_decl("pubz fn x() {}"));
        assert!(!is_pub_decl("pub"));
    }

    #[test]
    fn struct_field_fallback_keeps_same_typed_fields() {
        // Two fields sharing a type must BOTH survive the fallback (the old
        // dedup key was (type, file) and dropped the second).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("reliary_v75f_{}", nanos));
        std::fs::create_dir_all(&dir).unwrap();
        let src = "pub struct Point {\n    pub x: i32,\n    y: i32,\n}\n";
        std::fs::write(dir.join("point.rs"), src).unwrap();

        let db = rusqlite::Connection::open(dir.join("idx.sqlite")).unwrap();
        crate::schema::create_new_db(&db).unwrap();
        crate::ingest::index_directory(&db, dir.to_str().unwrap()).unwrap();
        if let Some(pid) = phrase_id_for(&db, "point").ok().flatten() {
            let _ = crate::lazy_occurrence::ensure_occurrence_for_phrase(&db, pid);
        }
        let mr = find_methods_on(&db, "Point").expect("find_methods_on ok");

        let x = mr.methods.iter().find(|m| m.name == "x").expect("field x survived");
        assert!(x.is_field, "x must be flagged as a field");
        assert_eq!(x.line, 2, "field x is on line 2, got {}", x.line);
        assert!(x.is_pub, "pub x");
        let y = mr.methods.iter().find(|m| m.name == "y").expect("field y survived (same type as x)");
        assert_eq!(y.line, 3, "field y is on line 3, got {}", y.line);
        assert!(!y.is_pub, "y is private");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
