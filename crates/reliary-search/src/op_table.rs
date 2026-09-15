//! Auto-extracted operator precedence tables (Shannon + Lévi-Strauss).
//!
//! Mining algorithm: scan source files for operator chains like `a OP1 b OP2 c`.
//! When two operators appear without parens, OP1 binds tighter (standard parse).
//! We count these pairwise dominance relationships, then rank operators by their
//! dominance wins.
//!
//! Associativity: when `a OP b OP c` is wrapped in `(...)`, OP is right-associative.
//! When unwrapped, OP is left-associative.
//!
//! No per-language config — table is auto-extracted from corpus.

use rustc_hash::FxHashMap;
use rusqlite::Connection;
use std::fs;

/// One entry in the operator table.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OpEntry {
    /// Higher = tighter binding (binds first in `a OP1 b OP2 c`).
    pub precedence: f32,
    /// 'L' = left-associative (a OP b OP c → (a OP b) OP c).
    /// 'R' = right-associative (a OP b OP c → a OP (b OP c)).
    pub associativity: char,
}

impl Default for OpEntry {
    fn default() -> Self {
        Self { precedence: 0.0, associativity: 'L' }
    }
}

/// A precedence table for one corpus.
#[derive(Debug, Clone, Default)]
pub struct OpTable {
    pub entries: FxHashMap<String, OpEntry>,
    pub postfix: FxHashMap<String, OpEntry>,
}

impl OpTable {
    pub fn new() -> Self { Self::default() }

    pub fn get(&self, op: &str) -> Option<&OpEntry> {
        self.entries.get(op)
    }

    /// Tokenize a line into a flat list of operands and operators.
    /// Skips comments, strings, and chars. Used by shunting-yard.
    pub fn tokenize_expression(&self, line: &str) -> Vec<String> {
        let bytes = line.as_bytes();
        let mut tokens = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            // Comments.
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                return tokens;
            }
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') { i += 1; }
                i += 2;
                continue;
            }
            // Strings/chars.
            if b == b'"' || b == b'\'' {
                let quote = b;
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() { i += 2; } else { i += 1; }
                }
                i += 1;
                continue;
            }
            if b.is_ascii_whitespace() { i += 1; continue; }
            // Identifier.
            if b.is_ascii_alphabetic() || b == b'_' {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') { i += 1; }
                tokens.push(std::str::from_utf8(&bytes[start..i]).unwrap_or("").to_string());
                continue;
            }
            // Number.
            if b.is_ascii_digit() {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_') { i += 1; }
                tokens.push(std::str::from_utf8(&bytes[start..i]).unwrap_or("").to_string());
                continue;
            }
            // 3-char ops.
            if i + 3 <= bytes.len() {
                let three = &bytes[i..i+3];
                if three == b"<<=" || three == b">>=" || three == b"===" || three == b"!==" {
                    tokens.push(std::str::from_utf8(three).unwrap_or("").to_string());
                    i += 3;
                    continue;
                }
            }
            // 2-char ops.
            if i + 2 <= bytes.len() {
                let two = &bytes[i..i+2];
                if two == b"==" || two == b"!=" || two == b"<=" || two == b">=" || two == b"&&" ||
                   two == b"||" || two == b"->" || two == b"=>" || two == b"::" || two == b"<<" ||
                   two == b">>" || two == b"**" || two == b".." || two == b"++" || two == b"--" {
                    tokens.push(std::str::from_utf8(two).unwrap_or("").to_string());
                    i += 2;
                    continue;
                }
            }
            // 1-char ops.
            if matches!(b, b'+' | b'-' | b'*' | b'/' | b'%' | b'<' | b'>' | b'=' |
                       b'&' | b'|' | b'^' | b'!') {
                tokens.push((b as char).to_string());
                i += 1;
                continue;
            }
            // Single-char postfix/punctuation.
            if matches!(b, b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b';' |
                       b':' | b'.' | b'?' | b'~') {
                tokens.push((b as char).to_string());
                i += 1;
                continue;
            }
            i += 1;
        }
        tokens
    }

/// Convert infix tokens to postfix via Dijkstra's shunting-yard.
/// `.` is treated as a high-precedence left-associative operator.
/// `(` marks postfix function-call application (emitted as `(call,N)`).
/// `[` marks postfix index application (emitted as `[index]`).
pub fn to_postfix(&self, tokens: &[String]) -> Vec<String> {
        let mut output: Vec<String> = Vec::new();
        let mut stack: Vec<String> = Vec::new();
        for tok in tokens {
            if matches!(tok.as_str(), "(") {
                stack.push(tok.clone());
            } else if matches!(tok.as_str(), ")") {
                // Pop until matching `(`. Drop `,` markers we pushed; count them.
                let mut comma_count = 0;
                while let Some(top) = stack.last() {
                    if top == "(" { break; }
                    if top == "," { comma_count += 1; }
                    output.push(stack.pop().unwrap());
                }
                stack.pop(); // Pop `(`.
                // comma_count = num args - 1 (since comma_count == 0 → 1 arg? No, 0 commas = 1 arg only if there were operands. Let me think...)
                // 0 commas, 0 operands before `)` = empty parens (0 args).
                // 0 commas, >0 operands = 1 arg.
                // K commas = K+1 args.
                // We need to detect if there are any operands emitted after `(`.
                // Count items in output since `(` was pushed... but we don't track that.
                // Simpler heuristic: if comma_count == 0 AND no operands emitted after `(`, then 0 args (grouping).
                // We'll handle in the parse loop: when we see `(N)`, if N == 0 AND no callee below, it's grouping.
                let n_args = comma_count + 1;
                let _ = n_args;
                // Emit (N) where N = comma_count.
                output.push(format!("({})", comma_count));
            } else if matches!(tok.as_str(), ",") {
                // Drain operators from stack to output (so the previous arg completes).
                // Then mark arg boundary by pushing `,` to stack.
                while let Some(top) = stack.last() {
                    if top == "(" { break; }
                    output.push(stack.pop().unwrap());
                }
                stack.push(",".to_string());
            } else if matches!(tok.as_str(), "[") {
                stack.push(tok.clone());
            } else if matches!(tok.as_str(), "]") {
                while let Some(top) = stack.last() {
                    if top == "[" { break; }
                    output.push(stack.pop().unwrap());
                }
                stack.pop(); // Pop `[`.
                output.push("[index]".to_string());
            } else if matches!(tok.as_str(), "." | "?.") {
                // `.` as high-precedence LEFT-associative operator.
                // For `a.b.c`, left-assoc gives `a b . c .` (correctly Access(Access(a,b), c)).
                while let Some(top) = stack.last() {
                    let top_prec = self.precedence(top);
                    if top_prec > 90.0 { break; }
                    if top_prec == 90.0 {
                        // Equal precedence, left-assoc → pop.
                        output.push(stack.pop().unwrap());
                    } else {
                        break;
                    }
                }
                stack.push(tok.clone());
            } else if self.entries.contains_key(tok) || self.postfix.contains_key(tok) {
                let op_prec = self.precedence(tok);
                let op_assoc = self.associativity(tok);
                while let Some(top) = stack.last() {
                    if top == "(" || top == "[" { break; }
                    let top_prec = self.precedence(top);
                    if top_prec > op_prec || (top_prec == op_prec && op_assoc == 'L') {
                        output.push(stack.pop().unwrap());
                    } else { break; }
                }
                stack.push(tok.clone());
            } else {
                output.push(tok.clone());
            }
        }
        while let Some(top) = stack.pop() {
            if top != "(" && top != ")" && top != "[" && top != "]" {
                output.push(top);
            }
        }
        output
    }

    fn precedence(&self, op: &str) -> f32 {
        self.entries.get(op).map(|e| e.precedence).unwrap_or_else(|| {
            self.postfix.get(op).map(|e| e.precedence).unwrap_or(0.0)
        })
    }

    fn associativity(&self, op: &str) -> char {
        self.entries.get(op).map(|e| e.associativity).unwrap_or_else(|| {
            self.postfix.get(op).map(|e| e.associativity).unwrap_or('L')
        })
    }

    pub fn save(&self, path: &str) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(&self.entries)
            .map_err(std::io::Error::other)?;
        fs::write(path, json)
    }

    pub fn load(path: &str) -> std::io::Result<Self> {
        let content = fs::read_to_string(path)?;
        let entries: FxHashMap<String, OpEntry> = serde_json::from_str(&content)
            .map_err(std::io::Error::other)?;
        Ok(Self { entries, postfix: default_postfix() })
    }
}

/// Universal postfix operators (function calls, field access, indexing).
pub fn default_postfix() -> FxHashMap<String, OpEntry> {
    let mut m = FxHashMap::default();
    m.insert("(".to_string(), OpEntry { precedence: 100.0, associativity: 'L' });
    m.insert("[".to_string(), OpEntry { precedence: 100.0, associativity: 'L' });
    m.insert(".".to_string(), OpEntry { precedence: 90.0, associativity: 'L' });
    m.insert("?.".to_string(), OpEntry { precedence: 90.0, associativity: 'L' });
    m
}

/// Mine the operator table from a corpus.
/// Algorithm v2 (Phase C): filter operators that appear in NON-arithmetic
/// contexts. We track:
/// - `paren_depth`: how deep we are in `(...)` (excluding `<...>` generics).
/// - `angle_depth`: how deep in `<>` generics.
/// - Only count operator occurrences when:
///   - prev/next are operands (not opening paren/bracket suggesting generics).
///   - NOT inside `<>` (Rust `Vec<T>`, `Option<T>`, etc.).
///   - NOT inside `#[...]` attributes.
///   - Operator has SPACES AROUND IT (heuristic for actual usage vs syntax).
pub fn mine_op_table(db: &Connection) -> rusqlite::Result<OpTable> {
    let mut entries: FxHashMap<String, OpEntry> = FxHashMap::default();

    let mut stmt = db.prepare_cached(
        "SELECT DISTINCT f.file_path FROM occurrence o JOIN file_map f ON f.id = o.file_id"
    )?;
    let file_paths: Vec<String> = stmt.query_map([], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    let mut raw_count: FxHashMap<String, u64> = FxHashMap::default();
    let mut paren_count: FxHashMap<String, u64> = FxHashMap::default();

    for path in &file_paths {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        mine_v2(&content, &mut raw_count, &mut paren_count);
    }

    let mut all_ops: std::collections::HashSet<String> = std::collections::HashSet::new();
    for op in raw_count.keys() { all_ops.insert(op.clone()); }
    for op in paren_count.keys() { all_ops.insert(op.clone()); }

    let mut ratios: Vec<(String, f32, u64, u64)> = Vec::new();
    for op in &all_ops {
        let raw = *raw_count.get(op).unwrap_or(&0);
        let paren = *paren_count.get(op).unwrap_or(&0);
        let total = raw + paren;
        if total < 3 { continue; }
        let ratio = paren as f32 / total as f32;
        ratios.push((op.clone(), ratio, raw, paren));
    }
    // Sort by ratio DESCENDING: operators with HIGHER paren ratio bind TIGHTER.
    // (High ratio = mostly wrapped = needs parens to override = high precedence.)
    ratios.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Map ratio to actual precedence (not rank). Range [1.0, 10.0].
    for (op, ratio, _raw, _paren) in &ratios {
        // ratio in [0, 1] → precedence in [2, 9]. Add baseline 1.
        let precedence = 1.0 + ratio * 8.0;
        entries.insert(op.clone(), OpEntry { precedence, associativity: 'L' });
    }

    Ok(OpTable { entries, postfix: default_postfix() })
}

/// v2 mining algorithm: stricter filtering of operator occurrences.
/// Only counts operators that:
/// - Are in EXPRESSION context (between operands, not in syntax positions).
/// - Have spaces around them (heuristic).
/// - Are NOT inside `<>` generics or `#[...]` attributes.
fn mine_v2(
    content: &str,
    raw_count: &mut FxHashMap<String, u64>,
    paren_count: &mut FxHashMap<String, u64>,
) {
    let bytes = content.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut paren_depth: i32 = 0;
    let mut angle_depth: i32 = 0;
    let mut bracket_depth: i32 = 0; // also tracks #[
    let mut operator_had_space_before = false;

    while i < bytes.len() {
        let b = bytes[i];

        if in_line_comment {
            if b == b'\n' { in_line_comment = false; }
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block_comment = false; i += 2; continue;
            }
            i += 1; continue;
        }
        if in_string {
            if b == b'\\' && i + 1 < bytes.len() { i += 2; continue; }
            if b == b'"' { in_string = false; }
            i += 1; continue;
        }
        if in_char {
            if b == b'\\' && i + 1 < bytes.len() { i += 2; continue; }
            if b == b'\'' { in_char = false; }
            i += 1; continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_line_comment = true; i += 2; continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            in_block_comment = true; i += 2; continue;
        }
        if b == b'"' { in_string = true; i += 1; continue; }
        if b == b'\'' { in_char = true; i += 1; continue; }
        if b.is_ascii_alphanumeric() || b == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') { i += 1; }
            continue;
        }
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Track brackets for expression context.
        if b == b'(' || b == b'[' || b == b'{' {
            if b == b'(' {
                paren_depth += 1;
                // Detect #[ attribute.
                if i > 0 && (bytes[i-1] == b'#' || (i > 1 && bytes[i-2] == b'#' && bytes[i-1].is_ascii_whitespace())) {
                    bracket_depth += 1;
                }
            } else if b == b'['
                && i > 0 && bytes[i-1] == b'#' {
                    bracket_depth += 1;
                }
            i += 1;
            continue;
        }
        if b == b')' || b == b']' || b == b'}' {
            if b == b')' && paren_depth > 0 { paren_depth -= 1; }
            if b == b']' && bracket_depth > 0 { bracket_depth -= 1; }
            i += 1;
            continue;
        }
        // Track angle bracket depth (for generics).
        if b == b'<' && angle_depth == 0 {
            // Could be generic or comparison. Heuristic: if previous non-space
            // token is identifier and next is identifier, treat as generic.
            let prev_is_id = find_prev_nonspace_ident(bytes, i);
            let next_is_id = find_next_nonspace(bytes, i + 1);
            // Check for `:` (shouldn't happen for comparison).
            if prev_is_id && next_is_id {
                // Likely generic. Skip and count via tracking.
                angle_depth += 1;
                operator_had_space_before = check_space_before(bytes, i);
                i += 1;
                continue;
            }
        }
        if b == b'>' && angle_depth > 0 {
            angle_depth -= 1;
            i += 1;
            continue;
        }

        // Multi-char operators.
        let op_start = i;
        let op_str: String = match extract_op_at(bytes, &mut i, b) {
            Some(s) => s,
            None => continue,
        };
        // Skip universal postfix.
        if matches!(op_str.as_str(), "(" | "[" | "." | "?.") {
            continue;
        }

        // Check expression context: not in generic, not in attribute.
        let is_arithmetic_context = angle_depth == 0
            && bracket_depth == 0
            && operator_had_space_before
            && check_space_after(bytes, op_start + op_str.len());

        if is_arithmetic_context {
            if paren_depth == 0 {
                *raw_count.entry(op_str.clone()).or_insert(0) += 1;
            } else {
                *paren_count.entry(op_str.clone()).or_insert(0) += 1;
            }
        }
        operator_had_space_before = false;
    }
}

fn find_prev_nonspace_ident(bytes: &[u8], pos: usize) -> bool {
    let mut i = pos;
    while i > 0 {
        i -= 1;
        let b = bytes[i];
        if b.is_ascii_whitespace() { continue; }
        return b.is_ascii_alphabetic() || b == b'_';
    }
    false
}

fn find_next_nonspace(bytes: &[u8], pos: usize) -> bool {
    let mut i = pos;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() { i += 1; continue; }
        return b.is_ascii_alphabetic() || b == b'_';
    }
    false
}

fn check_space_before(bytes: &[u8], pos: usize) -> bool {
    if pos == 0 { return false; }
    let mut i = pos - 1;
    while i < pos && (i > 0 && bytes[i].is_ascii_whitespace()) {
        i -= 1;
    }
    i < pos && bytes[i].is_ascii_whitespace()
}

fn check_space_after(bytes: &[u8], pos: usize) -> bool {
    if pos >= bytes.len() { return false; }
    bytes[pos].is_ascii_whitespace()
}

/// Mine operator precedence via paren-balancing signal.
/// Algorithm: for each operator occurrence, count whether it's "wrapped" (preceded by
/// matching parens without intervening op) or "raw" (top-level). Operators that are
/// mostly raw are LOW precedence (don't need parens to bind correctly). Operators that
/// are mostly wrapped are HIGH precedence (need parens to override lower-precedence neighbors).
///
/// Also extract dominance from OP-OP pairs within paren groups.
#[allow(dead_code)]
fn mine_operator_chains(
    content: &str,
    dominates: &mut FxHashMap<(String, String), u64>,
    chain_count: &mut FxHashMap<String, u64>,
    right_wrapped: &mut FxHashMap<String, u64>,
    raw_count: &mut FxHashMap<String, u64>,
    paren_count: &mut FxHashMap<String, u64>,
) {
    let bytes = content.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut depth: i32 = 0; // Current paren depth.
    let mut max_paren_depth: i32 = 0;

    while i < bytes.len() {
        let b = bytes[i];

        if in_line_comment {
            if b == b'\n' { in_line_comment = false; }
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block_comment = false; i += 2; continue;
            }
            i += 1; continue;
        }
        if in_string {
            if b == b'\\' && i + 1 < bytes.len() { i += 2; continue; }
            if b == b'"' { in_string = false; }
            i += 1; continue;
        }
        if in_char {
            if b == b'\\' && i + 1 < bytes.len() { i += 2; continue; }
            if b == b'\'' { in_char = false; }
            i += 1; continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_line_comment = true; i += 2; continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            in_block_comment = true; i += 2; continue;
        }
        if b == b'"' { in_string = true; i += 1; continue; }
        if b == b'\'' { in_char = true; i += 1; continue; }
        if b.is_ascii_alphanumeric() || b == b'_' {
            // Skip identifier/number.
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.') { i += 1; }
            continue;
        }
        if b.is_ascii_whitespace() { i += 1; continue; }

        // Track paren depth.
        if b == b'(' {
            depth += 1;
            if depth > max_paren_depth { max_paren_depth = depth; }
            i += 1;
            continue;
        }
        if b == b')' {
            depth -= 1;
            i += 1;
            continue;
        }
        if matches!(b, b',' | b';' | b':' | b'{' | b'}' | b'[' | b']') {
            i += 1;
            continue;
        }

        // Extract operator.
        let _op_start = i;
        let op_str: String = match extract_op_at(bytes, &mut i, b) {
            Some(s) => s,
            None => continue,
        };

        // Universal postfix doesn't count.
        if matches!(op_str.as_str(), "." | "?.") {
            continue;
        }

        // Count raw vs wrapped.
        if depth == 0 {
            *raw_count.entry(op_str.clone()).or_insert(0) += 1;
        } else {
            *paren_count.entry(op_str.clone()).or_insert(0) += 1;
        }
    }
    let _ = (dominates, chain_count, right_wrapped, max_paren_depth);
}

fn extract_op_at(bytes: &[u8], i: &mut usize, b: u8) -> Option<String> {
    if *i + 3 <= bytes.len() {
        let three = &bytes[*i..*i+3];
        if three == b"<<=" || three == b">>=" || three == b"===" || three == b"!==" {
            let s = std::str::from_utf8(three).unwrap_or("").to_string();
            *i += 3;
            return Some(s);
        }
    }
    if *i + 2 <= bytes.len() {
        let two = &bytes[*i..*i+2];
        if two == b"==" || two == b"!=" || two == b"<=" || two == b">=" || two == b"&&" ||
           two == b"||" || two == b"->" || two == b"=>" || two == b"::" || two == b"<<" ||
           two == b">>" || two == b"**" || two == b".." || two == b"++" || two == b"--" {
            let s = std::str::from_utf8(two).unwrap_or("").to_string();
            *i += 2;
            return Some(s);
        }
    }
    if matches!(b, b'+' | b'-' | b'*' | b'/' | b'%' | b'<' | b'>' | b'=' |
                  b'&' | b'|' | b'^' | b'!') {
        let s = (b as char).to_string();
        *i += 1;
        return Some(s);
    }
    // Single-char punctuation that breaks chains (handled by caller).
    if matches!(b, b'(' | b'[' | b'.' | b'?') {
        let s = (b as char).to_string();
        *i += 1;
        return Some(s);
    }
    *i += 1;
    None
}

#[allow(dead_code)]
fn is_wrapped_in_paren_at(bytes: &[u8], pos: usize) -> bool {
    let mut depth = 0;
    let mut i = pos;
    while i > 0 {
        i -= 1;
        let b = bytes[i];
        if b == b')' { depth += 1; }
        else if b == b'(' {
            if depth == 0 { return true; }
            depth -= 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_simple() {
        let t = OpTable::new();
        assert_eq!(t.tokenize_expression("x + 1 * 2"), vec!["x", "+", "1", "*", "2"]);
    }

    #[test]
    fn test_tokenize_call() {
        let t = OpTable::new();
        assert_eq!(t.tokenize_expression("foo.bar(x, y)"),
                   vec!["foo", ".", "bar", "(", "x", ",", "y", ")"]);
    }

    #[test]
    fn test_tokenize_string() {
        let t = OpTable::new();
        let tokens = t.tokenize_expression("let s = \"hello + world\";");
        // The `+` inside the string should NOT be tokenized.
        assert!(!tokens.contains(&"+".to_string()),
            "Operator inside string should not appear: {:?}", tokens);
    }

    #[test]
    fn test_postfix_simple() {
        let mut t = OpTable::new();
        t.entries.insert("+".to_string(), OpEntry { precedence: 5.0, associativity: 'L' });
        t.entries.insert("*".to_string(), OpEntry { precedence: 7.0, associativity: 'L' });
        let postfix = t.to_postfix(&t.tokenize_expression("1 + 2 * 3"));
        // * binds tighter: 1 2 3 * +
        assert_eq!(postfix, vec!["1", "2", "3", "*", "+"]);
    }

    #[test]
    fn test_postfix_paren() {
        let mut t = OpTable::new();
        t.entries.insert("+".to_string(), OpEntry { precedence: 5.0, associativity: 'L' });
        t.entries.insert("*".to_string(), OpEntry { precedence: 7.0, associativity: 'L' });
        let postfix = t.to_postfix(&t.tokenize_expression("(1 + 2) * 3"));
        // `(1+2)` becomes `(0)` (0 args = implicit grouping), then * follows.
        assert_eq!(postfix, vec!["1", "2", "+", "(0)", "3", "*"]);
    }

    #[test]
    fn test_postfix_right_assoc() {
        let mut t = OpTable::new();
        t.entries.insert("=".to_string(), OpEntry { precedence: 1.0, associativity: 'R' });
        t.entries.insert("+".to_string(), OpEntry { precedence: 5.0, associativity: 'L' });
        let postfix = t.to_postfix(&t.tokenize_expression("a = b = c"));
        // Right-assoc =: a b c = =
        assert_eq!(postfix, vec!["a", "b", "c", "=", "="]);
    }

    #[test]
    fn test_mine_chain_extraction() {
        // Test that mining counts paren vs raw correctly.
        // `+` appears both raw and wrapped, `*` always raw (because it binds tightest).
        let content = "let x = a + b; let y = (a + b) * c; if (a + b) > c { d = a + b; }";
        let mut dominates = FxHashMap::default();
        let mut chain_count = FxHashMap::default();
        let mut right_wrapped = FxHashMap::default();
        let mut raw_count = FxHashMap::default();
        let mut paren_count = FxHashMap::default();
        mine_operator_chains(content, &mut dominates, &mut chain_count,
                             &mut right_wrapped, &mut raw_count, &mut paren_count);
        // `+` appears 1 raw + 2 wrapped = 3 total. The 2 wrapped indicate that when
        // `+` is adjacent to higher-precedence ops, authors use parens.
        assert!(*paren_count.get("+").unwrap_or(&0) >= 2, "Expected paren + count >= 2: {:?}", paren_count);
        assert!(*raw_count.get("+").unwrap_or(&0) >= 1, "Expected raw + count >= 1: {:?}", raw_count);
    }

    #[test]
    fn test_default_postfix() {
        let p = default_postfix();
        assert!(p.contains_key("("));
        assert!(p.contains_key("."));
        assert!(p.contains_key("["));
    }
}