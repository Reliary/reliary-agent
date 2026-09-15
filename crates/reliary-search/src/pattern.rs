//! Progressive Pattern Matching — Steps 1+2 of the context-key breakthrough.
//!
//! Two conceptual leaps:
//!
//! 1. COLUMN-AGNOSTIC CONTEXT: when `stem` appears multiple times on a line
//!    (e.g. `park.park()`), try ALL columns and pick the one with the most DB
//!    matches. This fixes the receiver-vs-method ambiguity.
//!
//! 2. PROGRESSIVE WILDCARDS: match (left, right) at 5 levels of specificity:
//!    Level 0: exact match          → 1.0  "same call site"
//!    Level 1: wildcard-right       → 0.8  "same receiver, any arg"
//!    Level 2: wildcard-left        → 0.7  "same first arg, any receiver"
//!    Level 3: both wildcard        → 0.5  "any call of this method"
//!    Level 4: empty-key fallback   → 0.2  "no distinguishing context"
//!    These levels are STRUCTURAL (0/1/2 wildcards), not tuned.

use crate::symbol::{OccHit, file_id_for, phrase_id_for};
use rusqlite::{params, Connection};
use rustc_hash::FxHashMap;

/// Punctuation/operator chars that separate tokens.
const PUNCT: &str = "(){}[]<>;,.:!?&|^+-*/%='\"`~@#";

/// Extract next stem right of `from` on `line`.
/// V59: floor to a valid UTF-8 char boundary (ASCII fast path).
#[inline]
fn floor_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// V59: ceil to a valid UTF-8 char boundary.
#[inline]
fn ceil_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return i;
    }
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn next_stem_right(line: &str, from: usize) -> String {
    let bytes = line.as_bytes();
    let mut i = from;
    while i < line.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() { i += 1; continue; }
        if PUNCT.contains(c) { i += 1; continue; }
        let start = i;
        while i < line.len() {
            let c = bytes[i] as char;
            if c.is_whitespace() || PUNCT.contains(c) { break; }
            i += 1;
        }
        let end = ceil_boundary(line, i);
        let st = floor_boundary(line, start);
        return line[st..end].to_string();
    }
    String::new()
}

/// Extract previous stem left of `to` on `line`.
fn prev_stem_left(line: &str, to: usize) -> String {
    let bytes = line.as_bytes();
    let mut i = to.min(line.len());
    while i > 0 {
        i -= 1;
        let c = bytes[i] as char;
        if c.is_whitespace() { continue; }
        if PUNCT.contains(c) { return String::new(); } // stop at punct
        let end = ceil_boundary(line, i + 1);
        while i > 0 {
            let c = bytes[i - 1] as char;
            if c.is_whitespace() || PUNCT.contains(c) { break; }
            i -= 1;
        }
        let st = floor_boundary(line, i);
        return line[st..end].to_string();
    }
    String::new()
}

/// Find ALL columns where `stem` occurs on `line`.
fn all_stem_cols(line: &str, stem: &str) -> Vec<usize> {
    let mut cols = Vec::new();
    let mut start = 0usize;
    while start < line.len() {
        start = ceil_boundary(line, start);
        let Some(pos) = line[start..].find(stem) else { break };
        cols.push(start + pos);
        start += pos + 1;
        if start >= line.len() { break; }
    }
    cols
}

/// Context key at a specific column on a specific line.
pub fn context_key_at(file: &str, line: i32, stem: &str, col: usize) -> (String, String) {
    // Arc 60 Phase 2: use file_meta cache instead of file read.
    let line_text = if let Some(m) = crate::file_meta::get(file) {
        let li = line as usize;
        if li < m.lines.len() { m.lines[li].clone() } else { return (String::new(), String::new()); }
    } else {
        let content = match std::fs::read_to_string(file) {
            Ok(c) => c,
            Err(_) => return (String::new(), String::new()),
        };
        match content.lines().nth(line as usize) {
            Some(l) => l.to_string(),
            None => return (String::new(), String::new()),
        }
    };
    if col >= line_text.len() {
        return (String::new(), String::new());
    }
    let left = prev_stem_left(&line_text, col);
    let right = next_stem_right(&line_text, col + stem.len());
    (left, right)
}

/// Resolve the BEST context key for an anchor: try ALL columns, pick the one
/// whose key has the most matches in the DB.
pub fn best_context_key(
    _db: &Connection, _phrase_id: i64, anchor_file: &str, anchor_line: i32, stem: &str,
) -> (String, String) {
    // Arc 60 Phase 2: use file_meta cache instead of file read.
    // V51: anchor_line from MCP is 1-indexed; file_lines is 0-indexed.
    let line_text = if let Some(m) = crate::file_meta::get(anchor_file) {
        let li = anchor_line.saturating_sub(1) as usize;
        if li < m.lines.len() { m.lines[li].clone() } else { return (String::new(), String::new()); }
    } else {
        let content = match std::fs::read_to_string(anchor_file) {
            Ok(c) => c,
            Err(_) => return (String::new(), String::new()),
        };
        match content.lines().nth(anchor_line.saturating_sub(1) as usize) {
            Some(l) => l.to_string(),
            None => return (String::new(), String::new()),
        }
    };

    let cols = all_stem_cols(&line_text, stem);
    if cols.is_empty() {
        return (String::new(), String::new());
    }
    if cols.len() == 1 {
        let col = cols[0];
        return context_key_at(anchor_file, anchor_line, stem, col);
    }

    // Multiple columns: try each, pick the one with the most distinctive
    // context key. C14: previously ran a useless COUNT query (always
    // identical per column) and returned the first column regardless.
    // Now: evaluate each column's context key (left, right), pick the
    // one whose key is most distinctive (longest non-empty concatenation).
    let mut best: (String, String) = (String::new(), String::new());
    let mut best_len = 0usize;
    for &col in &cols {
        let k = context_key_at(anchor_file, anchor_line, stem, col);
        let len = k.0.len() + k.1.len();
        if len > best_len {
            best = k;
            best_len = len;
            if len >= 32 {
                break;
            } // good enough — skip remaining columns
        }
    }
    if best_len > 0 {
        return best;
    }
    (String::new(), String::new())
}

/// Progressive pattern match level between anchor key and candidate key.
///
/// Returns a score in [0.0, 1.0]:
///   1.0 — exact match
///   0.8 — same right token, any left (wildcard-left)
///   0.7 — same left token, any right (wildcard-right)
///   0.5 — neither empty nor matching; both sides present (generic method_call match)
///   0.2 — anchor has no distinguishing context (both sides empty)
///   0.0 — no match via any pattern
pub fn match_level(anchor: &(String, String), candidate: &(String, String)) -> f32 {
    if candidate == anchor {
        return 1.0;
    }
    // Both sides empty → no distinguishing context
    if anchor.0.is_empty() && anchor.1.is_empty() {
        return 0.2;
    }
    // Wildcard-left: same right token, any left
    if !anchor.1.is_empty() && candidate.1 == anchor.1 {
        return 0.8;
    }
    // Wildcard-right: same left token, any right
    if !anchor.0.is_empty() && candidate.0 == anchor.0 {
        return 0.7;
    }
    // Both sides non-empty → generic method call match
    if !candidate.0.is_empty() || !candidate.1.is_empty() {
        return 0.5;
    }
    0.0
}

/// find_references_pattern — pattern-based progressive find-references.
///
/// Uses best_context_key to resolve ambiguous columns, then progressive
/// wildcard matching (levels 1.0/0.8/0.7/0.5/0.2). Includes null-avoidance:
/// if the filtered result is empty, returns ALL occurrences at score 0.001.
pub fn find_references_pattern(
    db: &Connection, raw_name: &str, anchor_file: &str, anchor_line: i32,
    threshold: f32,
) -> rusqlite::Result<Vec<OccHit>> {
    let phrase_id = match phrase_id_for(db, raw_name)? {
        Some(id) => id, None => return Ok(vec![]),
    };
    let anchor_file_id = match file_id_for(db, anchor_file)? {
        Some(id) => id, None => return Ok(vec![]),
    };

    // Use best column resolution.
    let anchor_key = best_context_key(db, phrase_id, anchor_file, anchor_line, raw_name);

    // All occurrences of the phrase.
    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id WHERE o.phrase_id = ?1 AND f.is_source = 1",
    )?;
    let mut rows = stmt.query(params![phrase_id])?;
    let mut hits = Vec::new();

    while let Some(r) = rows.next()? {
        let occ_id: i64 = r.get(0)?;
        let hit_file_id: i64 = r.get(1)?;
        let hit_file_path: String = r.get(2)?;
        let line: i32 = r.get(3)?;
        let col: i32 = r.get(4)?;
        let is_def: i32 = r.get(5)?;
        let block_id: i64 = r.get(6)?;

        // Compute candidate key — use the exact stored column.
        let cand_key = context_key_at(&hit_file_path, line, raw_name, col as usize);

        // Anchor always matches itself perfectly.
        let sim = if hit_file_id == anchor_file_id && line == anchor_line.saturating_sub(1) {
            1.0
        } else {
            match_level(&anchor_key, &cand_key)
        };

        if sim >= threshold {
            hits.push(OccHit {
                occ_id, file_id: hit_file_id, file_path: hit_file_path,
                line, col, is_def: is_def != 0, block_id, similarity: sim,
            });
        }
    }

    // No null-avoidance: returning noise at 0.001 similarity was actively harmful.
    // If nothing matched, the caller gets an empty result and can decide to lower
    // the threshold or use a different tool (e.g. reliary_search for fuzzy match).
    hits.sort_by(|a, b| {
        b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.line.cmp(&b.line))
    });
    Ok(hits)
}

/// find_references_pattern_hybrid — combines pattern + callgraph + NCD.
///
/// Scoring cascade:
///   1. Pattern match level (1.0/0.8/0.7/0.5/0.2)
///   2. If pattern < threshold: callgraph Jaccard (0.9/0.7)
///   3. If both < threshold: NCD × 0.3
///   4. Null-avoidance: minimal 0.001 always
pub fn find_references_pattern_hybrid(
    db: &Connection, raw_name: &str, anchor_file: &str, anchor_line: i32,
    threshold: f32, k: i32,
) -> rusqlite::Result<Vec<OccHit>> {
    use crate::compat::{jaccard, extract_window, ncd_similarity};
    use crate::symbol::block_bag;

    let phrase_id = match phrase_id_for(db, raw_name)? {
        Some(id) => id, None => return Ok(vec![]),
    };
    let file_id = match file_id_for(db, anchor_file)? {
        Some(id) => id, None => return Ok(vec![]),
    };
    let anchor_block = match crate::symbol::block_id_at(db, file_id, anchor_line)? {
        Some(id) => id, None => return Ok(vec![]),
    };

    // Best anchor context key.
    let anchor_key = best_context_key(db, phrase_id, anchor_file, anchor_line, raw_name);

    // Anchor fingerprint (callgraph).
    let anchor_bag = block_bag(db, anchor_block)?;
    let anchor_fp: rustc_hash::FxHashSet<i64> = anchor_bag.keys().copied().collect();

    // Anchor window for NCD.
    // V54: use file_meta cache instead of disk re-read.
    let anchor_lines: Option<Vec<String>> = crate::file_meta::get(anchor_file)
        .map(|m| m.lines.clone())
        .or_else(|| std::fs::read_to_string(anchor_file).ok().map(|c| c.lines().map(|l| l.to_string()).collect()));
    let anchor_window = if let Some(ref lines) = anchor_lines {
        extract_window(lines, anchor_line, k)
    } else {
        String::new()
    };

    // All occurrences.
    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id WHERE o.phrase_id = ?1 AND f.is_source = 1",
    )?;
    let mut rows = stmt.query(params![phrase_id])?;
    let mut hits = Vec::new();
    let mut fp_cache: FxHashMap<i64, rustc_hash::FxHashSet<i64>> = FxHashMap::default();
    let mut lines_cache: rustc_hash::FxHashMap<String, Vec<String>> = rustc_hash::FxHashMap::default();

    // V54: batch-load all distinct block bags first — no per-hit SQL round trips.
    let mut all_occ: Vec<(i64, i64, String, i32, i32, i32, i64)> = Vec::new();
    while let Some(r) = rows.next()? {
        all_occ.push((
            r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?,
            r.get(4)?, r.get(5)?, r.get(6)?,
        ));
    }
    for &(_, _, _, _, _, _, block_id) in &all_occ {
        if let std::collections::hash_map::Entry::Vacant(e) = fp_cache.entry(block_id) {
            if let Ok(bag) = block_bag(db, block_id) {
                let fp: rustc_hash::FxHashSet<i64> = bag.keys().copied().collect();
                e.insert(fp);
            }
        }
    }

    for (occ_id, hit_file_id, hit_file_path, line, col, is_def, block_id) in all_occ {

        let sim = if hit_file_id == file_id && line == anchor_line {
            1.0
        } else {
            // 1. Pattern match level.
            let cand_key = context_key_at(&hit_file_path, line, raw_name, col as usize);
            let pattern_score = match_level(&anchor_key, &cand_key);

            if pattern_score >= threshold {
                pattern_score
            } else {
                // 2. Callgraph Jaccard.
                let cand_fp = match fp_cache.get(&block_id) {
                    Some(fp) => fp,
                    None => {
                        let bag = block_bag(db, block_id)?;
                        let fp: rustc_hash::FxHashSet<i64> = bag.keys().copied().collect();
                        fp_cache.insert(block_id, fp);
                        fp_cache.get(&block_id).unwrap()
                    }
                };
                let jac = jaccard(&anchor_fp, cand_fp);
                let fp_score = if jac >= 0.7 { 0.9 }
                    else if jac >= 0.4 { 0.7 }
                    else { 0.0 };

                if fp_score >= threshold {
                    fp_score
                } else {
                    // 3. NCD fallback.
                    let cand_lines = match lines_cache.entry(hit_file_path.clone()) {
                        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                        std::collections::hash_map::Entry::Vacant(e) => {
                            // V54: prefer file_meta cache; fall back to disk.
                            let loaded = crate::file_meta::get(&hit_file_path)
                                .map(|m| m.lines.clone())
                                .or_else(|| std::fs::read_to_string(&hit_file_path).ok().map(|c| c.lines().map(|l| l.to_string()).collect()))
                                .unwrap_or_default();
                            e.insert(loaded)
                        }
                    };
                    let cand_window = extract_window(cand_lines, line, k);
                    let ncd = ncd_similarity(&anchor_window, &cand_window);
                    ncd * 0.5
                }
            }
        };

        if sim >= threshold {
            hits.push(OccHit {
                occ_id, file_id: hit_file_id, file_path: hit_file_path,
                line, col, is_def: is_def != 0, block_id, similarity: sim,
            });
        }
    }

    // No null-avoidance: returning 3141 hits at 0.001 was drowning real signal.
    // Empty result is more useful — the caller can lower threshold or switch tools.
    hits.sort_by(|a, b| {
        b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.line.cmp(&b.line))
    });
    Ok(hits)
}