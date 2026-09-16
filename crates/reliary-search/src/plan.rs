//! V23: Hologram Plan — task-to-file mapping.
//! Given a task description, returns the most likely file to look at,
//! related test files, coupled files, and risk level. Ported from stria.
use rusqlite::{Connection, OptionalExtension};
use rustc_hash::FxHashMap;
use serde_json::{json, Value};

use crate::schema::{unpack_file_blob, unpack_is_def};

/// Extract meaningful words from a task description (lower-case, alphanum, len>=3).
fn extract_phrases(task: &str) -> Vec<String> {
    task.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() >= 3)
        .map(|s| s.to_lowercase())
        .collect()
}

/// V23: Given a task description, return a plan with edit/verify/read_first/coupled/risk.
/// Grammar-free: pure math on the existing phrase index.
pub fn hologram_plan(db: &Connection, task: &str) -> Value {
    let task_phrases = extract_phrases(task);
    if task_phrases.is_empty() {
        return json!({
            "edit": null,
            "verify": [],
            "read_first": [],
            "coupled": [],
            "risk": "unknown",
            "reason": "no extractable phrases in task",
        });
    }

    let n_docs: f64 = db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0))
        .unwrap_or(1.0_f64).max(1.0_f64);
    let avgdl: f64 = db.query_row("SELECT value FROM meta WHERE key='avgdl'", [], |r| r.get(0))
        .unwrap_or(100.0);

    // Build (file_id, flags) per phrase using phrase_occ blob unpack.
    let mut file_scores: FxHashMap<String, f64> = FxHashMap::default();
    // V54: batch file_id→path resolution — collect all needed ids, then one
    // IN-clause query (was N+1 per (phrase, file) pair).
    let mut needed_ids: Vec<i64> = Vec::new();
    let mut raw_scores: Vec<(i64, f64, u8)> = Vec::new();
    for st in &task_phrases {
        // Find phrase_id. Distinguish "no such phrase" (skip) from a real DB
        // error (log — silently returning an empty plan hides corruption).
        let phrase_id: Option<i64> = match db.query_row(
            "SELECT id FROM phrases WHERE phrase = ?1",
            [st],
            |r| r.get(0),
        ).optional() {
            Ok(v) => v,
            Err(e) => { eprintln!("[plan] phrase lookup failed for {:?}: {}", st, e); None }
        };
        let pid = match phrase_id { Some(p) => p, None => continue };

        // Get (file_blob) for this phrase.
        let blob: Option<Vec<u8>> = db.query_row(
            "SELECT file_blob FROM phrase_occ WHERE phrase_id = ?1",
            [pid],
            |r| r.get(0),
        ).ok().flatten();
        let blob = match blob { Some(b) => b, None => continue };

        // Unpack blob into (file_id, flags) pairs.
        let pairs: Vec<(i64, u8)> = unpack_file_blob(&blob).collect();
        if pairs.is_empty() { continue; }

        // BM25 IDF.
        let df = pairs.len() as f64;
        let idf = crate::bm25_idf(n_docs as f32, df as f32);

        for (fid, flags) in &pairs {
            needed_ids.push(*fid);
            let count = crate::schema::unpack_count(*flags);
            let tf = if count >= 31 { 1.0 } else { count as f64 };
            let is_def = unpack_is_def(*flags);
            let doc_len: f64 = 1.0;
            let score = crate::bm25_score(idf, tf as f32, doc_len as f32, avgdl as f32) as f64;
            let def_mult = if is_def > 0 { 5.0 } else { 1.0 };
            raw_scores.push((*fid, score * def_mult, *flags));
        }
    }

    // Batch-resolve file paths for all needed ids.
    let mut id_to_path: FxHashMap<i64, String> = FxHashMap::default();
    if !needed_ids.is_empty() {
        let mut uniq: Vec<i64> = needed_ids.clone();
        uniq.sort_unstable();
        uniq.dedup();
        let placeholders = std::iter::repeat_n("?", uniq.len()).collect::<Vec<_>>().join(",");
        let sql = format!("SELECT id, file_path FROM file_map WHERE id IN ({})", placeholders);
        if let Ok(mut stmt) = db.prepare_cached(&sql) {
            let refs: Vec<&dyn rusqlite::ToSql> = uniq.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
            if let Ok(rows) = stmt.query_map(refs.as_slice(), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            }) {
                for row in rows.flatten() {
                    id_to_path.insert(row.0, row.1);
                }
            }
        }
    }
    for (fid, score, _flags) in raw_scores {
        if let Some(fp) = id_to_path.get(&fid) {
            *file_scores.entry(fp.clone()).or_insert(0.0) += score;
        }
    }

    if file_scores.is_empty() {
        return json!({
            "edit": null,
            "verify": [],
            "read_first": [],
            "coupled": [],
            "risk": "unknown",
            "reason": "no matching files for phrases",
        });
    }

    let mut scored: Vec<(String, f64)> = file_scores.into_iter().collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let edit_file = scored.first().map(|(fp, _)| fp.clone());
    let verify: Vec<String> = scored.iter()
        .filter(|(fp, _)| fp.contains("test") || fp.contains("spec") || fp.contains("__tests__"))
        .take(2).map(|(fp, _)| fp.clone()).collect();
    let risk = if scored.is_empty() {
        "unknown"
    } else if scored[0].1 > 1.0 {
        "low"
    } else if scored[0].1 > 0.3 {
        "moderate"
    } else {
        "high"
    };

    // V23b: Slim output — just edit + verify + risk. The model can call
    // find_references/callgraph for the actual content. This keeps the
    // plan response small (~200 bytes) to avoid context bloat over 10 turns.
    json!({
        "edit": edit_file,
        "verify": verify,
        "risk": risk,
    })
}
