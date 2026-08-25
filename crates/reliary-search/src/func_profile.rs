//! Phase R1: Function-level co-occurrence via brace-graph.
//!
//! For each occurrence of a stem, find its enclosing `function_def` brace-graph
//! node. Collect all stems appearing in that function's body. Two occurrences
//! with high function-stem-set overlap are more likely to be same-definition.
//!
//! This replaces block bags (noisy, coarse) with function-level profiles
//! (precise, exact function boundary via brace-graph).

use crate::brace_graph::{get_brace_graph, BraceNode};
use crate::symbol::phrase_id_for;
use rusqlite::{params, Connection};
use ahash::AHashMap;
use rustc_hash::{FxHashMap, FxHashSet};
use parking_lot::Mutex;

/// Cache: file_path → function-level stem profiles keyed by phrase_id.
static FUNC_PROFILE_CACHE: std::sync::OnceLock<Mutex<AHashMap<String, Vec<FunctionProfile>>>> =
    std::sync::OnceLock::new();

fn profile_cache() -> &'static Mutex<AHashMap<String, Vec<FunctionProfile>>> {
    FUNC_PROFILE_CACHE.get_or_init(|| Mutex::new(AHashMap::default()))
}

/// Stem profile for a function_def brace-graph node.
#[derive(Clone, Debug)]
pub struct FunctionProfile {
    pub start_line: i32,
    pub end_line: i32,
    /// Set of phrase_ids that appear in this function's body.
    pub stems: FxHashSet<i64>,
}

/// Build function-level stem profiles for a file.
/// Returns one profile per function_def brace-graph node.
pub fn build_function_profiles(db: &Connection, file_path: &str) -> Vec<FunctionProfile> {
    {
        let cache = profile_cache().lock();
        if let Some(profiles) = cache.get(file_path) {
            return profiles.clone();
        }
    }

    let profiles = match compute_profiles(db, file_path) {
        Some(p) => p,
        None => Vec::new(),
    };

    let mut cache = profile_cache().lock();
    if cache.len() > 512 {
        let keys: Vec<String> = cache.keys().cloned().collect();
        for k in &keys[..keys.len() / 2] { cache.remove(k); }
    }
    cache.insert(file_path.to_string(), profiles.clone());
    profiles
}

fn compute_profiles(db: &Connection, file_path: &str) -> Option<Vec<FunctionProfile>> {
    // Arc 62 Phase 1: use file_meta brace-graph (already pre-warmed).
    // Falls back to get_brace_graph only if file_meta doesn't have it.
    let graph = crate::file_meta::get(file_path)
        .map(|m| m.brace_graph.clone())
        .or_else(|| get_brace_graph(file_path))?;
    let fns = graph.find_by_role("function_def");

    // Arc 62 Phase 2: batch load all phrase_ids for this file in ONE query,
    // then group by function boundary in Rust. Replaces N DB queries
    // (one per function) with 1 batch query.
    let mut all_stems: Vec<(i32, i64)> = Vec::new();
    {
        let mut stmt = match db.prepare_cached(
            "SELECT o.line, o.phrase_id FROM occurrence o
             JOIN file_map f ON o.file_id = f.id
             WHERE f.file_path = ?1 ORDER BY o.line"
        ) {
            Ok(s) => s,
            Err(_) => return None,
        };
        let mut rows = match stmt.query_map(
            rusqlite::params![file_path],
            |r| Ok((r.get::<_, i32>(0)?, r.get::<_, i64>(1)?)),
        ) {
            Ok(r) => r,
            Err(_) => return None,
        };
        while let Some(result) = rows.next() {
            if let Ok((line, pid)) = result {
                all_stems.push((line, pid));
            }
        }
    }

    let mut profiles = Vec::new();
    // P2-4: single-pass cursor over sorted all_stems instead of O(N×M) linear scan.
    let mut cursor = 0usize;
    for node in &fns {
        let mut stems = rustc_hash::FxHashSet::default();
        // Advance cursor past stems before this function.
        while cursor < all_stems.len() && all_stems[cursor].0 < node.start_line {
            cursor += 1;
        }
        // Collect stems within this function.
        let mut inner = cursor;
        while inner < all_stems.len() && all_stems[inner].0 <= node.end_line {
            stems.insert(all_stems[inner].1);
            inner += 1;
        }
        profiles.push(FunctionProfile {
            start_line: node.start_line,
            end_line: node.end_line,
            stems,
        });
    }

    Some(profiles)
}

/// Collect all phrase_ids appearing in a function's body, given the file path.
fn collect_stems_in_function(db: &Connection, file_path: &str, node: &BraceNode) -> FxHashSet<i64> {
    let mut stems = FxHashSet::default();

    let sql = "SELECT DISTINCT o.phrase_id FROM occurrence o JOIN file_map f ON o.file_id = f.id
               WHERE f.file_path = ?1 AND o.line >= ?2 AND o.line <= ?3";

    let mut stmt = match db.prepare_cached(sql) {
        Ok(s) => s,
        Err(_) => return stems,
    };
    let rows = match stmt.query_map(
        rusqlite::params![file_path, node.start_line, node.end_line],
        |r| r.get::<_, i64>(0),
    ) {
        Ok(r) => r,
        Err(_) => return stems,
    };
    for pid in rows {
        if let Ok(p) = pid { stems.insert(p); }
    }

    stems
}

/// Compute Jaccard similarity between two function stem profiles.
pub fn function_profile_jaccard(a: &FunctionProfile, b: &FunctionProfile) -> f32 {
    if a.stems.is_empty() && b.stems.is_empty() { return 0.5; }
    if a.stems.is_empty() || b.stems.is_empty() { return 0.0; }
    let inter = a.stems.intersection(&b.stems).count();
    let union = a.stems.union(&b.stems).count();
    if union == 0 { 0.0 } else { inter as f32 / union as f32 }
}

/// Find the function profile containing a given line.
pub fn find_enclosing_profile(profiles: &[FunctionProfile], line: i32) -> Option<&FunctionProfile> {
    profiles.iter().find(|p| line >= p.start_line && line <= p.end_line)
}

/// Compute function-level co-occurrence similarity between anchor and candidate.
/// Both must be in the same file or have known function profiles.
pub fn function_cooccurrence_score(
    db: &Connection,
    anchor_file: &str, anchor_line: i32,
    cand_file: &str, cand_line: i32,
    anchor_stem: &str,
) -> f32 {
    let anchor_pid = match phrase_id_for(db, anchor_stem) {
        Ok(Some(p)) => p,
        _ => return 0.0,
    };

    let anchor_profiles = build_function_profiles(db, anchor_file);
    let cand_profiles = build_function_profiles(db, cand_file);

    let anchor_p = find_enclosing_profile(&anchor_profiles, anchor_line);
    let cand_p = find_enclosing_profile(&cand_profiles, cand_line);

    match (anchor_p, cand_p) {
        (Some(a), Some(b)) => {
            // Stronger weight for functions sharing the anchor stem.
            let base_jacc = function_profile_jaccard(a, b);
            let shared_stem = a.stems.contains(&anchor_pid) && b.stems.contains(&anchor_pid);
            if shared_stem { base_jacc * 1.5 } else { base_jacc * 0.5 }
        }
        _ => 0.0,
    }
}