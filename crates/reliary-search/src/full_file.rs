//! Phase T + U: Full-file receiver type inference + file-level orthogonal features.
//!
//! Phase T: scan ENTIRE file for impl headers, build impl-block map by brace depth.
//!   Each occurrence inherits the receiver type from its ENCLOSING impl block
//!   (lexical scope, not line proximity).
//!
//! Phase U: extract file-level features:
//!   1. use-set: imported type names from `use` statements at top of file
//!   2. mod-list: submodules declared
//!   3. impl-target-list: all types implemented in this file
//!
//! These features are ORTHOGONAL to call-site features (receiver type, context key).
//! They discriminate definitions from the FILE LEVEL — same definition in two files
//! has same call-site features but DIFFERENT impl-target-list (different files).

use crate::symbol::{OccHit, block_id_at, file_id_for, phrase_id_for};
use crate::compat::{infer_receiver_type, type_jaccard};
use rusqlite::{params, Connection};
use rustc_hash::FxHashMap;
use parking_lot::Mutex;

/// Global cache: file_path → file-level info.
/// One-time extraction per file, reused across queries.
static FILE_INFO_CACHE: Mutex<Option<FxHashMap<String, FileInfo>>> = Mutex::new(None);

#[derive(Clone, Debug, Default)]
pub struct FileInfo {
    pub impl_map: Vec<ImplSpan>,
    pub use_set: Vec<String>,
    pub mod_list: Vec<String>,
    pub impl_target_list: Vec<String>,
}

/// A continuous impl block span in the file: lines [start, end).
#[derive(Clone, Debug)]
pub struct ImplSpan {
    pub start_line: i32,
    pub end_line: i32,
    pub impl_text: String, // raw impl header text
    pub trait_name: String,
    pub target_type: String,
}

/// Build the file info once, cache it.
pub fn get_file_info(file_path: &str) -> FileInfo {
    {
        let cache = FILE_INFO_CACHE.lock();
        if let Some(ref m) = *cache {
            if let Some(fi) = m.get(file_path) {
                return fi.clone();
            }
        }
    }

    let info = parse_file_info(file_path);

    let mut cache = FILE_INFO_CACHE.lock();
    if cache.is_none() {
        *cache = Some(FxHashMap::default());
    }
    if let Some(ref mut m) = *cache {
        if m.len() > 512 {
            let keys: Vec<String> = m.keys().cloned().collect();
            for k in &keys[..keys.len() / 2] { m.remove(k); }
        }
        m.insert(file_path.to_string(), info.clone());
    }
    info
}

/// Parse a single file: find impl blocks, use statements, mod declarations.
fn parse_file_info(file_path: &str) -> FileInfo {
    let mut info = FileInfo::default();
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c, Err(_) => return info,
    };
    let lines: Vec<&str> = content.lines().collect();

    // Track brace depth and find impl blocks by lexical scope.
    let mut depth = 0i32;
    let mut impl_stack: Vec<(i32, i32, String)> = Vec::new(); // (depth_at_start, start_line, impl_text)
    let mut impl_spans: Vec<ImplSpan> = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        let line_num = (idx + 1) as i32;
        let trimmed = line.trim_start();

        // Arc 22: grammar-free — uses structural detection of type definitions.
        let has_open_block = trimmed.ends_with('{') || trimmed.ends_with(':');
        let result = crate::structural::classify_structural(trimmed, 0, has_open_block, false);
        // Type definition (impl/struct/enum) — block-start with `{` before `(`.
        if result.is_def && result.tag == 2 && !trimmed.contains("//") && !trimmed.starts_with("///") {
            // Only consider top-level or one-level-nested type blocks.
            if depth <= 1 {
                let cut = trimmed.find('{').or_else(|| trimmed.find(" where")).unwrap_or(trimmed.len());
                let impl_text = trimmed[..cut].trim().to_string();

                // Extract trait and target type from the impl header.
                let (trait_name, target_type) = parse_impl_header(&impl_text);
                impl_stack.push((depth, line_num, impl_text.clone()));

                // Stash a pending impl span (will be closed on matching brace).
                impl_spans.push(ImplSpan {
                    start_line: line_num,
                    end_line: line_num, // placeholder
                    impl_text: impl_text.clone(),
                    trait_name: trait_name.clone(),
                    target_type: target_type.clone(),
                });
                info.impl_target_list.push(target_type);
            }
        }

        // Track brace depth.
        for c in line.chars() {
            if c == '{' { depth += 1; }
            else if c == '}' {
                depth -= 1;
                if !impl_stack.is_empty() && depth < impl_stack.last().unwrap().0 {
                    // We closed an impl block.
                    let start_line = impl_stack.last().unwrap().1;
                    impl_stack.pop();
                    // Update the matching impl span.
                    if let Some(span) = impl_spans.iter_mut().rev().find(|s| s.start_line == start_line) {
                        span.end_line = line_num;
                    }
                }
            }
        }

        // Extract use statements (any depth).
        if trimmed.starts_with("use ") && !trimmed.contains("//") {
            // Strip `use` and `;`.
            let rest = &trimmed[4..];
            let cleaned = rest.trim_end_matches(';').trim();
            // Strip `crate::` and `super::` and `self::` prefixes.
            let path = cleaned.trim_start_matches("crate::")
                .trim_start_matches("super::")
                .trim_start_matches("self::");
            // Take the last segment as the type name.
            let last = path.rsplit("::").next().unwrap_or(path);
            // Skip generic re-exports like `*` or `{` or `as`.
            if !last.is_empty() && !last.contains('*') && !last.contains('{') && !last.starts_with("as ") {
                info.use_set.push(last.to_string());
            }
        }

        // Extract mod declarations (grammar-free: `mod ` followed by identifier).
        // Grammar-free check: line contains `mod ` followed by an identifier at start.
        // We check for the pattern: line trimmed = "mod ID..." or "pub mod ID..." etc.
        let is_mod_decl = if !trimmed.contains("//") && trimmed.contains("mod ") {
            let bytes = trimmed.as_bytes();
            // Find `mod ` and check it's preceded by start or whitespace.
            if let Some(pos) = trimmed.find("mod ") {
                let before_ok = pos == 0 || bytes[pos - 1] == b' ' || bytes[pos - 1] == b'(' || bytes[pos - 1] == b'&';
                // Check what's after `mod ` — must be an identifier.
                let after_pos = pos + 4;
                let after_ok = after_pos < bytes.len()
                    && (bytes[after_pos].is_ascii_alphabetic() || bytes[after_pos] == b'_');
                before_ok && after_ok
            } else { false }
        } else { false };
        if is_mod_decl {
            // Extract mod name (grammar-free: find identifier after the mod declaration).
            let mod_pos = trimmed.find("mod ").unwrap_or(0);
            let after = &trimmed[mod_pos + 4..];
            let name = after.split([' ', ';', '{']).next().unwrap_or("");
            if !name.is_empty() {
                info.mod_list.push(name.to_string());
            }
        }
    }

    info.impl_map = impl_spans;
    info.use_set.sort();
    info.use_set.dedup();
    info.mod_list.sort();
    info.mod_list.dedup();
    info.impl_target_list.sort();
    info.impl_target_list.dedup();
    info
}

/// Parse `impl Trait for Type {`, `impl Type {`, `impl<'a> Trait for Type {`, etc.
fn parse_impl_header(impl_text: &str) -> (String, String) {
    // Strip "impl " prefix.
    let after = impl_text.strip_prefix("impl").unwrap_or(impl_text).trim_start();

    // Strip lifetime/generic params.
    let after_lt = if let Some(lt) = after.find('<') {
        if let Some(gt) = after[lt..].find('>') {
            after[gt + 1..].trim_start()
        } else {
            after
        }
    } else {
        after
    };

    // Look for "for" separator.
    if let Some(pos) = after_lt.find(" for ") {
        let trait_part = after_lt[..pos].trim();
        let type_part = after_lt[pos + 5..].trim();
        (trait_part.to_string(), type_part.to_string())
    } else {
        // No "for" — it's `impl Type`.
        let type_part = after_lt.split_whitespace().next().unwrap_or("").to_string();
        (String::new(), type_part)
    }
}

/// Find the impl block that LEXICALLY encloses line N in the file.
/// Returns (trait_name, target_type) if found, else (None, None).
pub fn enclosing_impl_at(file_info: &FileInfo, line: i32) -> (Option<String>, Option<String>) {
    for span in &file_info.impl_map {
        if line >= span.start_line && line <= span.end_line {
            let trait_name = if span.trait_name.is_empty() { None } else { Some(span.trait_name.clone()) };
            let target_type = if span.target_type.is_empty() { None } else { Some(span.target_type.clone()) };
            return (trait_name, target_type);
        }
    }
    (None, None)
}

/// Compute file-level feature scores for a candidate vs anchor.
pub fn file_level_similarity(
    anchor_path: &str, anchor_line: i32,
    cand_path: &str, cand_line: i32,
) -> f32 {
    let anchor_info = get_file_info(anchor_path);
    let cand_info = get_file_info(cand_path);

    // Enclosing impl at the call site.
    let (anchor_trait, anchor_target) = enclosing_impl_at(&anchor_info, anchor_line);
    let (cand_trait, cand_target) = enclosing_impl_at(&cand_info, cand_line);

    let mut score = 0.0f32;

    // Same enclosing impl target → very strong signal.
    if let (Some(at), Some(ct)) = (&anchor_target, &cand_target) {
        if at == ct { score += 0.5; }
    }

    // Same trait → strong signal.
    if let (Some(at), Some(ct)) = (&anchor_trait, &cand_trait) {
        if at == ct { score += 0.3; }
    }

    // Same impl-target-list (same file's all targets) — moderate.
    if anchor_info.impl_target_list == cand_info.impl_target_list && !anchor_info.impl_target_list.is_empty() {
        score += 0.1;
    }

    // File use-set overlap — weak but ortho.
    if !anchor_info.use_set.is_empty() {
        let anchor_set: std::collections::HashSet<&str> = anchor_info.use_set.iter().map(String::as_str).collect();
        let cand_set: std::collections::HashSet<&str> = cand_info.use_set.iter().map(String::as_str).collect();
        let inter = anchor_set.intersection(&cand_set).count();
        let union = anchor_set.union(&cand_set).count();
        if union > 0 {
            score += (inter as f32 / union as f32) * 0.1;
        }
    }

    score.min(1.0)
}

/// find_references_full_file: uses full-file receiver type inference + file features.
pub fn find_references_full_file(
    db: &Connection, raw_name: &str, anchor_file: &str, anchor_line: i32,
    threshold: f32,
) -> rusqlite::Result<Vec<OccHit>> {
    let phrase_id = match phrase_id_for(db, raw_name)? { Some(id) => id, None => return Ok(vec![]) };
    let file_id = match file_id_for(db, anchor_file)? { Some(id) => id, None => return Ok(vec![]) };
    let anchor_block = match block_id_at(db, file_id, anchor_line)? { Some(id) => id, None => return Ok(vec![]) };

    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id WHERE o.phrase_id = ?1 AND f.is_source = 1",
    )?;
    let mut rows = stmt.query(params![phrase_id])?;
    let mut occs: Vec<(i64, i64, String, i32, i32, bool, i64)> = Vec::new();
    let mut anchor_idx = 0usize;
    while let Some(r) = rows.next()? {
        let oi = (r.get::<_,i64>(0)?, r.get::<_,i64>(1)?, r.get::<_,String>(2)?,
                  r.get::<_,i32>(3)?, r.get::<_,i32>(4)?, r.get::<_,i32>(5)? != 0, r.get::<_,i64>(6)?);
        if oi.1 == file_id && oi.3 == anchor_line && oi.6 == anchor_block { anchor_idx = occs.len(); }
        occs.push(oi);
    }
    let n = occs.len();
    if n < 3 { return Ok(vec![]); }
    if n > 1500 {
        return Ok(vec![]);
    }

    // Get anchor's full-file receiver type.
    let anchor_info = get_file_info(anchor_file);
    let (anchor_trait, anchor_target) = enclosing_impl_at(&anchor_info, anchor_line);

    // Fallback to regex type inference if full-file scan didn't find anything.
    let anchor_type = if anchor_target.is_some() {
        anchor_target.clone().unwrap_or_default()
    } else {
        infer_receiver_type(db, anchor_file, anchor_line, raw_name).unwrap_or_default()
    };

    let mut hits = Vec::new();
    for (i, oi) in occs.iter().enumerate() {
        let sim = if i == anchor_idx {
            1.0
        } else {
            // Use full-file type inference.
            let cand_info = get_file_info(&oi.2);
            let (cand_trait, cand_target) = enclosing_impl_at(&cand_info, oi.3);
            let cand_type = if cand_target.is_some() {
                cand_target.clone().unwrap_or_default()
            } else {
                infer_receiver_type(db, &oi.2, oi.3, raw_name).unwrap_or_default()
            };

            let mut s = 0.0f32;
            // Strong: same exact impl target.
            if !anchor_type.is_empty() && anchor_type == cand_type && !cand_type.is_empty() {
                s += 0.6;
            }
            // Trait match.
            if let (Some(at), Some(ct)) = (&anchor_trait, &cand_trait) {
                if at == ct && !at.is_empty() { s += 0.2; }
            }
            // Fallback: type_jaccard.
            s += type_jaccard(&anchor_type, &cand_type) * 0.3;
            // File-level features.
            s += file_level_similarity(anchor_file, anchor_line, &oi.2, oi.3) * 0.3;
            s.min(1.0)
        };
        if sim >= threshold {
            hits.push(OccHit {
                occ_id: oi.0, file_id: oi.1, file_path: oi.2.clone(),
                line: oi.3, col: oi.4, is_def: oi.5, block_id: oi.6,
                similarity: sim,
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

    hits.sort_by(|a, b| {
        b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.line.cmp(&b.line))
    });
    Ok(hits)
}