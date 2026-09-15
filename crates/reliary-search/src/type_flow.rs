//! Phase Y + Z + AA: Role-Aware Type-Flow Disambiguation.
//!
//! Phase Y: Predict each candidate's ROLE via grammar-free regex (same patterns
//!   as the bench's autolabeler). Boost candidates whose role matches anchor's role.
//!
//! Phase Z: Resolve receiver types via scope tracking. For `self.poll()`:
//!   1. Find receiver = "self"
//!   2. Walk up to enclosing fn signature → extract self type
//!   3. Resolve Self to enclosing impl Type
//!   4. Match receiver types across candidates
//!
//! Phase AA: Multi-view self-similarity count. Score = count of matching views / 5.

use crate::symbol::{OccHit, block_id_at, file_id_for, phrase_id_for};
use crate::pattern::{best_context_key, context_key_at, match_level};
use crate::compat::{infer_receiver_type, type_jaccard, module_path, module_jaccard, enclosing_fn_name, enclosing_impl_target, function_profile_wasserstein};
use crate::brace_graph::{get_brace_graph, same_scope};
use crate::signature::{call_arity, resolve_pattern_match_binding, extract_call_receiver};
use rusqlite::{params, Connection};
use ahash::AHashMap;
use rustc_hash::FxHashMap;

/// Occurrence tuple read from the occurrence table.
type OccTuple = (i64, i64, String, i32, i32, bool, i64, i64);

// ── Phase Y: Role prediction (grammar-free, no regex crate) ──

/// Predict the role of an occurrence from its line text.
/// Arc 22: grammar-free — uses structural detection, no keywords.
/// Arc 23: comment lines always return "type_name" (prevents doc comment false positives).
pub fn predict_role(line_text: &str) -> &'static str {
    let line = line_text.trim();
    if line.is_empty() { return "type_name"; }

    // Arc 23: comment detection (grammar-free — universal comment syntax).
    if line.starts_with("//") || line.starts_with("#") || line.starts_with("/*") || line.starts_with("*") {
        return "type_name";
    }

    // Structural definition check (grammar-free).
    // Use tag from structural detector to distinguish fn_def from local_var etc.
    let has_open_block = line.ends_with('{') || line.ends_with(':');
    let result = crate::structural::classify_structural(line, 0, has_open_block, false);
    if result.is_def {
        return match result.tag {
            1 | 3 => "function_def",
            2 => "type_name",  // V21: type_def → type_name (was function_def)
            4 => "field_access",
            5 => "param",
            6 => "local_var",
            7 => "import_or_use",
            _ => "function_def",
        };
    }

    // Method call: .identifier( (universal dot notation — grammar-free).
    if contains_method_call(line) {
        return "method_call";
    }

    // Local binding: top-level `=` (structural, no keywords).
    if has_top_level_eq(line) {
        return "local_var";
    }

    // Field access: .identifier without `(` after.
    if line.contains('.') && !contains_method_call(line) {
        return "field_access";
    }

    // Module path: `::` anywhere (grammar-free).
    if line.contains("::") {
        return "module_name";
    }

    // Bare call: name( at start of expression.
    if contains_bare_call(line) {
        return "method_call";
    }

    "type_name"
}

/// Predict role using line text + stem position (Arc 38, opt-in).
///
/// When the stem is known, we can disambiguate more precisely by looking
/// at the actual character before and after the stem in the line. This is
/// grammar-free (no AST, no keywords, no language detection) — only column
/// position inspection.
///
/// Rules:
/// - `.NAME(` → `method_call` (universal OOP notation)
/// - `.NAME` (no parens after) → `field_access`
/// - `::NAME` → `module_name`
/// - bare `NAME(` with line ending in `{` or `:` → `function_def`
///
/// Falls through to `predict_role()` if stem not found or no rule matches.
pub fn predict_role_with_stem(line_text: &str, stem: &str) -> &'static str {
    if stem.is_empty() {
        return predict_role(line_text);
    }

    let line = line_text.trim();
    if line.is_empty() {
        return "type_name";
    }

    // Comment detection (grammar-free — universal).
    if line.starts_with("//") || line.starts_with("#") || line.starts_with("/*") || line.starts_with("*") {
        return "type_name";
    }

    // Import / use / mod statement (multi-language).
    if line.starts_with("use ") || line.starts_with("import ") || line.starts_with("from ")
        || line.starts_with("mod ") || line.starts_with("extern crate ") {
        return "import_or_use";
    }

    // Find the first occurrence of `stem` in `line`. Word-boundary aware.
    let stem_bytes = stem.as_bytes();
    let line_bytes = line.as_bytes();

    let mut pos: Option<usize> = None;
    if line_bytes.len() >= stem_bytes.len() {
        // M7: memchr-based stem search with word-boundary verification.
        // memchr::memmem::find returns the byte offset of the needle.
        // For ASCII needles on ASCII haystacks, this uses SIMD (very fast).
        let finder = memchr::memmem::Finder::new(stem_bytes);
        let mut start = 0;
        while let Some(idx) = finder.find(&line_bytes[start..]) {
            let i = start + idx;
            let before_ok = i == 0 || !is_word_char(line_bytes[i - 1]);
            let after_ok = i + stem_bytes.len() == line_bytes.len()
                || !is_word_char(line_bytes[i + stem_bytes.len()]);
            if before_ok && after_ok {
                pos = Some(i);
                break;
            }
            // Move past this occurrence.
            start = i + 1;
            if start + stem_bytes.len() > line_bytes.len() { break; }
        }
    }

    let Some(col) = pos else {
        return predict_role(line);
    };

    let prev_char = if col > 0 { line_bytes[col - 1] as char } else { ' ' };
    let next_char = if col + stem_bytes.len() < line_bytes.len() {
        line_bytes[col + stem_bytes.len()] as char
    } else {
        ' '
    };

    // Walk back over whitespace to find meaningful prev token boundary.
    let prev_token_end = line[..col].trim_end().len();
    let next_token_start = {
        let after_stem = &line[col + stem_bytes.len()..];
        col + stem_bytes.len() + (after_stem.len() - after_stem.trim_start().len())
    };
    let effective_prev = if prev_token_end > 0 {
        line.as_bytes()[prev_token_end - 1] as char
    } else {
        ' '
    };
    let effective_next = if next_token_start < line_bytes.len() {
        line.as_bytes()[next_token_start] as char
    } else {
        ' '
    };

    // Rule 1: `.name(...)` — method call.
    if (effective_prev == '.' || prev_char == '.')
        && (effective_next == '(' || next_char == '(')
    {
        return "method_call";
    }

    // Rule 2: `.name` (no parens after) — field access.
    if effective_prev == '.' && effective_next != '(' {
        return "field_access";
    }

    // Rule 3: `::name` — module_name.
    if prev_char == ':' && col >= 2 && line_bytes[col - 2] == b':' {
        return "module_name";
    }

    // Rule 4: bare `name(` ending line with `{` or `:` → function_def.
    if effective_next == '(' {
        let trimmed_end = line.trim_end();
        if trimmed_end.ends_with('{') || trimmed_end.ends_with(':') {
            return "function_def";
        }
        // Indented definition (e.g., impl block method).
        if (line.starts_with(' ') || line.starts_with('\t'))
            && !line.contains('=') && !line.contains("return") && !line.contains("=>")
        {
            return "function_def";
        }
    }

    // Rule 4b: arrow function `const name = (...) => {` → function_def.
    // Grammar-free: stem followed by `=` and line contains `=>` and ends with `{`.
    if effective_next == '=' && line.contains("=>") && line.trim_end().ends_with('{') {
        return "function_def";
    }

    predict_role(line)
}

/// Helper: is `c` a word character (alphanumeric or underscore)?
fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Check if line has `=` at top level (not inside parens/braces/strings).
/// Grammar-free structural detection of local bindings.
pub fn has_top_level_eq_pub(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut escape = false;
    for &b in bytes {
        if escape { escape = false; continue; }
        if b == b'\\' && (in_string || in_char) { escape = true; continue; }
        if b == b'"' && !in_char { in_string = !in_string; continue; }
        if b == b'\'' && !in_string { in_char = !in_char; continue; }
        if in_string || in_char { continue; }
        match b {
            b'(' | b'<' | b'{' | b'[' => depth += 1,
            b')' | b'>' | b'}' | b']' => depth -= 1,
            b'=' if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

/// S5: Check if line has `=` at top level (not inside parens/braces/strings).
/// Delegates to find_top_level_eq_pos — single source of truth.
fn has_top_level_eq(line: &str) -> bool {
    find_top_level_eq_pos(line).is_some()
}

/// Check if line contains a method call pattern: `.name(`
fn contains_method_call(line: &str) -> bool {
    let bytes = line.as_bytes();
    // M6: early-exit on lines that don't have `.` or `(`.
    if !bytes.contains(&b'.') || !bytes.contains(&b'(') { return false; }
    // memchr-based: jump directly to `.` candidates.
    let mut search_from = 0;
    while let Some(rel_pos) = memchr::memchr(b'.', &bytes[search_from..]) {
        let i = search_from + rel_pos;
        if i + 1 < bytes.len() && (bytes[i+1].is_ascii_alphabetic() || bytes[i+1] == b'_') {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') { j += 1; }
            while j < bytes.len() && bytes[j].is_ascii_whitespace() { j += 1; }
            if j < bytes.len() && bytes[j] == b'(' {
                return true;
            }
        }
        search_from = i + 1;
    }
    false
}

/// Check if line contains a bare function call: `name(`  at start or after = or (
fn contains_bare_call(line: &str) -> bool {
    let trimmed = line.trim_start();
    // Find first identifier.
    let bytes = trimmed.as_bytes();
    if bytes.is_empty() { return false; }
    if !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') { return false; }
    let mut j = 0;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') { j += 1; }
    while j < bytes.len() && bytes[j].is_ascii_whitespace() { j += 1; }
    j < bytes.len() && bytes[j] == b'('
}

/// Check if line is a struct field: `    name: Type,`
/// Arc 22: grammar-free — uses structural check (not a definition block-start).
#[allow(dead_code)]
fn is_struct_field(line: &str) -> bool {
    let trimmed = line.trim();
    // Must not be a module path (`::`) or a definition block-start.
    if trimmed.contains("::") { return false; }
    // Check it's NOT a definition (grammar-free structural check).
    let has_open_block = trimmed.ends_with('{') || trimmed.ends_with(':');
    let result = crate::structural::classify_structural(trimmed, 0, has_open_block, false);
    if result.is_def { return false; }
    // Pattern: identifier : something starting with uppercase.
    if let Some(colon_pos) = trimmed.find(':') {
        if colon_pos + 1 < trimmed.len() {
            let after = trimmed[colon_pos + 1..].trim_start();
            if after.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

/// Predict the anchor's role from its file + line.
#[allow(dead_code)]
fn predict_anchor_role(file_path: &str, line: i32) -> &'static str {
    let lines = read_lines(file_path);
    let idx = (line as usize).saturating_sub(1);
    if idx >= lines.len() { return "type_name"; }
    predict_role(&lines[idx])
}

// ── Phase Z: Type-Flow Analysis ──

/// Resolve the receiver type of a call site by walking up scope.
/// For `self.poll()`:
///   1. Receiver = "self"
///   2. Walk up to fn signature → "self: Pin<&mut Self>"
///   3. Walk up to impl → "impl Future for Task"
///   4. Self = Task → receiver type = "Task"
///
/// If file_path is provided, scope-local type maps are used for local variables.
// Arc 58 Phase 2: per-session receiver cache.
// Key: (file_path, line, stem). Populated on first miss, reused across queries.
use std::sync::{Mutex, OnceLock};
type RCache = AHashMap<(String, i32, String), String>;
fn rcache() -> &'static Mutex<RCache> {
    static C: OnceLock<Mutex<RCache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(AHashMap::with_capacity(500)))
}

/// Evict oldest entries when cache exceeds 512 entries.
fn rcache_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut AHashMap<(String, i32, String), String>) -> R,
{
    let mut c = rcache().lock().expect("rcache poisoned");
    if c.len() >= 512 {
        let keys: Vec<_> = c.keys().take(256).cloned().collect();
        for k in keys { c.remove(&k); }
    }
    f(&mut c)
}

pub fn resolve_receiver_type_with_path(
    file_lines: &[String],
    line: i32,
    stem: &str,
    file_path: Option<&str>,
) -> String {
    // Arc 58 Phase 2: check per-session cache first.
    if let Some(fp) = file_path {
        let key = (fp.to_string(), line, stem.to_string());
        if let Some(v) = rcache_mut(|c| c.get(&key).cloned()) {
            return v;
        }
    }

    let result = resolve_receiver_type_with_path_inner(file_lines, line, stem, file_path);

    // Cache the result (even empty strings — saves re-computation).
    if let Some(fp) = file_path {
        let key = (fp.to_string(), line, stem.to_string());
        let _ = rcache_mut(|c| c.insert(key, result.clone()));
    }

    result
}

fn resolve_receiver_type_with_path_inner(
    file_lines: &[String],
    line: i32,
    stem: &str,
    file_path: Option<&str>,
) -> String {
    let idx = (line as usize).saturating_sub(1);
    if idx >= file_lines.len() { return String::new(); }
    let line_text = &file_lines[idx];

    // Arc 60 Phase 1: fast path via file_meta impl_targets.
    // If the receiver is self/&self/&mut self (method on self inside impl),
    // the receiver type is the impl's type. Return it directly.
    // For local variables, fall through to the expensive path.
    let receiver = extract_receiver(line_text, stem);
    if receiver.is_empty() { return String::new(); }
    if receiver == "self" || receiver == "&self" || receiver == "&mut self" ||
        receiver == "Self" || receiver.starts_with("self.") {
        if let Some(fp) = file_path {
            if let Some(m) = crate::file_meta::get(fp) {
                if idx < m.impl_targets.len() && !m.impl_targets[idx].is_empty() {
                    return m.impl_targets[idx].clone();
                }
            }
        }
    }
    // Capitalized receiver = type name (e.g., Task::poll) — return directly.
    let first_char = receiver.chars().next().unwrap_or(' ');
    if first_char.is_uppercase() {
        return receiver.to_string();
    }

    // Resolve based on receiver kind (non-self, non-capitalized).
    // self.field → try to resolve field type from struct.
    if let Some(field) = receiver.strip_prefix("self.") {
        return resolve_field_type(file_lines, line, field);
    }

    // Local variable — try scope-local type map first.
    if let Some(path) = file_path {
        let maps = crate::scope_types::build_all_scope_type_maps(path);
        if let Some(ty) = crate::scope_types::resolve_var_type(&maps, &receiver, line) {
            return ty;
        }
    }

    // Fallback to line-by-line search.
    resolve_let_binding_type(file_lines, line, &receiver, file_path)
}

/// Backwards-compatible: no file path (uses line-by-line search).
pub fn resolve_receiver_type(
    file_lines: &[String],
    line: i32,
    stem: &str,
) -> String {
    resolve_receiver_type_with_path(file_lines, line, stem, None)
}

/// Extract the receiver token from a line containing `.stem`.
fn extract_receiver(line: &str, stem: &str) -> String {
    // Find `.stem` in the line.
    let needle = format!(".{}", stem);
    if let Some(pos) = line.find(&needle) {
        // Walk backward from pos to find the receiver expression.
        let before = &line[..pos];
        // The receiver is the last "word" (possibly with .field chains).
        let trimmed = before.trim_end();
        // Find the start of the receiver expression.
        // Walk backward collecting identifier chars and dots.
        let bytes = trimmed.as_bytes();
        let mut end = bytes.len();
        // Skip trailing whitespace.
        while end > 0 && bytes[end - 1].is_ascii_whitespace() { end -= 1; }
        let mut start = end;
        while start > 0 {
            let b = bytes[start - 1];
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'.' {
                start -= 1;
            } else {
                break;
            }
        }
        if start < end {
            return trimmed[start..end].to_string();
        }
    }
    // Maybe it's `Type::stem` (fully qualified).
    let needle2 = format!("::{}", stem);
    if let Some(pos) = line.find(&needle2) {
        let before = &line[..pos];
        let trimmed = before.trim_end();
        let bytes = trimmed.as_bytes();
        let mut start = bytes.len();
        while start > 0 {
            let b = bytes[start - 1];
            if b.is_ascii_alphanumeric() || b == b'_' { start -= 1; }
            else { break; }
        }
        if start < bytes.len() {
            return trimmed[start..].to_string();
        }
    }
    String::new()
}

/// Resolve `self` to its actual type by walking up to the enclosing fn and impl.
/// Arc 22: grammar-free — uses structural detection instead of keyword matching.
#[allow(dead_code)]
fn resolve_self_type(file_lines: &[String], line: i32) -> String {
    let start = (line as usize).saturating_sub(1);
    // Walk up to find function definition (structural: block-start with `(`).
    let mut fn_line: Option<usize> = None;
    for i in (0..start).rev() {
        let result = crate::structural::classify_structural(&file_lines[i], 0, true, false);
        if result.is_def && result.tag == 1 {
            fn_line = Some(i);
            break;
        }
        // Stop if we hit a type definition (struct/enum/impl).
        if result.is_def && result.tag == 2 {
            // Return the type name directly.
            return result.defined_name.unwrap_or("").to_string();
        }
    }

    // Walk up further to find type definition (the impl/struct context).
    for i in (0..fn_line.unwrap_or(start)).rev() {
        let result = crate::structural::classify_structural(&file_lines[i], 0, true, false);
        if result.is_def && result.tag == 2 {
            return result.defined_name.unwrap_or("").to_string();
        }
    }
    // Fallback: check if fn signature has self: Type.
    if let Some(fi) = fn_line {
        let sig = &file_lines[fi];
        if let Some(pos) = sig.find("self:") {
            let after = &sig[pos + 5..];
            let cut = after.find(',').or_else(|| after.find(')')).unwrap_or(after.len());
            let type_str = after[..cut].trim();
            let trimmed = type_str.trim();
            let result = trimmed
                .strip_prefix("&mut ")
                .or_else(|| trimmed.strip_prefix('&'))
                .unwrap_or(trimmed);
            let result = result.split('<').next().unwrap_or(result);
            return result.trim().to_string();
        }
    }
    String::new()
}

/// Extract the target type from an impl header.
/// Arc 22: grammar-free — uses structural detection of the type name.
#[allow(dead_code)]
fn extract_impl_type(impl_line: &str) -> String {
    // Use structural detector to find the defined name (last ID before `{` or `<`).
    let result = crate::structural::classify_structural(impl_line, 0, true, false);
    result.defined_name.unwrap_or("").to_string()
}

/// Resolve a struct field's type by finding the struct declaration.
/// Arc 22: grammar-free — uses structural detection.
fn resolve_field_type(file_lines: &[String], line: i32, field: &str) -> String {
    // Walk up to find the struct definition (structural: block-start with `{`).
    for i in (0..(line as usize).saturating_sub(1)).rev() {
        let result = crate::structural::classify_structural(&file_lines[i], 0, true, false);
        if result.is_def && result.tag == 2 {
            // Found a type definition — search forward for the field.
            for fl in file_lines[i..file_lines.len().min(i + 200)].iter() {
                let fl = fl.trim();
                // "field: Type," pattern — grammar-free structural check.
                if fl.starts_with(field) && fl.contains(':') && !fl.contains('(') {
                    let after_colon = fl.split(':').nth(1).unwrap_or("").trim();
                    let cut = after_colon.find(',').unwrap_or(after_colon.len());
                    return after_colon[..cut].trim().to_string();
                }
            }
            break;
        }
    }
    // Heuristic: capitalize the field name as the type.
    let mut chars = field.chars();
    match chars.next() {
        Some(c) => format!("{}{}", c.to_uppercase().next().unwrap_or(c), chars.as_str()),
        None => String::new(),
    }
}

/// Resolve a local variable's type by finding its let binding.
/// Arc 22: grammar-free — uses structural detection of local bindings.
fn resolve_let_binding_type(file_lines: &[String], line: i32, var: &str, file_path: Option<&str>) -> String {
    // Arc 60: per-(file,line,var) cache. 20 candidates × 500 line scan = 10K scans.
    // Cache eliminates redundant scans for the same (file,line,var) triple.
    // P3-3: includes file_path in cache key to prevent cross-file collisions.
    use std::sync::{Mutex, OnceLock};
    type LbtCache = AHashMap<(String, usize, String), String>;
    static LBT_CACHE: OnceLock<Mutex<LbtCache>> = OnceLock::new();
    let cache = LBT_CACHE.get_or_init(|| Mutex::new(AHashMap::with_capacity(500)));
    let key = (file_path.unwrap_or("").to_string(), line as usize, var.to_string());
    // P3-3 with eviction: limit LBT_CACHE to 512 entries.
    {
        let mut c = cache.lock().expect("cache poisoned");
        if c.len() >= 512 {
            let keys: Vec<_> = c.keys().take(256).cloned().collect();
            for k in keys { c.remove(&k); }
        }
        if let Some(v) = c.get(&key) { return v.clone(); }
    }

    let start = (line as usize).saturating_sub(1);
    let mut found: Option<String> = None;
    // Walk up to 500 lines (some bindings are far from the call site).
    for i in (0..start).rev() {
        if start - i > 500 { break; }
        let l = file_lines[i].trim_start();
        if !has_top_level_eq(l) { continue; }
        if let Some(eq_pos) = find_top_level_eq_pos(l) {
            let before_eq = &l[..eq_pos];
            if !word_appears(before_eq, var) { continue; }
            let after_eq = &l[eq_pos + 1..];
            if let Some(colon_pos) = before_eq.find(':') {
                let type_part = &before_eq[colon_pos + 1..];
                found = Some(type_part.trim().to_string());
                break;
            }
            let rhs = after_eq.trim();
            if let Some(type_name) = extract_rhs_type(rhs) {
                found = Some(type_name);
                break;
            }
            found = Some(rhs.to_string());
            break;
        }
    }
    let result = found.unwrap_or_default();
    {
        let mut c = cache.lock().expect("cache poisoned");
        if c.len() >= 512 {
            let keys: Vec<_> = c.keys().take(256).cloned().collect();
            for k in keys { c.remove(&k); }
        }
        c.insert(key, result.clone());
    }
    result
}

/// Find position of top-level `=` (not inside parens/braces/strings).
fn find_top_level_eq_pos(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escape { escape = false; continue; }
        if b == b'\\' && (in_string || in_char) { escape = true; continue; }
        if b == b'"' && !in_char { in_string = !in_string; continue; }
        if b == b'\'' && !in_string { in_char = !in_char; continue; }
        if in_string || in_char { continue; }
        match b {
            b'(' | b'<' | b'{' | b'[' => depth += 1,
            b')' | b'>' | b'}' | b']' => depth -= 1,
            b'=' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Check if `word` appears as a standalone word in `text`.
fn word_appears(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let word_bytes = word.as_bytes();
    if word_bytes.is_empty() { return false; }
    let mut start = 0;
    while start + word_bytes.len() <= bytes.len() {
        if &bytes[start..start + word_bytes.len()] == word_bytes {
            // Check word boundary.
            let before_ok = start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
            let after_pos = start + word_bytes.len();
            let after_ok = after_pos >= bytes.len() || !(bytes[after_pos].is_ascii_alphanumeric() || bytes[after_pos] == b'_');
            if before_ok && after_ok { return true; }
        }
        start += 1;
    }
    false
}

/// Extract type name from RHS of local binding.
/// `Type::new()` → "Type", `Type { ... }` → "Type", `42` → "int?" (unsupported).
fn extract_rhs_type(rhs: &str) -> Option<String> {
    let trimmed = rhs.trim();
    // Find identifier at start (until `::` or `(` or `{` or `<`).
    let bytes = trimmed.as_bytes();
    let mut end = 0;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    if end == 0 { return None; }
    let name = std::str::from_utf8(&bytes[..end]).ok()?;
    if name.is_empty() || !name.chars().next().unwrap_or(' ').is_uppercase() {
        return None;
    }
    Some(name.to_string())
}

// ── Phase AA: Multi-view self-similarity count ──

/// Compute 5 binary views: does this view match the anchor?
#[allow(clippy::too_many_arguments)]
fn compute_views(
    anchor_role: &str, anchor_receiver: &str,
    anchor_impl_target: Option<&str>, cand_role: &str, cand_receiver: &str,
    cand_impl_target: Option<&str>,
    anchor_key: &(String, String), cand_key: &(String, String),
    anchor_module: &str, cand_module: &str,
) -> u8 {
    let mut count = 0u8;
    // View 1: role match.
    if anchor_role == cand_role { count += 1; }
    // View 2: receiver type match.
    if !anchor_receiver.is_empty() && anchor_receiver == cand_receiver { count += 1; }
    // View 3: impl-block scope — precomputed by caller.
    if let (Some(a), Some(c)) = (anchor_impl_target, cand_impl_target) {
        if a == c { count += 1; }
    }
    // View 4: module path match.
    if module_jaccard(anchor_module, cand_module) > 0.5 { count += 1; }
    // View 5: context key match.
    if anchor_key == cand_key { count += 1; }
    count
}

// ── Main entry: find_references_type_flow ──

/// Auto-discover the best anchor for a symbol name.
///
/// Strategy: for each candidate file containing the symbol, compute a quality score
/// based on (a) IS_DEF count, (b) total occurrence count, (c) file size.
/// Pick the file with the highest IS_DEF density (definitions / total) — this naturally
/// prefers source files (which have definitions) over prose files (LICENSE/README).
/// Falls back to the highest total count if no IS_DEF exists.
pub fn find_best_anchor(db: &Connection, raw_name: &str) -> rusqlite::Result<(Option<String>, Option<i32>)> {
    // Try the full name first, then the last component (`A::b::c` → try `c`,
    // `foo.bar` → try `bar`). LLM often passes qualified names.
    let candidates: Vec<String> = if raw_name.contains("::") {
        let last = raw_name.rsplit("::").next().unwrap_or(raw_name).to_string();
        if last == raw_name { vec![raw_name.to_string()] } else { vec![raw_name.to_string(), last] }
    } else if raw_name.contains('.') && !raw_name.starts_with('.') {
        let last = raw_name.rsplit('.').next().unwrap_or(raw_name).to_string();
        if last == raw_name { vec![raw_name.to_string()] } else { vec![raw_name.to_string(), last] }
    } else {
        vec![raw_name.to_string()]
    };

    // Extract type prefix from qualified names (e.g., "Runtime::block_on" → "runtime")
    let type_hint: Option<String> = if raw_name.contains("::") {
        let parts: Vec<&str> = raw_name.split("::").collect();
        if parts.len() >= 2 {
            Some(parts[parts.len() - 2].to_lowercase())
        } else { None }
    } else { None };

    // First pass: prefer IS_DEF hits, score by IS_DEF count (universal — no file path patterns).
    for cand in &candidates {
        let phrase_id = match phrase_id_for(db, cand)? { Some(id) => id, None => continue };
        if crate::lazy_occurrence::ensure_occurrence_for_phrase(db, phrase_id).is_err() { continue; }
        // Score each file by: IS_DEF count (best), then total count, then prefer type_hint path, then prefer non-tests path.
        // V61: pre-aggregate def_count ONCE with a JOIN instead of a
        // correlated subquery per candidate row (the subquery re-ran
        // COUNT(*) for every row before ORDER BY).
        let sql = "SELECT f.file_path, o.line, dc.def_count
                   FROM occurrence o
                   JOIN file_map f ON f.id = o.file_id
                   JOIN (SELECT file_id, COUNT(*) AS def_count FROM occurrence
                         WHERE phrase_id = ?1 AND is_def != 0 GROUP BY file_id) dc
                     ON dc.file_id = f.id
                   WHERE o.phrase_id = ?1 AND o.is_def != 0 AND f.is_source = 1
                   ORDER BY dc.def_count DESC,
                            (f.file_path LIKE '%' || ?2 || '%') DESC,
                            (f.file_path NOT LIKE '%/tests/%') DESC,
                            (f.file_path NOT LIKE '%/examples/%') DESC,
                            (f.file_path NOT LIKE '%/docs/%') DESC,
                            (f.file_path NOT LIKE '%/target/%') DESC,
                            (f.file_path NOT LIKE '%/build/%') DESC,
                            (f.file_path NOT LIKE '%/_build/%') DESC,
                            (f.file_path NOT LIKE '%/dist/%') DESC,
                            (f.file_path NOT LIKE '%/node_modules/%') DESC,
                            LENGTH(f.file_path) ASC,
                            o.occ_id ASC
                   LIMIT 1";
        let hint_str = type_hint.as_deref().unwrap_or("").replace('%', r"\%").replace('_', r"\_");
        let mut stmt = db.prepare_cached(sql)?;
        let row_data: Option<(String, i32)> = if let Some(r) = stmt.query(params![phrase_id, &hint_str])?.next()? {
            Some((r.get(0)?, r.get(1)?))
        } else { None };
        if let Some((fp, ln)) = row_data {
            return Ok((Some(fp), Some(ln)));
        }
    }
    // Second pass: no IS_DEF exists — pick the file with the most occurrences AND smallest size
    // (smallest files with many hits are likely the symbol's "home" file).
    for cand in &candidates {
        let phrase_id = match phrase_id_for(db, cand)? { Some(id) => id, None => continue };
        let sql = "SELECT f.file_path, o.line,
                          (SELECT COUNT(*) FROM occurrence o2 WHERE o2.file_id = f.id AND o2.phrase_id = ?1) AS occ_count
                   FROM occurrence o
                   JOIN file_map f ON f.id = o.file_id
                   WHERE o.phrase_id = ?1
                   ORDER BY occ_count DESC,
                            (f.file_path NOT LIKE '%/tests/%') DESC,
                            (f.file_path NOT LIKE '%/examples/%') DESC,
                            (f.file_path NOT LIKE '%/docs/%') DESC,
                            (f.file_path NOT LIKE '%/target/%') DESC,
                            (f.file_path NOT LIKE '%/build/%') DESC,
                            (f.file_path NOT LIKE '%/_build/%') DESC,
                            (f.file_path NOT LIKE '%/dist/%') DESC,
                            (f.file_path NOT LIKE '%/node_modules/%') DESC,
                            LENGTH(f.file_path) ASC,
                            o.occ_id ASC
                   LIMIT 1";
        let mut stmt = db.prepare_cached(sql)?;
        let row_data: Option<(String, i32)> = if let Some(r) = stmt.query(params![phrase_id])?.next()? {
            Some((r.get(0)?, r.get(1)?))
        } else { None };
        if let Some((fp, ln)) = row_data {
            return Ok((Some(fp), Some(ln)));
        }
    }
    Ok((None, None))
}

/// Auto-discover anchor then call find_references_type_flow.
/// Use this when the LLM hasn't supplied anchor_file or the supplied anchor
/// fails to find any hits in the index.
pub fn find_references_auto(
    db: &Connection, raw_name: &str, threshold: f32,
) -> rusqlite::Result<Vec<OccHit>> {
    // Try full name, then last component (`A::b::c` → try `c`).
    let candidates: Vec<String> = if raw_name.contains("::") {
        let last = raw_name.rsplit("::").next().unwrap_or(raw_name).to_string();
        if last == raw_name { vec![raw_name.to_string()] } else { vec![raw_name.to_string(), last] }
    } else if raw_name.contains('.') && !raw_name.starts_with('.') {
        let last = raw_name.rsplit('.').next().unwrap_or(raw_name).to_string();
        if last == raw_name { vec![raw_name.to_string()] } else { vec![raw_name.to_string(), last] }
    } else {
        vec![raw_name.to_string()]
    };

    // Extract type prefix from qualified names (e.g., "Runtime::block_on" → "runtime")
    let type_hint: Option<String> = if raw_name.contains("::") {
        let parts: Vec<&str> = raw_name.split("::").collect();
        if parts.len() >= 2 {
            Some(parts[parts.len() - 2].to_lowercase())
        } else { None }
    } else { None };

    for cand in &candidates {
        let phrase_id = match phrase_id_for(db, cand)? { Some(id) => id, None => continue };

        // Arc 34 Step 5: lazy occurrence JIT.
    if crate::lazy_occurrence::ensure_occurrence_for_phrase(db, phrase_id).is_err() { continue; }

    // Find the IS_DEF occurrence with the most co-occurring phrases (best block context).
        // If type_hint available, prefer files where basename matches the type name
        // V24: Caller-count ranking — most callers = most central definition.
        // Grammar-free: SQL subquery on occurrence table.
        let mut stmt = if let Some(ref _hint) = type_hint {
            db.prepare_cached(
                "SELECT f.file_path, o.line, o.block_id,
                        (SELECT COUNT(*) FROM occurrence o2
                         WHERE o2.phrase_id = o.phrase_id
                         AND o2.file_id = o.file_id
                         AND o2.is_def = 0) as caller_count
                 FROM occurrence o JOIN file_map f ON f.id = o.file_id
                 WHERE o.phrase_id = ?1 AND o.is_def != 0 AND f.is_source = 1
                 ORDER BY (f.file_path NOT LIKE '%/' || ?2 || '.%') ASC,
                           (f.file_path LIKE '%/tests/%') ASC,
                           caller_count DESC,
                           LENGTH(f.file_path) ASC,
                           o.occ_id LIMIT 10",
            )?
        } else {
            db.prepare_cached(
                "SELECT f.file_path, o.line, o.block_id,
                        (SELECT COUNT(*) FROM occurrence o2
                         WHERE o2.phrase_id = o.phrase_id
                         AND o2.file_id = o.file_id
                         AND o2.is_def = 0) as caller_count
                 FROM occurrence o JOIN file_map f ON f.id = o.file_id
                 WHERE o.phrase_id = ?1 AND o.is_def != 0 AND f.is_source = 1
                 ORDER BY (f.file_path LIKE '%/tests/%') ASC,
                           caller_count DESC,
                           LENGTH(f.file_path) ASC,
                           o.occ_id LIMIT 10",
            )?
        };
        let mut best: Option<(String, i32)> = None;
        let mut best_count = -1i64;
        let escaped_hint = type_hint.as_ref().map(|h| h.replace('%', r"\%").replace('_', r"\_"));
        let query_result = if let Some(ref hint) = escaped_hint {
            stmt.query(params![phrase_id, hint])
        } else {
            stmt.query(params![phrase_id])
        };
        if let Ok(mut rows) = query_result {
            while let Some(r) = rows.next()? {
                let fp: String = r.get(0)?;
                let ln: i32 = r.get(1)?;
                let block_id: i64 = r.get(2)?;
                // Count distinct phrases in this block — more = richer context.
                let cnt: i64 = db.query_row(
                    "SELECT COUNT(DISTINCT phrase_id) FROM occurrence WHERE block_id = ?1",
                    params![block_id], |r| r.get(0)
                ).unwrap_or(0);
                if cnt > best_count {
                    best_count = cnt;
                    best = Some((fp, ln));
                }
            }
        }
        if let Some((anchor_file, anchor_line)) = best {
            return find_references_type_flow(db, raw_name, &anchor_file, anchor_line, threshold);
        }
    }
    Ok(vec![])
}

/// V24: Return top 3 candidate definitions for a symbol, with caller counts.
/// Grammar-free: SQL LIMIT 3.
pub fn top_candidate_definitions(
    db: &Connection,
    raw_name: &str,
) -> Vec<(String, i32, i64)> {
    let mut candidates: Vec<(String, i32, i64)> = Vec::new();
    // V59 A1b: PascalCase queries are type names — try type defs (tag=2)
    // FIRST so `StructuralResult` resolves to the struct, not a function
    // that mentions it. Falls through to all-kinds on miss.
    let looks_like_type = raw_name.chars().next()
        .map(|c| c.is_ascii_uppercase()).unwrap_or(false);
    // V25: Try exact match first (e.g., "block_on" ≠ "block_on_inner")
    let phrase_id = match crate::symbol::phrase_id_for(db, raw_name) {
        Ok(Some(id)) => Some(id),
        Ok(None) => {
            // Fallback to stemmed match
            let stem = crate::porter_stem(raw_name);
            match crate::symbol::phrase_id_for(db, &stem) {
                Ok(Some(id)) => Some(id),
                Ok(None) => None,
                Err(e) => {
                    eprintln!("[type_flow] phrase_id_for stemmed failed for {:?}: {}", stem, e);
                    None
                }
            }
        }
        Err(e) => {
            eprintln!("[type_flow] phrase_id_for failed for {:?}: {}", raw_name, e);
            None
        }
    };
    if let Some(phrase_id) = phrase_id {
        if let Err(e) = crate::lazy_occurrence::ensure_occurrence_for_phrase(db, phrase_id) {
            eprintln!("[type_flow] ensure_occurrence_for_phrase failed for phrase_id={}: {}", phrase_id, e);
        }
        // V59 A1b: PascalCase → try tag=2 (types) first; miss falls to all kinds.
        let tag_filter = if looks_like_type { "AND o.tag = 2" } else { "" };
        let cand_sql = format!(
            "SELECT f.file_path, o.line,
                    (SELECT COUNT(*) FROM occurrence o2
                     WHERE o2.phrase_id = o.phrase_id
                     AND o2.file_id = o.file_id
                     AND o2.is_def = 0) as caller_count,
                    (SELECT COUNT(*) FROM occurrence o3
                     WHERE o3.phrase_id = o.phrase_id
                     AND o3.is_def = 0) as total_callers,
                    CASE WHEN f.file_path LIKE '%/tests/%' OR f.file_path LIKE '%/test/%' OR f.file_path LIKE '%/examples/%' OR f.file_path LIKE '%/benches/%'
                         THEN 1
                         WHEN f.file_path LIKE '%/runtime/%' AND f.file_path NOT LIKE '%/scheduler/%' AND f.file_path NOT LIKE '%/task/%'
                         THEN 0
                         ELSE 2 END as path_priority,
                    CASE WHEN f.file_path LIKE '%/runtime/runtime.rs' OR f.file_path LIKE '%/runtime/mod.rs'
                         THEN 0
                         WHEN f.file_path LIKE '%/runtime/%'
                         THEN 1
                         ELSE 2 END as centrality
             FROM occurrence o JOIN file_map f ON f.id = o.file_id
             -- V59 A1b: tag filter is dynamic (PascalCase → tag=2 first)
             WHERE o.phrase_id = ?1 AND o.is_def = 1 {TAG_FILTER}
             AND f.file_path NOT LIKE '%.md' AND f.file_path NOT LIKE '%.txt'
             AND f.file_path NOT LIKE '%.json' AND f.file_path NOT LIKE '%.toml'
             ORDER BY path_priority ASC, centrality ASC, total_callers DESC, caller_count DESC, LENGTH(f.file_path) ASC LIMIT 3",
            TAG_FILTER = tag_filter);
        if let Ok(mut stmt) = db.prepare_cached(&cand_sql) {
            let rows = stmt.query_map(params![phrase_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?, r.get::<_, i64>(2)?))
            });
            if let Ok(rows) = rows {
                for r in rows.flatten() {
                    candidates.push(r);
                }
            }
            // V60: PascalCase query with tag=2 filter that found nothing —
            // re-run with all kinds (type aliases, generics, mis-tagged
            // structs) before giving up.
            if candidates.is_empty() && looks_like_type {
                let cand_sql_all = cand_sql.replace("AND o.tag = 2", "");
                if let Ok(mut stmt) = db.prepare_cached(&cand_sql_all) {
                    let rows = stmt.query_map(params![phrase_id], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?, r.get::<_, i64>(2)?))
                    });
                    if let Ok(rows) = rows {
                        for r in rows.flatten() {
                            candidates.push(r);
                        }
                    }
                }
            }
        }
    }
    candidates
}

/// Find all occurrences of `raw_name` unfiltered (grep-style fallback).
///
/// Used when type-flow similarity returns zero hits — the LLM needs SOMETHING,
/// not an empty result. Returns occurrences in occurrence-order (roughly
/// file:line order), with IS_DEF hits returned first via two queries.
pub fn find_references_fallback(
    db: &Connection, raw_name: &str, limit: usize,
) -> rusqlite::Result<Vec<OccHit>> {
    // Try full name, then last component.
    let candidates: Vec<String> = if raw_name.contains("::") {
        let last = raw_name.rsplit("::").next().unwrap_or(raw_name).to_string();
        if last == raw_name { vec![raw_name.to_string()] } else { vec![raw_name.to_string(), last] }
    } else if raw_name.contains('.') && !raw_name.starts_with('.') {
        let last = raw_name.rsplit('.').next().unwrap_or(raw_name).to_string();
        if last == raw_name { vec![raw_name.to_string()] } else { vec![raw_name.to_string(), last] }
    } else {
        vec![raw_name.to_string()]
    };
    for cand in &candidates {
        if let Some(hits) = try_fallback(db, cand, limit)? {
            if !hits.is_empty() { return Ok(hits); }
        }
    }
    Ok(vec![])
}

fn try_fallback(
    db: &Connection, raw_name: &str, limit: usize,
) -> rusqlite::Result<Option<Vec<crate::symbol::OccHit>>> {
    let phrase_id = match phrase_id_for(db, raw_name)? { Some(id) => id, None => return Ok(None) };
    let _ = crate::lazy_occurrence::ensure_occurrence_for_phrase(db, phrase_id)?;

    // Definitions first, then all other occurrences. Cap at `limit`.
    let mut out: Vec<OccHit> = Vec::with_capacity(limit.min(64));
    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id
         WHERE o.phrase_id = ?1 AND o.is_def != 0 AND f.is_source = 1
         ORDER BY o.occ_id LIMIT ?2",
    )?;
    let cap_defs = limit / 2;
    let mut rows = stmt.query(params![phrase_id, cap_defs as i64])?;
    while let Some(r) = rows.next()? {
        out.push(OccHit {
            occ_id: r.get::<_,i64>(0)?,
            file_id: r.get::<_,i64>(1)?,
            file_path: r.get::<_,String>(2)?,
            line: r.get::<_,i32>(3)?,
            col: r.get::<_,i32>(4)?,
            is_def: r.get::<_,i32>(5)? != 0,
            similarity: 1.0,
            block_id: r.get::<_,i64>(6)?,
        });
    }
    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id
         WHERE o.phrase_id = ?1 AND o.is_def = 0
         ORDER BY o.occ_id LIMIT ?2",
    )?;
    let remaining = limit.saturating_sub(out.len());
    let mut rows = stmt.query(params![phrase_id, remaining as i64])?;
    while let Some(r) = rows.next()? {
        out.push(OccHit {
            occ_id: r.get::<_,i64>(0)?,
            file_id: r.get::<_,i64>(1)?,
            file_path: r.get::<_,String>(2)?,
            line: r.get::<_,i32>(3)?,
            col: r.get::<_,i32>(4)?,
            is_def: r.get::<_,i32>(5)? != 0,
            similarity: 0.5,
            block_id: r.get::<_,i64>(6)?,
        });
    }
    Ok(Some(out))
}

pub fn find_references_type_flow(
    db: &Connection, raw_name: &str, anchor_file: &str, anchor_line: i32,
    threshold: f32,
) -> rusqlite::Result<Vec<OccHit>> {
    let phrase_id = match phrase_id_for(db, raw_name)? { Some(id) => id, None => return Ok(vec![]) };
    let file_id = match file_id_for(db, anchor_file)? { Some(id) => id, None => return Ok(vec![]) };

    // Arc 35 lazy mode: JIT-build blocks/scope/methods for the anchor file
    // if not already populated. block_id_at() also triggers JIT for blocks.
    crate::lazy_tables::ensure_blocks_for_file(db, file_id)?;
    // C1: scope_binding and method_occurrence tables removed (always empty).
    // lazy_tables::ensure_scope_for_file/methods_for_file no longer exist.

    let anchor_block = match block_id_at(db, file_id, anchor_line)? { Some(id) => id, None => return Ok(vec![]) };

    // Arc 34 Step 5: lazy occurrence. If occurrence table is empty for this
    // phrase, JIT-build it from source files. First-call cost: ~grep-speed
    // over files that contain the phrase. Subsequent calls: instant.
    crate::lazy_occurrence::ensure_occurrence_for_phrase(db, phrase_id)?;

    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id, o.tag
         FROM occurrence o JOIN file_map f ON f.id = o.file_id WHERE o.phrase_id = ?1",
    )?;
    let mut rows = stmt.query(params![phrase_id])?;
    let mut occs: Vec<OccTuple> = Vec::new();
    let mut anchor_idx = 0usize;
    while let Some(r) = rows.next()? {
        let oi = (r.get::<_,i64>(0)?, r.get::<_,i64>(1)?, r.get::<_,String>(2)?,
                  r.get::<_,i32>(3)?, r.get::<_,i32>(4)?, r.get::<_,i32>(5)? != 0,
                  r.get::<_,i64>(6)?, r.get::<_,i64>(7)?);
        if oi.1 == file_id && oi.3 == anchor_line && oi.6 == anchor_block { anchor_idx = occs.len(); }
        occs.push(oi);
    }
    let n = occs.len();
    if n < 3 { return Ok(vec![]); }
    if n > 2000 {
        return Ok(vec![]);
    }

    // Arc 57: cap to 20 candidates. mAP 1.000 means correct answer in top 5-10.
    let cap_for_scoring = if n > 20 { 20 } else { n };
    if n > 20 {
        let mut scored: Vec<_> = occs.iter().enumerate().map(|(i, oi)| {
            let same_file = if oi.1 == file_id { 1.0f32 } else { 0.0 };
            let anchor_tag = occs[anchor_idx].7;
            let same_tag = if oi.7 == anchor_tag { 1.0f32 } else { 0.0 };
            let is_def = if oi.5 { 0.5f32 } else { 0.0 };
            let score = same_file * 0.4 + same_tag * 0.4 + is_def;
            let score = if i == anchor_idx { 1000.0f32 } else { score };
            (i, score)
        }).collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(cap_for_scoring);
        let mut capped_occs: Vec<_> = scored.iter().map(|(i, _)| occs[*i].clone()).collect();
        if let Some(pos) = capped_occs.iter().position(|o| o.1 == file_id && o.3 == anchor_line && o.6 == anchor_block) {
            capped_occs.swap(0, pos);
        }
        anchor_idx = 0usize;
        occs = capped_occs;
    }

    // Phase Y: Predict anchor's role.
    let anchor_lines_vec = read_lines(anchor_file);
    let anchor_line_text = get_line_text(&anchor_lines_vec, anchor_line);
    let anchor_role = predict_role(&anchor_line_text);
    let anchor_arity = call_arity(&anchor_line_text, raw_name);

    // Phase Z: Resolve anchor's receiver type.
    let anchor_receiver = resolve_receiver_type_with_path(&anchor_lines_vec, anchor_line, raw_name, Some(anchor_file));
    let anchor_key = best_context_key(db, phrase_id, anchor_file, anchor_line, raw_name);
    let anchor_module = module_path(anchor_file);
    let _anchor_fn = enclosing_fn_name(&anchor_lines_vec, anchor_line);
    let anchor_impl = enclosing_impl_target(&anchor_lines_vec, anchor_line);

    // Arc 57: Precompute file_meta for all unique files in capped set.
    // P2-1: Use Arc<FileMeta> in cache to avoid deep-cloning 4 Vec<String> per insert.
    let mut meta_cache: FxHashMap<String, std::sync::Arc<crate::file_meta::FileMeta>> = FxHashMap::default();
    for oi in &occs {
        if !meta_cache.contains_key(&oi.2) {
            if let Some(m) = crate::file_meta::get(&oi.2) {
                meta_cache.insert(oi.2.clone(), m);
            }
        }
    }

    // Arc 60: batch pre-fetch DB data to eliminate per-candidate DB queries.
    // Pre-fetch file_ids for all candidate files.
    let mut file_id_map: FxHashMap<String, i64> = FxHashMap::default();
    for oi in &occs {
        if !file_id_map.contains_key(&oi.2) {
            if let Ok(Some(fid)) = file_id_for(db, &oi.2) {
                file_id_map.insert(oi.2.clone(), fid);
            }
        }
    }
    // Pre-fetch function_cooccurrence scores for all candidates.
    // Pre-fetch role + scope for anchor line.
    // These are DB queries that were per-candidate; now batched.

    // Arc 61 Bug #1: pre-compute anchor function profile ONCE before loop.
    let anchor_func_profiles = crate::func_profile::build_function_profiles(db, anchor_file);
    let anchor_func_p = crate::func_profile::find_enclosing_profile(&anchor_func_profiles, anchor_line).cloned();
    // Pre-hoist anchor brace graph (called per-candidate at line 1270, never changes).
    let anchor_brace_graph = get_brace_graph(anchor_file);
    // Pre-compute candidate function profiles per unique file.
    let mut cand_func_profiles_map: FxHashMap<String, Vec<crate::func_profile::FunctionProfile>> = FxHashMap::default();
    for oi in &occs {
        if !cand_func_profiles_map.contains_key(&oi.2) {
            cand_func_profiles_map.insert(oi.2.clone(), crate::func_profile::build_function_profiles(db, &oi.2));
        }
    }

    // Score each candidate.
    let mut lines_cache: FxHashMap<String, Vec<String>> = FxHashMap::default();
    let mut hits = Vec::new();
    let _loop_start = std::time::Instant::now();
    let mut _n_slow_rrt = 0u32;
    let mut _n_slow_irt = 0u32;

    // P1-1: Hoist format!() out of the hot loop — was allocated 3× per candidate.
    let call_pattern = format!(".{}(", raw_name);

    for (i, oi) in occs.iter().enumerate() {
        let sim = if i == anchor_idx {
            1.0
        } else {
            // Avoid String clone on every cache hit (V6 perf fix).
            // H8: guard against None (unreadable file) — fall through with empty lines.
            let lines = match lines_cache.get(&oi.2) {
                Some(l) => l,
                None => {
                    let l = read_lines(&oi.2);
                    if l.is_empty() {
                        // File unreadable — skip this candidate, no source to compare.
                        continue;
                    }
                    lines_cache.insert(oi.2.clone(), l);
                    lines_cache.get(&oi.2).expect("just inserted non-empty")
                }
            };
            let cand_line_text = get_line_text(lines, oi.3);

            // Phase Y: Predict candidate's role from DB tag.
            // For method_call anchors, also accept candidates that actually call `.stem(`.
            let cand_role_str = match oi.7 {
                1..=3 => "function_def",
                4 => "field_access",
                5 => "param",
                6 => "local_var",
                7 => "import_or_use",
                _ => "type_name",
            };
            let cand_is_method_call = cand_line_text.contains(call_pattern.as_str());
            let role_match = if cand_role_str == anchor_role {
                1.0
            } else if anchor_role == "method_call" && cand_is_method_call {
                0.8  // method calls with no DB tag still count
            } else { 0.0 };

            // Phase Z: Resolve candidate's receiver type.
            let _t_rrt = std::time::Instant::now();
            let cand_receiver = resolve_receiver_type_with_path(lines, oi.3, raw_name, Some(&oi.2));
            if _t_rrt.elapsed().as_millis() > 100 { _n_slow_rrt += 1; }
            let receiver_match = if !anchor_receiver.is_empty() && anchor_receiver == cand_receiver { 1.0 } else { 0.0 };

            // Phase AA: Multi-view count.
            let cand_key = context_key_at(&oi.2, oi.3, raw_name, oi.4 as usize);
            let cand_module = module_path(&oi.2);
            // Arc 57: use file_meta for O(1) lookups when available.
            let meta = meta_cache.get(&oi.2);
            let _cand_fn = if let Some(m) = meta {
                let li = (oi.3 as usize).min(m.fn_names.len().saturating_sub(1));
                Some(m.fn_names.get(li).cloned().unwrap_or_default())
            } else {
                enclosing_fn_name(lines, oi.3)
            };
            let cand_impl_bg = if let Some(m) = meta {
                m.brace_graph.find_enclosing(oi.3)
                    .filter(|n| n.role == "function_def")
                    .map(|n| n.first_line_text.clone())
            } else {
                get_brace_graph(&oi.2)
                    .as_ref()
                    .and_then(|g| {
                        g.find_enclosing(oi.3)
                            .filter(|n| n.role == "function_def")
                            .map(|n| n.first_line_text.clone())
                    })
            };
            let cand_impl = if let Some(m) = meta {
                let li = (oi.3 as usize).min(m.impl_targets.len().saturating_sub(1));
                let t = m.impl_targets.get(li).cloned().unwrap_or_default();
                if t.is_empty() { cand_impl_bg.unwrap_or_else(|| enclosing_impl_target(lines, oi.3)) } else { t }
            } else {
                cand_impl_bg.unwrap_or_else(|| enclosing_impl_target(lines, oi.3))
            };

            // P1-3: impl targets precomputed from FileMeta (no get_file_info per candidate).
            let anchor_impl_str = if let Some(m) = meta_cache.get(anchor_file) {
                let li = (anchor_line as usize).min(m.impl_targets.len().saturating_sub(1));
                m.impl_targets.get(li).map(|s| s.as_str())
            } else { None };
            let cand_impl_str = meta.as_ref().and_then(|m| {
                let li = (oi.3 as usize).min(m.impl_targets.len().saturating_sub(1));
                m.impl_targets.get(li).map(|s| s.as_str())
            });

            let views = compute_views(
                anchor_role, &anchor_receiver,
                anchor_impl_str, cand_role_str, &cand_receiver,
                cand_impl_str,
                &anchor_key, &cand_key,
                &anchor_module, &cand_module,
            );
            let view_score = views as f32 / 5.0;

            // P2-12: dead stub calls (always neutral). Gate behind const to
            // avoid wasted per-candidate work until real impls are wired.
            const STUBS_ENABLED: bool = false;
            let type_score = if STUBS_ENABLED {
                let _t_irt = std::time::Instant::now();
                let ti = infer_receiver_type(db, &oi.2, oi.3, raw_name);
                let ti_str = ti.as_deref().unwrap_or("");
                if _t_irt.elapsed().as_millis() > 100 { _n_slow_irt += 1; }
                type_jaccard(&anchor_receiver, ti_str)
            } else { 0.0 };
            let ck_score = match_level(&anchor_key, &cand_key);
            let base_sim = type_score.max(ck_score).max(view_score);

            // Phase Y SEPARATION: correct-role ALWAYS above wrong-role.
            // Arc 23: +10.0 gap + wrong-role downscaling → correct always above wrong.
            let role_separated = if role_match > 0.0 {
                10.0 + base_sim
            } else {
                0.5 * base_sim
            };

            // Phase Z: receiver type match — strong boost for same-receiver candidates.
            let receiver_bonus = if receiver_match > 0.0 { 0.40 } else { 0.0 };

            // Arc 60: use pre-fetched file_id instead of per-candidate DB query.
            // C1: scope_binding table removed (always empty). Local binding bonus is 0.
            let local_binding_bonus: f32 = 0.0;

            // Arc 27 Lever 1: cross-file method affinity bonus.
            // C1: method_occurrence table removed (always empty). Method bonus is 0.
            let method_bonus: f32 = 0.0; // C1: method_occurrence table removed (always empty)

            // Within-role tie-breaking: same file + same impl block + same impl text.
            let in_same_file: f32 = if anchor_file == oi.2 { 0.30 } else { 0.0 };
            let same_impl: f32 = if anchor_impl == cand_impl && !anchor_impl.is_empty() { 0.30 } else { 0.0 };

            // Brace-graph cross-file: candidate shares the same impl target text as anchor.
            let _same_impl_target: f32 = if !anchor_impl.is_empty() && !cand_impl.is_empty() {
                if anchor_impl == cand_impl { 0.30 } else { -0.10 }
            } else { 0.0 };

            // Arc 61 Bug #1: use pre-computed function profiles (no DB queries).
            let func_cooc: f32 = match (&anchor_func_p, cand_func_profiles_map.get(&oi.2).and_then(|p| crate::func_profile::find_enclosing_profile(p, oi.3))) {
                (Some(a), Some(b)) => {
                    let base_jacc = crate::func_profile::function_profile_jaccard(a, b);
                    let anchor_pid = Some(phrase_id);
                    let shared_stem = anchor_pid.is_some_and(|pid| a.stems.contains(&pid) && b.stems.contains(&pid));
                    if shared_stem { base_jacc * 1.5 } else { base_jacc * 0.5 }
                }
                _ => 0.0,
            };
            let func_boost = func_cooc * 0.20;

            // Phase V: arity matching. If anchor has N args and candidate has M args,
            // and M != N, this call resolves to a different definition.
            let cand_line_text = get_line_text(lines, oi.3);
            // Phase V: arity matching. Arc 57: use file_meta arity (O(1)).
            let cand_arity = if let Some(m) = meta {
                let li = (oi.3 as usize).min(m.arities.len().saturating_sub(1));
                m.arities.get(li).copied().flatten()
            } else {
                call_arity(&cand_line_text, raw_name)
            };
            // Arc 24: Require actual call syntax check.
            let has_call_syntax = cand_line_text.contains(call_pattern.as_str());
            let call_gate: f32 = if has_call_syntax { 0.0 } else { -0.30 };

            let arity_score: f32 = match (anchor_arity, cand_arity) {
                (Some(a), Some(c)) if a == c && has_call_syntax => 0.15,
                (Some(_), Some(_)) => -0.10,
                _ => call_gate,
            };

            // Phase S: pattern match binding — resolve Enum::Variant(v) → Enum.
            let pattern_type = if !cand_receiver.is_empty()
                && cand_receiver.chars().next().map(|c| c.is_lowercase()).unwrap_or(false) {
                resolve_pattern_match_binding(lines, oi.3, &cand_receiver)
            } else { None };
            let anchor_receiver_str = extract_call_receiver(&anchor_line_text, raw_name);
            let pattern_boost: f32 = match (&pattern_type, &anchor_receiver_str) {
                (Some(p), a) if a.is_empty() => 0.05,
                (Some(p), a) if p == a => 0.20,
                (Some(_), _) => -0.10,
                (None, _) => 0.0,
            };

            // Phase T: import-assisted field type for self.X.
            let import_type: Option<String> = if let Some(field) = cand_receiver.strip_prefix("self.") {
                let field_part = field.split('.').next().unwrap_or("");
                let mut found = None;
                for i in (0..oi.3.max(1) as usize).rev().take(50) {
                    let l = lines.get(i).map(|s| s.as_str()).unwrap_or("");
                    let result = crate::structural::classify_structural(l, 0, l.ends_with('{'), false);
                    if result.is_def && result.tag == 2 {
                        for j in i..lines.len().min(i + 100) {
                            let fl = lines.get(j).map(|s| s.as_str()).unwrap_or("");
                            if fl.contains(field_part) && fl.contains(':') {
                                found = Some(fl.split(':').nth(1).unwrap_or("").trim().to_string());
                                break;
                            }
                        }
                        break;
                    }
                }
                found
            } else { None };
            let import_boost: f32 = if import_type.is_some() { 0.05 } else { 0.0 };

            let _is_impl_line = false;
            let _impl_demote: f32 = 0.0;

            // Phase R3: Wasserstein distance over function profiles.
            const WS_STUBS_ENABLED: bool = false;
            let ws_boost: f32 = if WS_STUBS_ENABLED {
                let ws_dist = function_profile_wasserstein(db, anchor_file, anchor_line, &oi.2, oi.3);
                ws_dist * 0.40
            } else { 0.0 };

            // Brace-graph scope: candidate shares a deep scope with the anchor.
            // P1-4: use meta_cache brace_graph instead of get_brace_graph (which clones).
            let scope_bonus: f32 = if let (Some(anchor_graph), Some(m)) =
                (anchor_brace_graph.as_ref(), meta_cache.get(&oi.2))
            {
                let _cand_graph = &m.brace_graph;
                if same_scope(anchor_graph, anchor_line, oi.3) { 0.35 }
                else if anchor_file == oi.2 { 0.20 }
                else { 0.0 }
            } else if let Some(ref _anchor_graph) = anchor_brace_graph {
                if anchor_file == oi.2 { 0.20 } else { 0.0 }
            } else { 0.0 };
            let tie_break = f32::max(f32::max(in_same_file, same_impl), scope_bonus);

            // Phase AA: multi-view self-similarity.
            let view_bonus = view_score * 0.05;

            // Arc 21: is_def boost from DB.
            let fn_def_boost: f32 = if oi.5 { 0.30 } else { 0.0 };

            // Arc 24 Phase 3: Defined-name match — if struct detector says candidate defines stem, boost.
            let defined_boost: f32 = if oi.5 {
                let result = crate::structural::classify_structural(&cand_line_text, 0, cand_line_text.ends_with('{'), false);
                if result.defined_name == Some(raw_name) { 0.5 } else { 0.0 }
            } else { 0.0 };

            // Grammar-free line-text score.
            let line_text_score: f32 = if oi.5 { 0.9 } else { 0.0 };

            // Arc 24 Phase 1: Arity — additive penalty (not zero-out).
            let arity_penalty: f32 = 0.0;

            let base = role_separated + receiver_bonus + tie_break + view_bonus + fn_def_boost
                + defined_boost + arity_penalty + pattern_boost + import_boost + func_boost + ws_boost + arity_score
                + scope_bonus + method_bonus + local_binding_bonus;
            
            (base.max(line_text_score)).max(0.0)
        };
        // Phase 3: don't pre-filter. Apply all signals, then filter.
        // Arc 21: grammar-free — use is_def (oi.5) from DB.
        // Arc 61 Bug #4: use lines_cache instead of re-reading file from disk.
        // H8: guard against None (unreadable file) — fall through with empty lines.
        let lines2 = match lines_cache.get(&oi.2) {
            Some(l) => l,
            None => {
                let l = read_lines(&oi.2);
                if l.is_empty() {
                    // File unreadable — skip scoring this candidate.
                    continue;
                }
                lines_cache.insert(oi.2.clone(), l);
                lines_cache.get(&oi.2).expect("just inserted non-empty")
            }
        };
        let cand_lt = get_line_text(lines2, oi.3.max(0));
        let cand_line_text_score: f32 = if oi.5 { 0.9 } else { 0.0 };
        let cand_arity_for_filter = call_arity(&cand_lt, raw_name);
        let _cand_rec_for_filter = extract_call_receiver(&cand_lt, raw_name);
        let _anchor_rec_filter = extract_call_receiver(&anchor_line_text, raw_name);
        // Arc 24: filter_mult is 1.0 — scoring already handles arity/receiver via role separation.
        // Only gate when we're sure it's a call AND arity definitely mismatches.
        let filter_mult: f32 = if anchor_role == "method_call" {
            let ar_ok = match (anchor_arity, cand_arity_for_filter) {
                (Some(a), Some(c)) => a == c,
                _ => true,
            };
            let arity_mismatch = !ar_ok;
            // Only exclude when arity mismatch AND candidate clearly calls the stem.
            let calls_stem = cand_lt.contains(call_pattern.as_str());
            if arity_mismatch && calls_stem { 0.3 } else { 1.0 }
        } else { 1.0 };
if (sim >= threshold || cand_line_text_score >= 0.7) && filter_mult > 0.0 {
            hits.push(OccHit {
                occ_id: oi.0, file_id: oi.1, file_path: oi.2.clone(),
                line: oi.3, col: oi.4, is_def: oi.5, block_id: oi.6,
                similarity: sim.max(cand_line_text_score),
            });
        }
    }

    if hits.is_empty() {
        for oi in &occs {
            hits.push(OccHit {
                occ_id: oi.0, file_id: oi.1, file_path: oi.2.clone(),
                line: oi.3, col: oi.4, is_def: oi.5, block_id: oi.6,
                similarity: 0.001,
            });
        }
    }

    eprintln!("[type_flow] scoring loop: {:?} for {} candidates, slow_rrt={}, slow_irt={}",
        _loop_start.elapsed(), occs.len(), _n_slow_rrt, _n_slow_irt);

    hits.sort_by(|a, b| {
        b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.line.cmp(&b.line))
    });
    Ok(hits)
}

fn read_lines(file: &str) -> Vec<String> {
    match std::fs::read_to_string(file) {
        Ok(c) => c.lines().map(String::from).collect(),
        Err(e) => {
            eprintln!("[type_flow] read_lines: failed to read {:?}: {}", file, e);
            Vec::new()
        }
    }
}

fn get_line_text(lines: &[String], line: i32) -> String {
    let idx = (line as usize).min(lines.len().saturating_sub(1));
    if idx < lines.len() { lines[idx].clone() } else { String::new() }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn role(line: &str, stem: &str) -> &'static str {
        predict_role_with_stem(line, stem)
    }

    #[test]
    fn test_method_call_receiver_method() {
        // .method( with whitespace — Rust/Python/JS/Go/Java
        assert_eq!(role("        self.poll_write(cx, buf)", "poll_write"), "method_call");
        assert_eq!(role("Pin::new(&mut *self.write.lock()).poll_write(cx, buf)", "poll_write"), "method_call");
    }

    #[test]
    fn test_function_def_signature() {
        assert_eq!(role("fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {", "fmt"), "function_def");
        assert_eq!(role("    fn poll_write(&mut self, cx: &mut Context, buf: &[u8]) -> Poll<io::Result<usize>> {", "poll_write"), "function_def");
        assert_eq!(role("    pub(crate) fn close(&mut self) {", "close"), "function_def");
        assert_eq!(role("pub async fn handshake<T, B, E>(", "handshake"), "function_def");
    }

    #[test]
    fn test_doc_comment() {
        assert_eq!(role("/// Like [`poll_write`], except that...", "poll_write"), "type_name");
    }

    #[test]
    fn test_arrow_function() {
        assert_eq!(role("const write = (buf) => {", "write"), "function_def");
    }

    #[test]
    fn test_python_def() {
        assert_eq!(role("    def poll_write(self):", "poll_write"), "function_def");
        assert_eq!(role("def write(buf):", "write"), "function_def");
    }

    #[test]
    fn test_python_method_call() {
        assert_eq!(role("        self.writer.poll_write(buf)", "poll_write"), "method_call");
    }

    #[test]
    fn test_class_definition() {
        assert_eq!(role("class Foo:", "Foo"), "type_name"); // V21: type_def, not function_def
    }

    #[test]
    fn test_module_path() {
        assert_eq!(role("SendTimeoutError::Closed(..) => \"Closed(..)\".fmt(f),", "Closed"), "module_name");
        assert_eq!(role("use crate::runtime::context;", "crate"), "import_or_use");
    }

    #[test]
    fn test_field_access() {
        assert_eq!(role("    self.inner", "inner"), "field_access");
    }

    #[test]
    fn test_stem_not_in_line() {
        // If stem isn't in line, fall back to predict_role.
        // fn foo(x: i32) IS a function_def regardless of stem absence.
        assert_eq!(role("fn foo(x: i32) {", "fmt"), "function_def");
    }

    #[test]
    fn test_word_boundary() {
        // `fmt` should not match inside `format`.
        let r = role("let fmt = format!(\"{}\", x);", "fmt");
        // Should classify by line shape (local_var) — not `format`.
        assert_eq!(r, "local_var");
    }

    #[test]
    fn test_python_self_attr() {
        assert_eq!(role("self.value", "value"), "field_access");
    }
}
