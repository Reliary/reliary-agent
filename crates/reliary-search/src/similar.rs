//! V58 P2a: HDC near-clone detection over the occurrence index.
//!
//! Each function definition's token multiset is encoded into a 10K-bit
//! bipolar hypervector (bundle of per-token HVs). Similarity = cosine =
//! XOR + popcount (64× fewer iterations than an i8 loop). Grammar-free:
//! tokens come from scan_identifiers, encoding is bag-of-tokens hashing.

use rusqlite::{params, Connection};
use rustc_hash::FxHashMap;
use reliary_memory::{MemoryStore, Hypervector};

pub struct SimilarFn {
    pub name: String,
    pub file: String,
    pub line: i32,
    pub similarity: f64,
}

/// Encode one function's body tokens into a hypervector.
fn encode_fn(store: &mut MemoryStore, tokens: &[String]) -> Hypervector {
    store.encode_tokens(tokens)
}

/// Find functions similar to `name`'s definition.
///
/// Loads all `is_def=1 tag=1` occurrences for candidate phrases, reads each
/// function body via file_meta cache (bounded), encodes, ranks by cosine.
/// Caps work: at most 400 candidate bodies scanned per call.
pub fn find_similar(db: &Connection, name: &str, top_n: usize) -> Vec<SimilarFn> {
    let mut out = Vec::new();
    let phrase_id = match crate::symbol::phrase_id_for(db, name) {
        Ok(Some(id)) => id,
        _ => return out,
    };
    if crate::lazy_occurrence::ensure_occurrence_for_phrase(db, phrase_id).is_err() {
        eprintln!("[similar] JIT occurrence build failed for phrase_id={}", phrase_id);
    }

    // Anchor definition site(s): take the best-ranked def.
    let defs: Vec<(String, i32)> = {
        let mut stmt = match db.prepare_cached(
            "SELECT f.file_path, o.line FROM occurrence o
             JOIN file_map f ON f.id = o.file_id
             WHERE o.phrase_id = ?1 AND o.is_def = 1 AND o.tag = 1
               AND f.file_path NOT LIKE '%.md'
             ORDER BY LENGTH(f.file_path) ASC LIMIT 3",
        ) {
            Ok(s) => s,
            Err(_) => return out,
        };
        let rows = stmt.query_map(params![phrase_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?))
        });
        match rows {
            Ok(rs) => rs.flatten().collect(),
            Err(_) => return out,
        }
    };
    if defs.is_empty() { return out; }

    // Anchor HV from its body tokens.
    let mut store = MemoryStore::new(10_000);
    // store is mutable: encode_tokens registers token HVs
    let (anchor_file, anchor_line) = &defs[0];
    let meta = match crate::file_meta::get(anchor_file) {
        Some(m) => m,
        None => return out,
    };
    let start = (*anchor_line).max(0) as usize;
    let end = (start + 60).min(meta.lines.len());
    let anchor_text: String = meta.lines[start..end].join("\n");
    let mut anchor_tokens: Vec<String> = crate::scan_identifiers(&anchor_text);
    anchor_tokens.retain(|t| t.len() >= 3);
    let anchor_hv = encode_fn(&mut store, &anchor_tokens);

    // V58b: candidate pool = ALL indexed function definitions (bounded).
    // The co-occurrence-pid approach under-sampled: doc-comment def rows and
    // the LIMIT-8-per-pid cut starved it. Direct def scan is simpler.
    let mut store = MemoryStore::new(10_000);
    let anchor_hv = encode_fn(&mut store, &anchor_tokens);

    let cand_rows: Vec<(String, i32)> = {
        let stmt = db.prepare_cached(
            "SELECT f.file_path, o.line FROM occurrence o
             JOIN file_map f ON f.id = o.file_id
             WHERE o.is_def = 1 AND o.tag = 1
               AND f.file_path NOT LIKE '%.md'
               AND f.file_path NOT LIKE '%.txt'
             ORDER BY RANDOM() LIMIT 400",
        );
        match stmt.and_then(|mut s| {
            s.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?))
            }).map(|rows| rows.flatten().collect::<Vec<_>>())
        }) {
            Ok(v) => { eprintln!("[similar] pool={} anchor_file={}", v.len(), anchor_file); v }
            Err(e) => { eprintln!("[similar] pool query err: {}", e); return out; }
        }
    };

    for (fp, line) in cand_rows {
        if &fp == anchor_file && (line - 2..=line + 2).contains(anchor_line) { continue; }
        let m = match crate::file_meta::get(&fp) { Some(m) => m, None => continue };
        let s = (line.max(0) as usize).min(m.lines.len());
        let e = (s + 60).min(m.lines.len());
        let text: String = m.lines[s..e].join("\n");
        let mut toks: Vec<String> = crate::scan_identifiers(&text);
        toks.retain(|t| t.len() >= 3);
        if toks.len() < 5 || anchor_tokens.len() < 5 { continue; }
        let hv = encode_fn(&mut store, &toks);
        let sim = anchor_hv.cosine(&hv);
        if sim > 0.20 {
            let disp = m.fn_names.get(line.max(0) as usize).cloned()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| fp.rsplit('/').next().unwrap_or(&fp).to_string());
            out.push(SimilarFn { name: disp, file: fp.clone(), line, similarity: sim });
        }
    }

    out.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(top_n);
    out
}

// Silence unused import when feature combos change.
#[allow(dead_code)]
fn _typecheck_hv(_: &FxHashMap<String, Hypervector>) {}
