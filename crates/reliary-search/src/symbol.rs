//! Symbol-level queries (occurrence-level vocab, schema v2).
//!
//! The competitive moat: grammar-free find-references, goto-def, callgraph, scope,
//! dead-symbols — operations every grammar-aware tool has (LSP, tree-sitter, Semgrep,
//! CodeQL) and no grammar-free tool has, until now.
//!
//! All queries are **query-time neighborhood** (no upfront clustering): the caller
//! supplies an anchor (phrase, file, line) and we compute context similarity from
//! that anchor to all other occurrences of the phrase. The "cluster" emerges from
//! the query; homonyms split naturally because their blocks carry different context.
//!
//! Context representation: hybrid window+block — a block bag contains all stems
//! within the enclosing indentation-anchored block of each occurrence. Cosine
//! similarity between bags is the disambiguation signal. Window-truncation at block
//! boundaries is implicit (block IS the bounded context).

use rusqlite::{params, Connection};
use rustc_hash::FxHashMap;

/// A single occurrence returned by a query.
#[derive(Debug, Clone)]
pub struct OccHit {
    pub occ_id: i64,
    pub file_id: i64,
    pub file_path: String,
    pub line: i32,
    pub col: i32,
    pub is_def: bool,
    pub block_id: i64,
    /// Similarity score vs the anchor (0.0..=1.0). 1.0 for the anchor itself.
    pub similarity: f32,
}

/// Look up the global block_id for a (file_id, line) anchor.
/// Returns None if no block contains that line.
pub fn block_id_at(db: &Connection, file_id: i64, line: i32) -> rusqlite::Result<Option<i64>> {
    // Arc 35 lazy mode: if blocks for this file haven't been built yet,
    // JIT-build them now. Idempotent — no-op if already populated.
    if crate::lazy_tables::ensure_blocks_for_file(db, file_id).is_err() { eprintln!("[symbol] ensure_blocks_for_file failed for file_id={}", file_id); }
    // V51: convert 1-indexed MCP param to 0-indexed (block table uses 0-indexed lines).
    let line_0idx = line.saturating_sub(1);
    // V60: require end_line >= line so gap lines (blank/comment between
    // blocks) don't resolve to a block that already ended.
    let mut stmt = db.prepare_cached(
        "SELECT block_id FROM block WHERE file_id = ?1 AND start_line <= ?2 AND end_line >= ?2 ORDER BY end_line DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![file_id, line_0idx])?;
    if let Some(r) = rows.next()? {
        Ok(Some(r.get(0)?))
    } else {
        Ok(None)
    }
}

/// Compute the context bag for a block: (phrase_id, tag) -> count.
/// Using (phrase, tag) as the bag dimension disambiguates same-stem different-uses:
/// e.g., `poll` as fn_def (3) vs `poll` as method_call (0) vs `poll` as param (5)
/// occupy different positions in the bag.
pub(crate) fn block_bag(db: &Connection, block_id: i64) -> rusqlite::Result<FxHashMap<i64, u32>> {
    let mut stmt = db.prepare_cached(
        "SELECT phrase_id, tag, COUNT(*) FROM occurrence WHERE block_id = ?1 GROUP BY phrase_id, tag",
    )?;
    let mut rows = stmt.query(params![block_id])?;
    let mut bag = FxHashMap::default();
    while let Some(r) = rows.next()? {
        let pid: i64 = r.get(0)?;
        let tag: i64 = r.get(1)?;
        let c: i64 = r.get(2)?;
        // Composite key: phrase_id * 8 + tag (tag is 0..7).
        let key = pid * 8 + tag;
        bag.insert(key, c as u32);
    }
    Ok(bag)
}

/// Bigram-augmented block bag: (phrase, tag) + (phrase, next_phrase) pairs.
///
/// Phase 2 of the research arc. In addition to unigram counts, this bag includes
/// bigram keys: for each pair of consecutive tokens on the same line within the block,
/// a composite key (phrase_id * N_PHRASES + next_phrase_id) is added. This captures
/// usage patterns like `poll(cx)` vs `poll(handle)` that unigram bags can't distinguish.
///
/// The bigram dimension uses a fixed large multiplier to avoid collision with unigram keys.
fn block_bag_bigram(db: &Connection, block_id: i64) -> rusqlite::Result<FxHashMap<i64, u32>> {
    // Unigram part (same as block_bag).
    let mut bag = block_bag(db, block_id)?;

    // Bigram part: for each line in this block, get ordered occurrences and pair them.
    // Use a self-join approach: consecutive tokens on the same line in the same block.
    let mut stmt = db.prepare_cached(
        "SELECT a.phrase_id, b.phrase_id, COUNT(*)
         FROM occurrence a
         JOIN occurrence b ON a.file_id = b.file_id AND a.line = b.line AND a.col < b.col AND a.block_id = b.block_id
         WHERE a.block_id = ?1
         GROUP BY a.phrase_id, b.phrase_id",
    )?;
    let mut rows = stmt.query(params![block_id])?;
    let bigram_offset: i64 = 10_000_000;
    let max_phrase_id: i64 = db.query_row("SELECT COALESCE(MAX(id), 1) FROM phrases", [], |r| r.get(0)).unwrap_or(1);
    let stride = max_phrase_id + 1;
    while let Some(r) = rows.next()? {
        let pid_a: i64 = r.get(0)?;
        let pid_b: i64 = r.get(1)?;
        let c: i64 = r.get(2)?;
        let bigram_key = pid_a * stride + pid_b + bigram_offset;
        *bag.entry(bigram_key).or_insert(0) += c as u32;
    }
    Ok(bag)
}

/// Window-bounded context bag: ±K lines around a center line, truncated at block boundaries.
///
/// Phase 1 of the research arc. Instead of bagging the entire enclosing block, bag only
/// the ±K lines around the anchor's line, stopping at the block's start/end lines.
/// This produces a tighter, more discriminating context for method-call sites where
/// the whole function is too uniform but the local call-site neighborhood is distinctive.
///
/// Returns the bag AND the effective line range used (may be narrower than ±K if the
/// block is small or the anchor is near a boundary).
fn window_bag(
    db: &Connection,
    file_id: i64,
    center_line: i32,
    k: i32,
) -> rusqlite::Result<FxHashMap<i64, u32>> {
    // Find the enclosing block to truncate at boundaries.
    let block = match block_id_at(db, file_id, center_line)? {
        Some(b) => b,
        None => return Ok(FxHashMap::default()),
    };
    let (block_start, block_end): (i32, i32) = db.query_row(
        "SELECT start_line, end_line FROM block WHERE block_id = ?1",
        params![block],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    // Window: ±K lines around center, clamped to block boundaries.
    let win_start = (center_line - k).max(block_start);
    let win_end = (center_line + k).min(block_end);

    let mut stmt = db.prepare_cached(
        "SELECT phrase_id, tag, COUNT(*) FROM occurrence
         WHERE file_id = ?1 AND line >= ?2 AND line <= ?3
         GROUP BY phrase_id, tag",
    )?;
    let mut rows = stmt.query(params![file_id, win_start, win_end])?;
    let mut bag = FxHashMap::default();
    while let Some(r) = rows.next()? {
        let pid: i64 = r.get(0)?;
        let tag: i64 = r.get(1)?;
        let c: i64 = r.get(2)?;
        let key = pid * 8 + tag;
        bag.insert(key, c as u32);
    }
    Ok(bag)
}

/// IDF cache: phrase_id -> inverse document frequency.
/// Computed once per corpus (total unique blocks + per-phrase block frequency).
/// `idf = ln((N + 1) / (df + 1)) + 1` — smoothed, always positive, common words ~1.0, rare words higher.
pub struct IdfTable {
    weights: FxHashMap<i64, f32>,
}

impl IdfTable {
    pub fn compute(db: &Connection) -> rusqlite::Result<Self> {
        let total_blocks: i64 = db.query_row(
            "SELECT COUNT(DISTINCT block_id) FROM occurrence",
            [],
            |r| r.get(0),
        )?;
        let n = total_blocks.max(1) as f64;
        let mut stmt = db.prepare_cached(
            "SELECT phrase_id, COUNT(DISTINCT block_id) FROM occurrence GROUP BY phrase_id",
        )?;
        let mut rows = stmt.query([])?;
        let mut weights = FxHashMap::default();
        while let Some(r) = rows.next()? {
            let pid: i64 = r.get(0)?;
            let df: i64 = r.get(1)?;
            let idf = ((n + 1.0) / (df.max(1) as f64 + 1.0)).ln() + 1.0;
            weights.insert(pid, idf as f32);
        }
        Ok(Self { weights })
    }

    pub fn weight(&self, pid: i64) -> f32 {
        self.weights.get(&pid).copied().unwrap_or(1.0)
    }

    /// Apply IDF weights to a raw count bag, returning a TF-IDF bag (f32).
    pub fn weight_bag(&self, raw: &FxHashMap<i64, u32>) -> FxHashMap<i64, f32> {
        let mut out = FxHashMap::default();
        for (k, v) in raw {
            let w = self.weight(*k);
            if w > 0.0001 {
                out.insert(*k, (*v as f32) * w);
            }
        }
        out
    }
}

/// Weighted block bag: raw counts multiplied by IDF weights.
#[allow(dead_code)]
fn block_bag_idf(db: &Connection, idf: &IdfTable, block_id: i64) -> rusqlite::Result<FxHashMap<i64, f32>> {
    let raw = block_bag(db, block_id)?;
    Ok(idf.weight_bag(&raw))
}

/// Cosine similarity between two bags (sparse maps).
pub(crate) fn cosine(a: &FxHashMap<i64, u32>, b: &FxHashMap<i64, u32>) -> f32 {
    if a.is_empty() || b.is_empty() { return 0.0; }
    let mut dot = 0u128;
    let mut na = 0u128;
    let mut nb = 0u128;
    for (k, va) in a {
        na += (*va as u128) * (*va as u128);
        if let Some(vb) = b.get(k) {
            dot += (*va as u128) * (*vb as u128);
        }
    }
    for vb in b.values() {
        nb += (*vb as u128) * (*vb as u128);
    }
    let denom = ((na as f64).sqrt() * (nb as f64).sqrt()) as f32;
    if denom == 0.0 { 0.0 } else { (dot as f64 / denom as f64) as f32 }
}

/// Cosine similarity between two TF-IDF bags (f32 values).
fn cosine_f32(a: &FxHashMap<i64, f32>, b: &FxHashMap<i64, f32>) -> f32 {
    if a.is_empty() || b.is_empty() { return 0.0; }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (k, va) in a {
        let va_f = *va as f64;
        na += va_f * va_f;
        if let Some(vb) = b.get(k) {
            dot += va_f * (*vb as f64);
        }
    }
    for vb in b.values() {
        let vb_f = *vb as f64;
        nb += vb_f * vb_f;
    }
    let denom = (na.sqrt() * nb.sqrt()) as f32;
    if denom == 0.0 { 0.0 } else { (dot / denom as f64) as f32 }
}

/// Stem a raw token the same way the indexer does (for callers that supply raw names).
/// V61: use stem_identifier — the indexer (ingest.rs, lazy_occurrence.rs) stores
/// stem_identifier outputs which preserve snake_case/CamelCase compounds.
/// porter_stem strips `al` from `structural` → `structur`, destroying compound
/// names like `classify_structural` (every such lookup missed and paid the
/// noisy unstemmed fallback).
pub fn stem(token: &str) -> String {
    crate::stem_identifier(token)
}

/// Resolve (raw_name) → phrase_id, or None if the stem isn't in the index.
pub fn phrase_id_for(db: &Connection, raw_name: &str) -> rusqlite::Result<Option<i64>> {
    let stem = stem(raw_name);
    let mut stmt = db.prepare_cached("SELECT id FROM phrases WHERE phrase = ?1 LIMIT 1")?;
    // Try stemmed first.
    let stemmed_pid: Option<i64> = {
        let mut rows = stmt.query(params![stem])?;
        rows.next()?.map(|r| r.get::<_, i64>(0)).transpose()?
    };
    if let Some(pid) = stemmed_pid {
        // Verify this phrase_id has occurrence data. If stale (0 rows),
        // try the unstemmed literal as fallback.
        // V54: COUNT(*) → EXISTS with LIMIT 1 — early-exit instead of full scan.
        let has_occ: i64 = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM occurrence WHERE phrase_id = ?1 LIMIT 1)",
            params![pid], |r| r.get(0)
        ).unwrap_or(0);
        if has_occ > 0 {
            return Ok(Some(pid));
        }
        eprintln!("[guard:phrase_id_for] stemmed phrase_id={} for '{}' (stem='{}') has 0 occurrence rows, trying fallback", pid, raw_name, stem);
    } else {
        eprintln!("[guard:phrase_id_for] no stemmed match for '{}' (stem='{}'), trying fallback", raw_name, stem);
    }
    // Try unstemmed literal (e.g., "consume" instead of "consum").
    let lower = raw_name.to_ascii_lowercase();
    let mut rows2 = stmt.query(params![lower])?;
    if let Some(r) = rows2.next()? {
        return Ok(Some(r.get(0)?));
    }
    Ok(None)
}

/// Look up file_id from file_path.
pub fn file_id_for(db: &Connection, file_path: &str) -> rusqlite::Result<Option<i64>> {
    let mut stmt = db.prepare_cached("SELECT id FROM file_map WHERE file_path = ?1 LIMIT 1")?;
    let mut rows = stmt.query(params![file_path])?;
    if let Some(r) = rows.next()? {
        Ok(Some(r.get(0)?))
    } else {
        Ok(None)
    }
}

/// find_references(raw_name, anchor_file, anchor_line, threshold) — all occurrences of
/// Compute the corpus-mean block bag: for each (phrase_id, tag), the average count
/// across all blocks. Used by find_references_centered to remove
/// common-mode boilerplate from block similarity comparisons.
pub fn compute_corpus_mean(db: &Connection) -> rusqlite::Result<FxHashMap<i64, f64>> {
    let total_blocks: i64 = db.query_row("SELECT COUNT(*) FROM block", [], |r| r.get(0))?;
    if total_blocks == 0 { return Ok(FxHashMap::default()); }
    let tf = total_blocks as f64;
    let mut stmt = db.prepare_cached(
        "SELECT phrase_id, tag, CAST(COUNT(*) AS REAL) / ?1
         FROM occurrence GROUP BY phrase_id, tag",
    )?;
    let mut rows = stmt.query(params![tf])?;
    let mut mean = FxHashMap::default();
    while let Some(r) = rows.next()? {
        let pid: i64 = r.get(0)?;
        let tag: i64 = r.get(1)?;
        let avg: f64 = r.get(2)?;
        mean.insert(pid * 8 + tag, avg);
    }
    Ok(mean)
}

/// centered_bag subtracts the corpus mean from a block bag.
/// This removes common-mode boilerplate that inflates similarity
/// between unrelated blocks. After centering, the residual captures
/// only the *distinctive* vocabulary of a block.
fn centered_bag(bag: &FxHashMap<i64, u32>, mean: &FxHashMap<i64, f64>) -> FxHashMap<i64, f32> {
    let mut result = FxHashMap::default();
    for (&key, &count) in bag {
        let m = mean.get(&key).copied().unwrap_or(0.0);
        result.insert(key, count as f32 - m as f32);
    }
    result
}

/// find_references_centered — Phase 7 (common-mode rejection).
///
/// Subtracts the corpus-mean bag from each block's bag before cosine. This removes
/// the "canonical function body" that every block shares (let, self, return, fn),
/// leaving only the distinctive vocabulary.
pub fn find_references_centered(
    db: &Connection, raw_name: &str, anchor_file: &str, anchor_line: i32, threshold: f32,
) -> rusqlite::Result<Vec<OccHit>> {
    let phrase_id = match phrase_id_for(db, raw_name)? { Some(id) => id, None => return Ok(vec![]) };
    let file_id = match file_id_for(db, anchor_file)? { Some(id) => id, None => return Ok(vec![]) };
    let anchor_block = match block_id_at(db, file_id, anchor_line)? { Some(id) => id, None => return Ok(vec![]) };
    let corpus_mean = compute_corpus_mean(db)?;
    let anchor_raw = block_bag(db, anchor_block)?;
    let anchor_centered = centered_bag(&anchor_raw, &corpus_mean);
    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id WHERE o.phrase_id = ?1 AND f.is_source = 1",
    )?;
    let mut rows = stmt.query(params![phrase_id])?;
    let mut hits = Vec::new();
    let mut bag_cache: FxHashMap<i64, FxHashMap<i64, f32>> = FxHashMap::default();
    while let Some(r) = rows.next()? {
        let occ_id: i64 = r.get(0)?;
        let hit_file_id: i64 = r.get(1)?;
        let hit_file_path: String = r.get(2)?;
        let line: i32 = r.get(3)?;
        let col: i32 = r.get(4)?;
        let is_def: i32 = r.get(5)?;
        let block_id: i64 = r.get(6)?;
        let sim = if block_id == anchor_block { 1.0 } else {
            let bag = match bag_cache.get(&block_id) {
                Some(b) => b,
                None => { let c = centered_bag(&block_bag(db, block_id)?, &corpus_mean); bag_cache.insert(block_id, c); bag_cache.get(&block_id).expect("just inserted") }
            };
            cosine_f32(&anchor_centered, bag)
        };
        if sim >= threshold {
            hits.push(OccHit { occ_id, file_id: hit_file_id, file_path: hit_file_path, line, col, is_def: is_def != 0, block_id, similarity: sim });
        }
    }
    hits.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.file_path.cmp(&b.file_path)).then_with(|| a.line.cmp(&b.line)));
    Ok(hits)
}

/// find_references_prototype — Phase 9 (inverse / prototype discovery).
///
/// Instead of measuring similarity directly to the anchor, discovers K canonical
/// uses of the phrase via farthest-first traversal, then computes:
///   sim(candidate) = max over prototypes P of anchor_proto_sim[P] × cosine(candidate, P)
///
/// The bilinear form naturally suppresses prototypes irrelevant to the anchor's role
/// without requiring labels. If the anchor is a function_def, prototypes close to it
/// are weighted higher, so function_def candidates rank above method_call ones.
pub fn find_references_prototype(
    db: &Connection, raw_name: &str, anchor_file: &str, anchor_line: i32, threshold: f32, k: usize,
) -> rusqlite::Result<Vec<OccHit>> {
    let phrase_id = match phrase_id_for(db, raw_name)? { Some(id) => id, None => return Ok(vec![]) };
    let file_id = match file_id_for(db, anchor_file)? { Some(id) => id, None => return Ok(vec![]) };
    let anchor_block = match block_id_at(db, file_id, anchor_line)? { Some(id) => id, None => return Ok(vec![]) };

    // Get all distinct blocks containing this phrase.
    let mut blk_stmt = db.prepare_cached(
        "SELECT DISTINCT block_id FROM occurrence WHERE phrase_id = ?1 AND block_id != ?2",
    )?;
    let mut blk_rows = blk_stmt.query(params![phrase_id, anchor_block])?;
    let mut candidate_blocks: Vec<i64> = Vec::new();
    while let Some(r) = blk_rows.next()? {
        candidate_blocks.push(r.get(0)?);
    }
    if candidate_blocks.is_empty() { return Ok(vec![]); }

    // Cache block bags.
    let mut bag_cache: FxHashMap<i64, FxHashMap<i64, u32>> = FxHashMap::default();
    let anchor_bag = block_bag(db, anchor_block)?;
    bag_cache.insert(anchor_block, anchor_bag.clone());

    // Farthest-first traversal: pick K prototypes (including anchor).
    let effective_k = (k).max(2).min(candidate_blocks.len() + 1);
    let mut prototypes: Vec<i64> = Vec::with_capacity(effective_k);
    prototypes.push(anchor_block);
    bag_cache.insert(anchor_block, anchor_bag.clone());

    for _ in 1..effective_k {
        // Pre-fetch all candidate bags outside the inner loop to avoid borrow conflicts.
        for &bid in &candidate_blocks {
            if let std::collections::hash_map::Entry::Vacant(e) = bag_cache.entry(bid) {
                if let Ok(bag) = block_bag(db, bid) {
                    e.insert(bag);
                }
            }
        }
        let mut best_bid = candidate_blocks[0];
        let mut best_min_sim = -1.0f32;
        for &bid in &candidate_blocks {
            if prototypes.contains(&bid) { continue; }
            let bag = bag_cache.get(&bid).expect("bag pre-inserted above");
            let mut min_sim = 1.0f32;
            for &p in &prototypes {
                let pb = bag_cache.get(&p).expect("bag pre-inserted above");
                let s = cosine(pb, bag);
                if s < min_sim { min_sim = s; }
            }
            if min_sim > best_min_sim {
                best_min_sim = min_sim;
                best_bid = bid;
            }
        }
        prototypes.push(best_bid);
    }

    // Anchor's similarity to each prototype.
    let anchor_proto_sim: Vec<f32> = prototypes.iter().map(|p| {
        if *p == anchor_block { 1.0 } else {
            match bag_cache.get(p) {
                Some(pb) => cosine(&anchor_bag, pb),
                None => 0.0, // bag build failed — skip this prototype
            }
        }
    }).collect();

    // Now score all occurrences.
    let mut occ_stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id WHERE o.phrase_id = ?1 AND f.is_source = 1",
    )?;
    let mut occ_rows = occ_stmt.query(params![phrase_id])?;
    let mut hits = Vec::new();

    while let Some(r) = occ_rows.next()? {
        let occ_id: i64 = r.get(0)?;
        let hit_file_id: i64 = r.get(1)?;
        let hit_file_path: String = r.get(2)?;
        let line: i32 = r.get(3)?;
        let col: i32 = r.get(4)?;
        let is_def: i32 = r.get(5)?;
        let block_id: i64 = r.get(6)?;

        // Pre-fetch candidate bag outside the inner loop.
        if let std::collections::hash_map::Entry::Vacant(e) = bag_cache.entry(block_id) {
            if let Ok(bag) = block_bag(db, block_id) {
                e.insert(bag);
            }
        }

        let mut sim = 0.0f32;
        for (i, &p) in prototypes.iter().enumerate() {
            let aps = anchor_proto_sim[i];
            if aps < 1e-6 { continue; }
            let (Some(pb), Some(cb)) = (bag_cache.get(&p), bag_cache.get(&block_id)) else {
                continue; // bag build failed — skip
            };
            let cs = cosine(pb, cb);
            let combined = aps * cs;
            if combined > sim { sim = combined; }
        }

        if sim >= threshold {
            hits.push(OccHit {
                occ_id, file_id: hit_file_id, file_path: hit_file_path,
                line, col, is_def: is_def != 0, block_id, similarity: sim,
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

/// `raw_name` whose block-bag cosine similarity to the anchor's block is ≥ `threshold`.
/// The anchor itself is always included (similarity = 1.0). Returns hits sorted by
/// similarity descending, then by file/line for determinism.
pub fn find_references(
    db: &Connection,
    raw_name: &str,
    anchor_file: &str,
    anchor_line: i32,
    threshold: f32,
) -> rusqlite::Result<Vec<OccHit>> {
    let phrase_id = match phrase_id_for(db, raw_name)? {
        Some(id) => id,
        None => return Ok(vec![]),
    };
    let file_id = match file_id_for(db, anchor_file)? {
        Some(id) => id,
        None => return Ok(vec![]),
    };
    let anchor_block = match block_id_at(db, file_id, anchor_line)? {
        Some(id) => id,
        None => return Ok(vec![]),
    };
    let anchor_bag = block_bag(db, anchor_block)?;

    // All occurrences of this phrase across the corpus.
    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id
         WHERE o.phrase_id = ?1",
    )?;
    let mut rows = stmt.query(params![phrase_id])?;
    let mut hits = Vec::new();
    // V54: batch-load block bags. Collect ALL occurrences first, then load
    // every distinct block bag in one pass — no per-hit SQL round trips.
    let mut bag_cache: FxHashMap<i64, FxHashMap<i64, u32>> = FxHashMap::default();
    let mut all_occ: Vec<(i64, i64, String, i32, i32, i32, i64)> = Vec::new();
    while let Some(r) = rows.next()? {
        all_occ.push((
            r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?,
            r.get(4)?, r.get(5)?, r.get(6)?,
        ));
    }
    for &(_, _, _, _, _, _, block_id) in &all_occ {
        if block_id != anchor_block && !bag_cache.contains_key(&block_id) {
            if let Ok(bag) = block_bag(db, block_id) {
                bag_cache.insert(block_id, bag);
            }
        }
    }

    // Arc 21 (grammar-free): use is_def flag from DB to surface definition candidates.
    // No keyword matching — the indexer already classified lines structurally.
    // is_def is set by the indexer's structural detector (crates/reliary-search/src/structural.rs).

    for (occ_id, hit_file_id, hit_file_path, line, col, is_def, block_id) in all_occ {
        // Grammar-free definition detection: use is_def from DB.
        let is_def_score: f32 = if is_def != 0 { 0.9 } else { 0.0 };
        let sim = if block_id == anchor_block {
            1.0
        } else if is_def != 0 {
            // Grammar-free: definitions bypass block similarity.
            is_def_score
        } else {
            let bag = match bag_cache.get(&block_id) {
                Some(b) => b,
                None => {
                    let b = block_bag(db, block_id)?;
                    bag_cache.insert(block_id, b);
                    bag_cache.get(&block_id).expect("just inserted")
                }
            };
            cosine(&anchor_bag, bag)
        };
        // Phase 3: don't pre-filter by threshold. Apply all signals, then filter.
        // A-MED-11: defs also need minimum threshold (half the user threshold, min 0.05).
        // Prevents defs with near-zero similarity from flooding results.
        let final_sim = if is_def != 0 { sim.max(is_def_score) } else { sim };
        let def_min_threshold = (threshold * 0.5).max(0.05);
        let passes = if is_def != 0 { final_sim >= def_min_threshold } else { final_sim >= threshold };
        if passes {
            hits.push(OccHit {
                occ_id,
                file_id: hit_file_id,
                file_path: hit_file_path,
                line,
                col,
                is_def: is_def != 0,
                block_id,
                similarity: final_sim,
            });
        }
    }
    // Stable sort: similarity desc, then file/line/col.
    hits.sort_by(|a, b| {
        b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.col.cmp(&b.col))
    });
    Ok(hits)
}

/// find_references_window — Phase 1 (research arc): window-bounded context bags.
///
/// Same as find_references but uses ±K line windows (block-truncated) instead of
/// whole-block bags. The window is tighter, capturing the local call-site neighborhood
/// rather than the entire function body.
pub fn find_references_window(
    db: &Connection,
    raw_name: &str,
    anchor_file: &str,
    anchor_line: i32,
    threshold: f32,
    k: i32,
) -> rusqlite::Result<Vec<OccHit>> {
    let phrase_id = match phrase_id_for(db, raw_name)? {
        Some(id) => id,
        None => return Ok(vec![]),
    };
    let file_id = match file_id_for(db, anchor_file)? {
        Some(id) => id,
        None => return Ok(vec![]),
    };
    let anchor_bag = window_bag(db, file_id, anchor_line, k)?;
    if anchor_bag.is_empty() {
        return Ok(vec![]);
    }

    // All occurrences of this phrase across the corpus.
    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id
         WHERE o.phrase_id = ?1",
    )?;
    let mut rows = stmt.query(params![phrase_id])?;
    let mut hits = Vec::new();
    // Cache window bags keyed by (file_id, line) — but line varies, so cache by block_id
    // and compute the window for the occurrence's center line.
    // For efficiency, cache the BLOCK bag and derive window lazily.
    // Actually for Phase 1 we compute window per occurrence (slower but correct).
    // Optimisation: cache by (file_id, line) since many occurrences share lines.
    let mut win_cache: FxHashMap<(i64, i32), FxHashMap<i64, u32>> = FxHashMap::default();

    while let Some(r) = rows.next()? {
        let occ_id: i64 = r.get(0)?;
        let hit_file_id: i64 = r.get(1)?;
        let hit_file_path: String = r.get(2)?;
        let line: i32 = r.get(3)?;
        let col: i32 = r.get(4)?;
        let is_def: i32 = r.get(5)?;
        let block_id: i64 = r.get(6)?;

        // Self-similarity check: if same file and within window of anchor.
        let is_anchor = hit_file_id == file_id && line == anchor_line;
        let sim = if is_anchor {
            1.0
        } else {
            let key = (hit_file_id, line);
            let bag = match win_cache.get(&key) {
                Some(b) => b,
                None => {
                    let b = window_bag(db, hit_file_id, line, k)?;
                    win_cache.insert(key, b);
                    win_cache.get(&(hit_file_id, line)).unwrap()
                }
            };
            cosine(&anchor_bag, bag)
        };
        let _ = block_id; // kept for OccHit but not used for window computation
        if sim >= threshold {
            hits.push(OccHit {
                occ_id,
                file_id: hit_file_id,
                file_path: hit_file_path,
                line,
                col,
                is_def: is_def != 0,
                block_id,
                similarity: sim,
            });
        }
    }
    hits.sort_by(|a, b| {
        b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.col.cmp(&b.col))
    });
    Ok(hits)
}

/// find_references_bigram — Phase 2 (research arc): bigram-augmented block bags.
///
/// Same as find_references but uses (phrase, next_phrase) bigram keys in addition
/// to unigram keys. Captures usage patterns like `poll(cx)` vs `poll(handle)`.
pub fn find_references_bigram(
    db: &Connection,
    raw_name: &str,
    anchor_file: &str,
    anchor_line: i32,
    threshold: f32,
) -> rusqlite::Result<Vec<OccHit>> {
    let phrase_id = match phrase_id_for(db, raw_name)? {
        Some(id) => id,
        None => return Ok(vec![]),
    };
    let file_id = match file_id_for(db, anchor_file)? {
        Some(id) => id,
        None => return Ok(vec![]),
    };
    let anchor_block = match block_id_at(db, file_id, anchor_line)? {
        Some(id) => id,
        None => return Ok(vec![]),
    };
    let anchor_bag = block_bag_bigram(db, anchor_block)?;

    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id
         WHERE o.phrase_id = ?1",
    )?;
    let mut rows = stmt.query(params![phrase_id])?;
    let mut hits = Vec::new();
    let mut bag_cache: FxHashMap<i64, FxHashMap<i64, u32>> = FxHashMap::default();

    while let Some(r) = rows.next()? {
        let occ_id: i64 = r.get(0)?;
        let hit_file_id: i64 = r.get(1)?;
        let hit_file_path: String = r.get(2)?;
        let line: i32 = r.get(3)?;
        let col: i32 = r.get(4)?;
        let is_def: i32 = r.get(5)?;
        let block_id: i64 = r.get(6)?;
        let sim = if block_id == anchor_block {
            1.0
        } else {
            let bag = match bag_cache.get(&block_id) {
                Some(b) => b,
                None => {
                    let b = block_bag_bigram(db, block_id)?;
                    bag_cache.insert(block_id, b);
                    bag_cache.get(&block_id).expect("just inserted")
                }
            };
            cosine(&anchor_bag, bag)
        };
        if sim >= threshold {
            hits.push(OccHit {
                occ_id,
                file_id: hit_file_id,
                file_path: hit_file_path,
                line,
                col,
                is_def: is_def != 0,
                block_id,
                similarity: sim,
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

/// goto_def(raw_name, anchor_file, anchor_line) — the is_def=1 occurrence in the
/// find_references cluster of the anchor. If multiple defs exist in the cluster,
/// returns the one with the highest similarity (typically the anchor's own def).
pub fn goto_def(
    db: &Connection,
    raw_name: &str,
    anchor_file: &str,
    anchor_line: i32,
) -> rusqlite::Result<Option<OccHit>> {
    // V35: When no anchor is provided, use centrality-ranked candidate
    if anchor_file.is_empty() && anchor_line <= 0 {
        let candidates = crate::type_flow::top_candidate_definitions(db, raw_name);
        if let Some((fp, ln, _)) = candidates.into_iter().next() {
            return Ok(Some(OccHit {
                occ_id: 0, file_id: 0,
                file_path: fp, line: ln, col: 0,
                is_def: true, block_id: 0, similarity: 1.0,
            }));
        }
    }
    let refs = find_references(db, raw_name, anchor_file, anchor_line, 0.0)?;
    // Prefer the def within the anchor's own cluster (similarity == 1.0 for anchor-block).
    // If multiple, pick by similarity desc, then by file (anchor file preferred).
    let mut best: Option<OccHit> = None;
    // P7-1: look up anchor file's block_id once (uses idx_block_range now).
    // find_references already ran this query internally — but the result isn't
    // returned. For goto_def we only need the anchor's block_id for comparison.
    let anchor_file_id: Option<i64> = db.query_row(
        "SELECT id FROM file_map WHERE file_path = ?1",
        params![anchor_file],
        |r| r.get(0),
    ).ok(); // GUARDED: intentional — None means file not indexed, handled below
    let anchor_block_id: i64 = anchor_file_id
        .and_then(|fid| block_id_at(db, fid, anchor_line).ok().flatten())
        .unwrap_or(0);
    for hit in refs {
        if hit.is_def {
            match &best {
                None => best = Some(hit.clone()),
                Some(b) => {
                    // P7-1: use hit.block_id directly (already populated by find_references)
                    // instead of re-querying block_id_at per hit. Each block_id_at call
                    // does a SELECT from the block table — with D def hits this was 2D queries.
                    let hit_is_anchor_block = hit.block_id == anchor_block_id;
                    let b_is_anchor_block = b.block_id == anchor_block_id;
                    let hit_score = (if hit_is_anchor_block { 1.0 } else { 0.0 }) + hit.similarity;
                    let b_score = (if b_is_anchor_block { 1.0 } else { 0.0 }) + b.similarity;
                    if hit_score > b_score {
                        best = Some(hit.clone());
                    }
                }
            }
        }
    }
    Ok(best)
}

/// symbol_callgraph(raw_name, anchor_file, anchor_line, threshold) — phrases whose
/// blocks overlap the anchor's block (above threshold cosine similarity), grouped
/// by phrase. Returns (phrase_stem, [OccHit]) pairs sorted by max similarity desc.
///
/// Grammar-free equivalent of "what symbols does this function call/use".
pub fn symbol_callgraph(
    db: &Connection,
    raw_name: &str,
    anchor_file: &str,
    anchor_line: i32,
    threshold: f32,
) -> rusqlite::Result<Vec<(String, Vec<OccHit>)>> {
    let anchor_bag = match block_id_for(db, anchor_file, anchor_line)? {
        Some(b) => block_bag(db, b)?,
        None => return Ok(vec![]),
    };
    if anchor_bag.is_empty() { return Ok(vec![]); }

    // For each phrase in the anchor's bag (excluding the anchor stem itself), find all
    // occurrences whose block-bag overlaps the anchor's bag at >= threshold.
    let anchor_stem = stem(raw_name);
    let mut result: FxHashMap<String, Vec<OccHit>> = FxHashMap::default();
    let mut max_sim: FxHashMap<String, f32> = FxHashMap::default();

    for &pid in anchor_bag.keys() {
        let phrase_stem: String = db.query_row(
            "SELECT phrase FROM phrases WHERE id = ?1", params![pid], |r| r.get(0),
        )?;
        if phrase_stem == anchor_stem { continue; }
        // For every occurrence of this phrase, compute cosine of its block bag vs anchor bag.
        let mut stmt = db.prepare_cached(
            "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
             FROM occurrence o JOIN file_map f ON f.id = o.file_id
             WHERE o.phrase_id = ?1",
        )?;
        let mut rows = stmt.query(params![pid])?;
        let mut bag_cache: FxHashMap<i64, FxHashMap<i64, u32>> = FxHashMap::default();
        while let Some(r) = rows.next()? {
            let occ_id: i64 = r.get(0)?;
            let hit_file_id: i64 = r.get(1)?;
            let hit_file_path: String = r.get(2)?;
            let line: i32 = r.get(3)?;
            let col: i32 = r.get(4)?;
            let is_def: i32 = r.get(5)?;
            let block_id: i64 = r.get(6)?;
            let bag = match bag_cache.get(&block_id) {
                Some(b) => b,
                None => {
                    let b = block_bag(db, block_id)?;
                    bag_cache.insert(block_id, b);
                    bag_cache.get(&block_id).expect("just inserted")
                }
            };
            let raw_sim = cosine(&anchor_bag, bag);
            // H1: boost production code over test fixtures. Test files typically
            // contain "test", "tests", or tokio-specific "loom" in the path.
            // Apply a +15% similarity boost to non-test paths so they rank higher.
            // De-duplicate: use the higher of raw and boosted for sorting/display.
            let is_test_path = hit_file_path.split('/').any(|seg| {
                seg == "test" || seg == "tests" || seg == "testing" || seg == "loom"
                    || seg.starts_with("test_") || seg.ends_with("_test")
            });
            let sim = if is_test_path {
                raw_sim * 0.85 // penalize test fixtures
            } else {
                raw_sim
            };
            if sim >= threshold {
                let hit = OccHit {
                    occ_id, file_id: hit_file_id, file_path: hit_file_path,
                    line, col, is_def: is_def != 0, block_id, similarity: sim,
                };
                let entry = result.entry(phrase_stem.clone()).or_default();
                entry.push(hit);
                let cur = max_sim.get(&phrase_stem).copied().unwrap_or(0.0);
                if sim > cur { max_sim.insert(phrase_stem.clone(), sim); }
            }
        }
    }

    let mut out: Vec<(String, Vec<OccHit>)> = result.into_iter().collect();
    out.sort_by(|a, b| {
        let b_sim = max_sim.get(&b.0).copied().unwrap_or(0.0);
        let a_sim = max_sim.get(&a.0).copied().unwrap_or(0.0);
        b_sim.partial_cmp(&a_sim)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(out)
}

fn block_id_for(db: &Connection, file_path: &str, line: i32) -> rusqlite::Result<Option<i64>> {
    let file_id = match file_id_for(db, file_path)? {
        Some(id) => id,
        None => return Ok(None),
    };
    block_id_at(db, file_id, line)
}

/// scope(raw_name, anchor_file, anchor_line) — min and max line of any occurrence of
/// `raw_name` within the anchor's block. Equivalent to "live range" of the identifier
/// within its enclosing scope. Returns (min_line, max_line, count) or None if the
/// phrase doesn't occur in the anchor's block.
pub fn scope(
    db: &Connection,
    raw_name: &str,
    anchor_file: &str,
    anchor_line: i32,
) -> rusqlite::Result<Option<(i32, i32, u32)>> {
    let phrase_id = match phrase_id_for(db, raw_name)? {
        Some(id) => id,
        None => return Ok(None),
    };
    let block_id = match block_id_for(db, anchor_file, anchor_line)? {
        Some(id) => id,
        None => return Ok(None),
    };
    let mut stmt = db.prepare_cached(
        "SELECT MIN(line), MAX(line), COUNT(*) FROM occurrence WHERE phrase_id = ?1 AND block_id = ?2",
    )?;
    let mut rows = stmt.query(params![phrase_id, block_id])?;
    if let Some(r) = rows.next()? {
        let min: Option<i32> = r.get(0)?;
        let max: Option<i32> = r.get(1)?;
        let cnt: i64 = r.get(2)?;
        if let (Some(lo), Some(hi)) = (min, max) {
            return Ok(Some((lo, hi, cnt as u32)));
        }
    }
    Ok(None)
}

/// Well-known entry-point names that are public-API by convention even when no
/// other code references them. These are excluded from dead-symbol reporting.
fn is_likely_entry_point(name: &str) -> bool {
    // A6: phrases are porter-stemmed to lowercase, so only lowercase patterns match.
    // Removed "Main"/"Test"/"ends_with Test" since they never match stored phrases.
    matches!(name, "main" | "__init__" | "__main__" | "setup" | "teardown"
        | "beforeeach" | "aftereach" | "setupclass" | "teardownclass"
        | "module_exit" | "module_init" | "dllmain" | "wmain" | "tmain")
    || name.starts_with("test_")
    || name.ends_with("_test")
}

/// dead_symbols() — defined occurrences with zero inbound find-references from any
/// OTHER block (within the anchor's own block, references are free — that's just the
/// definition using itself). Returns up to `limit` (phrase, file, line, col) tuples.
/// Grammar-free equivalent of "unused symbols".
pub fn dead_symbols(db: &Connection, limit: usize, path_filter: Option<&str>, functions_only: bool) -> rusqlite::Result<Vec<(String, String, i32, i32)>> {
    // Find every is_def=1 occurrence where the phrase has no occurrences in any
    // block OTHER than this occurrence's own block. That's the "no inbound from
    // other blocks" criterion.
    // V13: path_filter scopes to a module (e.g., "io/util"). functions_only
    // restricts to tag=1 (function definitions).
    let path_pattern = path_filter.map(|p| {
        // Normalize: strip leading/trailing slashes, ensure it's a prefix match.
        // Handle both absolute (`/tmp/corpus/src/`) and relative (`src/`) forms.
        let p = p.trim_start_matches('/').trim_end_matches('/');
        if p.is_empty() {
            "%".to_string()
        } else if p.starts_with("tmp/") || p.starts_with("home/") || p.starts_with("Users/") || p.starts_with("usr/") {
            // Absolute-ish path: match anywhere the normalized tail appears.
            // file_map stores absolute paths (e.g. /tmp/corpus/src/augment.rs).
            // Filter "src" -> matches ".../src/...". Filter "tmp/corpus/src" ->
            // matches the tail. Drop the leading mount component.
            let tail = p.split('/').skip(2).collect::<Vec<_>>().join("/");
            if tail.is_empty() { "%".to_string() } else { format!("%{tail}%") }
        } else {
            format!("%/{}%", p)
        }
    });
    let sql = if path_filter.is_some() && functions_only {
        "SELECT o.phrase_id, f.file_path, o.line, o.col, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id
         WHERE o.is_def = 1 AND o.tag = 1 AND f.is_source = 1 AND f.file_path LIKE ?1
         ORDER BY (o.tag = 1) DESC, LENGTH(f.file_path) DESC, o.occ_id"
    } else if path_filter.is_some() {
        "SELECT o.phrase_id, f.file_path, o.line, o.col, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id
         WHERE o.is_def = 1 AND f.is_source = 1 AND f.file_path LIKE ?1
         ORDER BY (o.tag = 1) DESC, LENGTH(f.file_path) DESC, o.occ_id"
    } else if functions_only {
        "SELECT o.phrase_id, f.file_path, o.line, o.col, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id
         WHERE o.is_def = 1 AND o.tag = 1 AND f.is_source = 1
         ORDER BY (o.tag = 1) DESC, LENGTH(f.file_path) DESC, o.occ_id"
    } else {
        "SELECT o.phrase_id, f.file_path, o.line, o.col, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id
         WHERE o.is_def = 1 AND f.is_source = 1
         ORDER BY (o.tag = 1) DESC, LENGTH(f.file_path) DESC, o.occ_id"
    };
    let mut stmt = db.prepare_cached(sql)?;
    let mut rows = match &path_pattern {
        Some(pat) => stmt.query(params![pat])?,
        None => stmt.query([])?,
    };
    let mut dead = Vec::new();
    // For efficiency, precompute phrase -> distinct-block-count map for the phrases we touch.
    let mut distinct_blocks_cache: FxHashMap<i64, u32> = FxHashMap::default();

    while let Some(r) = rows.next()? {
        if dead.len() >= limit { break; }
        let phrase_id: i64 = r.get(0)?;
        let file_path: String = r.get(1)?;
        let line: i32 = r.get(2)?;
        let col: i32 = r.get(3)?;
        let block_id: i64 = r.get(4)?;
        let distinct = match distinct_blocks_cache.get(&phrase_id) {
            Some(v) => *v,
            None => {
                let v: i64 = db.query_row(
                    "SELECT COUNT(DISTINCT block_id) FROM occurrence WHERE phrase_id = ?1",
                    params![phrase_id], |r| r.get(0),
                )?;
                distinct_blocks_cache.insert(phrase_id, v as u32);
                v as u32
            }
        };
        if distinct <= 1 {
            // Only seen in one block (its own) — and it's a def — so no inbound from elsewhere.
            // Also fetch the raw stem for display.
            let phrase: String = db.query_row(
                "SELECT phrase FROM phrases WHERE id = ?1", params![phrase_id], |r| r.get(0),
            )?;
            // Skip well-known entry points that are public-API by convention
            // even when no other code references them.
            if !is_likely_entry_point(&phrase) {
                dead.push((phrase, file_path, line, col));
            }
        }
        let _ = block_id; // used implicitly via distinct count
    }
    Ok(dead)
}

/// Extract the stem(s) immediately to the left of col on line_text.
/// Returns the raw left-token string before further tokenization/splitting.
/// E.g. for "self.poll(cx)" at col 5 (pointing at "poll"), returns "self".
/// For "inner.poll(cx)" at col 6, returns "inner".
fn left_context_stems(line_text: &str, col: usize) -> Vec<String> {
    // Tokenize chars before col, splitting on non-alphanumeric.
    let before: String = line_text.chars().take(col).collect();
    // Walk backwards from end of 'before' to catch the token right before col.
    let mut token = String::new();
    for ch in before.chars().rev() {
        if ch.is_alphanumeric() || ch == '_' {
            token.push(ch);
        } else if !token.is_empty() {
            break;
        }
    }
    if token.is_empty() { return vec![]; }
    vec![token.chars().rev().collect()]
}

/// `find_references_role` — Phase 12 (role vectors).
///
/// Combines NCD structural similarity with role-vector cosine (the 1-2 tokens
/// immediately left of each occurrence). The role vector captures how a symbol
/// is USED (self.poll vs fn poll vs inner.poll).
pub fn find_references_role(
    db: &Connection, raw_name: &str, anchor_file: &str, anchor_line: i32,
    threshold: f32, anchor_col: i32, alpha: f32,
) -> rusqlite::Result<Vec<OccHit>> {
    use crate::compat::{extract_window, ncd_similarity};

    let phrase_id = match phrase_id_for(db, raw_name)? { Some(id) => id, None => return Ok(vec![]) };
    let _file_id = match file_id_for(db, anchor_file)? { Some(id) => id, None => return Ok(vec![]) };

    // Read anchor file and extract anchor role + NCD window.
    // V54: prefer file_meta cache (Arc<FileMeta>.lines) over disk re-read.
    let anchor_lines: Vec<String> = crate::file_meta::get(anchor_file)
        .map(|m| m.lines.clone())
        .or_else(|| std::fs::read_to_string(anchor_file).ok().map(|c| c.lines().map(|l| l.to_string()).collect()))
        .unwrap_or_default();
    if anchor_lines.is_empty() {
        return Ok(vec![]);
    }
    let anchor_line_text = anchor_lines.get((anchor_line.max(0) - 1) as usize).cloned().unwrap_or_default();
    // Auto-detect column if not provided (anchor_col=0 means "find on line").
    let actual_col = if anchor_col <= 0 {
        anchor_line_text.find(raw_name).unwrap_or(0)
    } else {
        anchor_col as usize
    };
    let anchor_role = left_context_stems(&anchor_line_text, actual_col);
    let anchor_window = extract_window(&anchor_lines, anchor_line, 5);

    // All occurrences of the phrase.
    let mut stmt = db.prepare_cached(
        "SELECT o.occ_id, o.file_id, f.file_path, o.line, o.col, o.is_def, o.block_id
         FROM occurrence o JOIN file_map f ON f.id = o.file_id WHERE o.phrase_id = ?1 AND f.is_source = 1",
    )?;
    let mut rows = stmt.query(params![phrase_id])?;
    let mut hits = Vec::new();
    // P7-2: use file_meta cache (bounded LRU) instead of local unbounded HashMap.
    // The old code grew without bound across a corpus and did its own disk reads.

    while let Some(r) = rows.next()? {
        let occ_id: i64 = r.get(0)?;
        let hit_file_id: i64 = r.get(1)?;
        let hit_file_path: String = r.get(2)?;
        let line: i32 = r.get(3)?;
        let col: i32 = r.get(4)?;
        let is_def: i32 = r.get(5)?;
        let block_id: i64 = r.get(6)?;

        // P7-2: file_meta::get returns Arc<FileMeta> — Arc refcount bump per hit.
        // Only the single line at `line` is needed; file_meta lines are Vec<String>
        // so we can index directly without cloning the whole vec.
        let meta_arc = match crate::file_meta::get(&hit_file_path) {
            Some(m) => m,
            None => continue,
        };
        let cand_lines: &[String] = &meta_arc.lines;

        // Role cosine.
        let c_line_text = cand_lines.get(line as usize).cloned().unwrap_or_default();
        let c_role = left_context_stems(&c_line_text, col as usize);
        let role_sim = if anchor_role.is_empty() || c_role.is_empty() {
            0.0
        } else {
            // Jaccard over role stems.
            let common = anchor_role.iter().filter(|s| c_role.contains(s)).count();
            common as f32 / (anchor_role.len() + c_role.len() - common) as f32
        };

        // NCD similarity.
        let c_window = extract_window(cand_lines, line, 5);
        let ncd = ncd_similarity(&anchor_window, &c_window);

        // Combined.
        let sim = alpha * role_sim + (1.0 - alpha) * ncd;

        if sim >= threshold {
            hits.push(OccHit {
                occ_id, file_id: hit_file_id, file_path: hit_file_path,
                line, col, is_def: is_def != 0, block_id, similarity: sim,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::DetectBlocksOut;
    use crate::ingest::detect_blocks;

    #[test]
    fn test_cosine_identical() {
        let mut a = FxHashMap::default();
        a.insert(1, 3);
        a.insert(2, 1);
        let b = a.clone();
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_orthogonal() {
        let mut a = FxHashMap::default();
        a.insert(1, 5);
        let mut b = FxHashMap::default();
        b.insert(2, 5);
        assert!(cosine(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_partial() {
        let mut a = FxHashMap::default();
        a.insert(1, 3);
        a.insert(2, 2);
        let mut b = FxHashMap::default();
        b.insert(1, 1);
        b.insert(2, 5);
        // dot = 3*1 + 2*5 = 13
        // |a| = sqrt(9+4) = sqrt(13)
        // |b| = sqrt(1+25) = sqrt(26)
        let s = cosine(&a, &b);
        let expected = 13.0 / ((13.0f64).sqrt() * (26.0f64).sqrt());
        assert!((s as f64 - expected).abs() < 1e-6, "{} vs {}", s, expected);
    }

    #[test]
    fn test_detect_blocks_basic() {
        // Grammar-free block detection treats "}" at indent 0 as the same block as
        // "fn a() {" because indentation rules only see indent >= leading. Real scope
        // boundaries require parser awareness — by design we don't have that.
        let lines = vec![
            "fn a() {",      // 0
            "    let x = 1;", // 1
            "    let y = 2;", // 2
            "}",              // 3 same indent 0 → same block
            "fn b() {",      // 4 same indent 0 → same block
            "    let z = 3;", // 5
        ];
        let DetectBlocksOut { blocks, line_block: lb } = detect_blocks(&lines);
        assert_eq!(blocks.len(), 1, "grammar-free: same-indent runs are one block");
        assert_eq!(blocks[0].start_line, 0);
        assert_eq!(blocks[0].end_line, 5);
        assert_eq!(lb[5], 0);
    }

    #[test]
    fn test_detect_blocks_blank_separator() {
        let lines = vec![
            "let x = 1;",
            "",
            "let y = 2;",
        ];
        let DetectBlocksOut { blocks, line_block: _lb } = detect_blocks(&lines);
        assert_eq!(blocks.len(), 2, "blank line should split blocks");
        assert_eq!(blocks[0].start_line, 0);
        assert_eq!(blocks[1].start_line, 2);
    }
}