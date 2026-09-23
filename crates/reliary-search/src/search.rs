//! FTS5 query and BM25 scoring against the inverted index.
use rusqlite::{params, Connection, OptionalExtension};
use rustc_hash::FxHashMap;

pub struct SearchResult {
    pub file: String,
    pub score: f32,
    pub line: Option<u32>,
    pub zone: Option<u8>,
}

/// Search the index. Returns top-N results sorted by BM25 score.
/// Arc 32: replaced FTS5 trigram tokenizer with LIKE on phrases table.
/// FTS5 overhead (~450 KiB) exceeded its benefit on small `phrases` tables.
pub fn search_fts5(db: &Connection, query: &str, top_n: usize) -> Vec<SearchResult> {
    // V57d: never-empty search. Primary: AND (all terms in one phrase).
    // Fallback 1: OR (union of per-term matches — phrases hold single
    // identifiers, so "pub struct Foo" (multi-word) can never AND-match).
    // Fallback 2: rarest term only (the most selective token).
    // Silent: the caller sees plain results either way.
    // V61: split on non-alphanumeric/underscore instead of keeping `-` in the
    // term — `foo-bar` never matched because scan_identifiers splits on `-`,
    // so no phrase contains it (AND/OR/rarest all returned empty despite
    // `foo` and `bar` being indexed).
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if terms.is_empty() { return vec![]; }

    // Primary: AND.
    let and_res = run_terms_query(db, &terms, JoinMode::And, top_n);
    if !and_res.is_empty() { return and_res; }

    // Fallback 1: OR (union).
    let or_res = run_terms_query(db, &terms, JoinMode::Or, top_n);
    if !or_res.is_empty() { return or_res; }

    // Fallback 2: rarest term (lowest document frequency = most selective).
    let rarest = rarest_term(db, &terms);
    if let Some(rt) = rarest {
        let rare_res = run_terms_query(db, &[rt], JoinMode::And, top_n);
        if !rare_res.is_empty() { return rare_res; }
    }

    vec![]
}

enum JoinMode { And, Or }

fn rarest_term(db: &Connection, terms: &[String]) -> Option<String> {
    let mut best: Option<(i64, String)> = None;
    for t in terms {
        let like = format!("%{}%", t.replace('_', "\\_"));
        let df: i64 = db
            .query_row("SELECT COUNT(*) FROM phrases WHERE phrase LIKE ?1 ESCAPE '\\'", params![&like], |r| r.get(0))
            .unwrap_or(0);
        if df > 0
            && best.as_ref().map(|(b, _)| df < *b).unwrap_or(true) {
                best = Some((df, t.clone()));
            }
    }
    best.map(|(_, t)| t)
}

/// M1: Path-based rank multiplier. Definitions in production source must
/// outrank the same identifier appearing in tests, bench scripts, docs and
/// fixtures. Demote (not exclude) so those files still appear when relevant.
///
/// V73: only the LAST 4 path segments are considered. Paths are absolute, so
/// an ancestor directory named `results`/`docs`/`tests` (e.g. a checkout under
/// `/tmp/results/…`) previously demoted every file beneath it.
fn path_rank(path: &str) -> f32 {
    let lower = path.to_ascii_lowercase();
    let all: Vec<&str> = lower.split('/').filter(|s| !s.is_empty()).collect();
    let tail_start = all.len().saturating_sub(4);
    let segs = &all[tail_start..];
    let tail: String = segs.join("/");
    // Tests (path segments or filename conventions) within the tail.
    if crate::impact::is_test_path(&tail) {
        return 0.35;
    }
    // Bench harnesses, examples, fixtures, archived plans.
    if segs.iter().any(|s| {
        *s == "bench" || *s == "benches" || *s == "examples" || *s == "fixtures"
            || *s == "archive" || *s == "results"
    }) {
        return 0.30;
    }
    // Docs and config trees (markdown plans, etc.).
    if segs.iter().any(|s| *s == "docs" || *s == "doc") {
        return 0.40;
    }
    1.0
}

/// M1: Definition boost. A file containing at least one definition of the
/// phrase ranks above files that merely use it. This is what makes
/// "where is X" resolve to the definition instead of the busiest caller.
const DEF_BOOST: f32 = 3.0;

fn run_terms_query(db: &Connection, terms: &[String], mode: JoinMode, top_n: usize) -> Vec<SearchResult> {
    if terms.is_empty() { return vec![]; }
    // Escape `_` so it's a literal in LIKE (not a single-char wildcard).
    let like_terms: Vec<String> = terms.iter().map(|t| {
        format!("%{}%", t.replace('_', "\\_"))
    }).collect();
    let sep = match mode { JoinMode::And => " AND ", JoinMode::Or => " OR " };
    let conditions: String = like_terms.iter()
        .map(|_| "p.phrase LIKE ? ESCAPE '\\'")
        .collect::<Vec<_>>().join(sep);
    let total_files: f64 = db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0)).unwrap_or(1.0);
    let avg_tokens_raw: f64 = db.query_row("SELECT AVG(token_len) FROM file_stats", [], |r| r.get(0)).unwrap_or(1.0);
    let avg_tokens: f64 = if avg_tokens_raw == 0.0 { 1.0 } else { avg_tokens_raw };

    let sql = format!(
        "SELECT p.id as phrase_id, occ.file_blob
         FROM phrases p JOIN phrase_occ occ ON occ.phrase_id = p.id
         WHERE {}
         ORDER BY p.id",
        conditions
    );
    let mut stmt = match db.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let like_params: Vec<Box<dyn rusqlite::ToSql>> = like_terms
        .into_iter()
        .map(|t| Box::new(t) as Box<dyn rusqlite::ToSql>)
        .collect();
    let like_refs: Vec<&dyn rusqlite::ToSql> = like_params.iter().map(|b| b.as_ref()).collect();

    let mut phrase_rows: Vec<(i64, Vec<u8>)> = Vec::new();
    if let Ok(rows) = stmt.query_map(like_refs.as_slice(), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
    }) {
        for row in rows.flatten() { phrase_rows.push(row); }
    }
    drop(stmt);
    drop(like_params);
    if phrase_rows.is_empty() { return vec![]; }

    let mut needed_ids_set: rustc_hash::FxHashSet<i64> = rustc_hash::FxHashSet::default();
    for (_, ref blob) in &phrase_rows {
        for (fid, _) in crate::schema::unpack_file_blob(blob) {
            needed_ids_set.insert(fid);
        }
    }
    let needed_ids: Vec<i64> = needed_ids_set.into_iter().collect();
    if needed_ids.is_empty() { return vec![]; }

    let placeholders = std::iter::repeat_n("?", needed_ids.len()).collect::<Vec<_>>().join(",");
    let max_fid: i64 = db.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM file_map", [], |r| r.get(0),
    ).unwrap_or(0);
    let mut file_map: Vec<Option<(String, f64, bool)>> = vec![None; (max_fid + 1) as usize];
    let fm_sql = format!(
        "SELECT fm.id, fm.file_path, COALESCE(fs.token_len, 50) as token_len, fm.is_source
         FROM file_map fm LEFT JOIN file_stats fs ON fs.file_id = fm.id
         WHERE fm.id IN ({})", placeholders
    );
    let mut fm_stmt = match db.prepare(&fm_sql) { Ok(s) => s, Err(_) => return vec![] };
    {
        let param_refs: Vec<&dyn rusqlite::ToSql> = needed_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        if let Ok(rows) = fm_stmt.query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, f64>(2)?, row.get::<_, bool>(3)?))
        }) {
            for row in rows.flatten() {
                let (id, path, tl, src) = row;
                let idx = id as usize;
                if idx < file_map.len() { file_map[idx] = Some((path, tl, src)); }
            }
        }
    }

    let mut results: Vec<SearchResult> = Vec::new();
    let mut file_index: rustc_hash::FxHashMap<String, usize> = rustc_hash::FxHashMap::default();
    for (_, file_blob) in &phrase_rows {
        let entries: Vec<(i64, u8)> = crate::schema::unpack_file_blob(file_blob).collect();
        if entries.is_empty() { continue; }
        let doc_freq = entries.len() as f64;
        for &(fid, flags) in &entries {
            let idx = fid as usize;
            let (file_path, token_len, is_source) = match file_map.get(idx).and_then(|o| o.as_ref()) {
                Some((p, t, s)) => (p.clone(), *t, *s),
                None => continue,
            };
            if !is_source { continue; }
            let tf = (crate::schema::unpack_count(flags) as f64).max(1.0);
            let zone = Some(crate::schema::unpack_zone_int(flags) as u8);
            let idf = crate::bm25_idf(total_files as f32, doc_freq as f32);
            let base = crate::bm25_score(idf, tf as f32, token_len as f32, avg_tokens as f32);
            // M1: definition-first ranking. Files where the phrase is defined
            // outrank files that merely reference it; production paths outrank
            // tests/bench/docs.
            let is_def = crate::schema::unpack_is_def(flags) > 0;
            let rank = path_rank(&file_path);
            let def_mult = if is_def { DEF_BOOST } else { 1.0 };
            let score = base * rank * def_mult;
            if let Some(&idx) = file_index.get(&file_path) {
                results[idx].score += score;
                if is_def { results[idx].zone = Some(1); }
                else { results[idx].zone = zone; }
            } else {
                file_index.insert(file_path.clone(), results.len());
                results.push(SearchResult { file: file_path, score, line: None, zone: if is_def { Some(1) } else { zone } });
            }
        }
    }

    // V23: Quale reranking.
    let terms_lower: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    quale_rerank(db, &terms_lower, &mut results);

    // V55: ColBERT-style late interaction (kill-switch RELIARY_LATE=0).
    if std::env::var("RELIARY_LATE").map(|v| v != "0").unwrap_or(true) {
        late_interaction_rerank(db, &terms_lower, &mut results);
    }

    let k = top_n.min(results.len());
    if k > 0 {
        results.select_nth_unstable_by(k.saturating_sub(1), |a, b| {
            b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.file.cmp(&b.file))
        });
    }
    results.truncate(k);
    results.sort_by(|a, b| {
        b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file.cmp(&b.file))
    });
    results
}

pub fn who_calls(db: &Connection, identifier: &str, exclude_file: &str) -> Vec<(String, u64)> {
    let stemmed = crate::porter_stem(&identifier.to_lowercase());
    let phrase_id: Option<i64> = match db.query_row(
        "SELECT id FROM phrases WHERE phrase = ?1",
        params![stemmed],
        |r| r.get(0),
    ).optional() {
        Ok(v) => v,
        Err(e) => { eprintln!("[search] phrase lookup failed for {:?}: {}", stemmed, e); None }
    };
    let phrase_id = match phrase_id {
        Some(id) => id,
        None => return vec![],
    };

    // Arc 37 schema v4: fetch file_blob, unpack to get file_ids, then look up paths.
    let file_blob: Option<Vec<u8>> = match db.query_row(
        "SELECT file_blob FROM phrase_occ WHERE phrase_id = ?1",
        params![phrase_id],
        |r| r.get(0),
    ).optional() {
        Ok(v) => v,
        Err(e) => { eprintln!("[search] phrase blob read failed for id {}: {}", phrase_id, e); None }
    };
    let file_blob = match file_blob { Some(b) => b, None => return vec![] };

    // P6-4: unpack once and capture per-file counts from flags.
    // unpack_count(flags) gives the actual occurrence count per file.
    let entries: Vec<(i64, u64)> = crate::schema::unpack_file_blob(&file_blob)
        .map(|(fid, flags)| (fid, crate::schema::unpack_count(flags) as u64))
        .collect();
    if entries.is_empty() { return vec![]; }
    let file_ids: Vec<i64> = entries.iter().map(|(fid, _)| *fid).collect();

    let placeholders = std::iter::repeat_n("?", file_ids.len()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, file_path FROM file_map WHERE id IN ({}) ORDER BY id",
        placeholders
    );
    let mut stmt = match db.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let mut params_v: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(file_ids.len());
    for fid in &file_ids {
        params_v.push(Box::new(*fid));
    }
    let params_refs: Vec<&dyn rusqlite::ToSql> = params_v.iter().map(|b| b.as_ref()).collect();
    let mut results: Vec<(String, u64)> = Vec::new();
    if let Ok(rows) = stmt.query_map(params_refs.as_slice(), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    }) {
        for row in rows.flatten() {
            let (fid, path) = row;
            if path != exclude_file {
                // P6-4: use actual unpack_count instead of hardcoded 1.
                let count = entries.iter()
                    .find(|(f, _)| *f == fid)
                    .map(|(_, c)| *c)
                    .unwrap_or(1);
                results.push((path, count));
            }
        }
    }
    results
}

/// V23: Quale reranking — boost files that DEFINE query terms.
/// A file that defines "block_on" (is_def=1) ranks above one that merely
/// mentions it in comments (is_def=0). Ported from stria's quale_rerank().
/// Grammar-free: pure math on the existing phrase index.
///
/// V61: the def-count boost queried `po.flags` — a column that does NOT
/// exist in schema v4 (phrase_occ = phrase_id, file_blob only). Every
/// prepare failed silently, so the boost was dead AND the score-crushing
/// below (bm25_norm * 0.01) ran anyway. Only the proximity bonus at the
/// end was live. Stripped to the live part.
pub fn quale_rerank(db: &Connection, terms: &[String], results: &mut [SearchResult]) {
    if results.is_empty() || terms.is_empty() {
        return;
    }
    // V23: Proximity bonus — boost files where query terms cluster together.
    apply_proximity_bonus(db, terms, results);
}

/// ColBERT-style late interaction, grammar-free.
///
/// ColBERT's insight: score each query token against its BEST match in the
/// document (MaxSim), then sum. This captures soft matches (typos, stems,
/// compound identifiers) that exact BM25 term overlap misses.
///
/// Our port has no embeddings — we use the phrase index + a grammar-free
/// string-similarity function:
///   for each query term t:
///     for each candidate file f:
///       best(t, f) = max over phrases p in f of
///                    term_similarity(t, p) * occurrence_weight(p, f)
///   file_score = Σ_t best(t, f)   (ColBERT late-interaction aggregation)
///
/// The result is blended into the existing score (quale/proximity already
/// applied): score += li_norm * LATE_WEIGHT.
pub fn late_interaction_rerank(
    db: &Connection,
    terms: &[String],
    results: &mut [SearchResult],
) {
    if results.is_empty() || terms.is_empty() {
        return;
    }
    // Only rerank the current top candidates (the candidate set is already
    // BM25-filtered). For each, find its phrases via phrase_occ LIKE scan.
    const LATE_WEIGHT: f32 = 0.45;

    // Batch: for each query term, load (phrase, blob) pairs via LIKE, then
    // unpack the blob in Rust — phrase_occ has NO file_id column (file_ids
    // are packed in file_blob as varint(file_id) || flags entries).
    // Map: file -> Vec<(phrase, occurrence_weight)>.
    let mut file_phrases: FxHashMap<String, Vec<(String, f32)>> = FxHashMap::default();
    // file_id -> file_path, loaded lazily for the file_ids we actually hit.
    let mut path_cache: FxHashMap<i64, String> = FxHashMap::default();
    // V58 P6a: pre-resolve candidate phrases via the in-memory index
    // (substring + prefix) so the SQL only fetches blobs for real matches.
    // Falls back to plain LIKE when the in-memory index is unavailable.
    // The phrase index is keyed by DB path; late_interaction_rerank only has
    // the connection, so resolve the path from file_map's canonical source —
    // the same ".reliary/index.sqlite" the MCP layer caches. Use a cheap
    // process-wide OnceLock mirror of the MCP cached path (set on first use).
    let db_path = crate::phrase_index::current_db_path().unwrap_or_default();
    let term_candidates: Vec<Vec<String>> =
        match crate::phrase_index::get_phrase_index(db, &db_path) {
            Some(index) => terms.iter().map(|t| index.substring_candidates(t, 200)).collect(),
            None => terms.iter().map(|t| vec![format!("%{}%", t)]).collect(),
        };
    for (_, cands) in terms.iter().zip(term_candidates.iter()) {
        let sql = "SELECT p.phrase, po.file_blob
             FROM phrase_occ po
             JOIN phrases p ON p.id = po.phrase_id
             WHERE p.phrase IN (SELECT value FROM json_each(?1))";
        let cand_json = serde_json::to_string(cands).unwrap_or_else(|_| "[]".into());
        if let Ok(mut stmt) = db.prepare_cached(sql) {
            if let Ok(rows) = stmt.query_map(params![&cand_json], |r| {
                let phrase: String = r.get(0)?;
                let blob: Vec<u8> = r.get(1)?;
                Ok((phrase, blob))
            }) {
                for row in rows.flatten() {
                    let (phrase, blob) = row;
                    // Unpack varint(file_id) || flags entries from the blob.
                    let mut pos = 0usize;
                    while pos < blob.len() {
                        let (file_id, n) = match crate::schema::decode_varint(&blob[pos..]) {
                            Some((id, n)) => (id, n),
                            None => break,
                        };
                        pos += n;
                        let flags = if pos < blob.len() { blob[pos] } else { 0 };
                        pos += 1;
                        let count = crate::schema::unpack_count(flags) as f32;
                        let is_def = crate::schema::unpack_is_def(flags);
                        let zone = crate::schema::unpack_zone_int(flags) as f32;
                        let w = 1.0 + (is_def.max(0) as f32) * 1.5 + zone * 0.5 + (count.min(10.0) * 0.05);
                        let fp = if let Some(p) = path_cache.get(&file_id) {
                            p.clone()
                        } else {
                            match db.query_row(
                                "SELECT file_path FROM file_map WHERE id = ?1",
                                params![file_id],
                                |r| r.get::<_, String>(0),
                            ) {
                                Ok(p) => { path_cache.insert(file_id, p.clone()); p }
                                Err(_) => continue,
                            }
                        };
                        file_phrases.entry(fp).or_default().push((phrase.clone(), w));
                    }
                }
            }
        }
    }

    // MaxSim per (term, file): max over matching phrases of sim * weight.
    let mut li_scores: FxHashMap<String, f32> = FxHashMap::default();
    for (file, phrases) in &file_phrases {
        let mut score = 0.0f32;
        for term in terms {
            let mut best = 0.0f32;
            for (phrase, w) in phrases {
                let sim = term_similarity(term, phrase);
                let s = sim * w;
                if s > best {
                    best = s;
                }
            }
            score += best;
        }
        if score > 0.0 {
            li_scores.insert(file.clone(), score);
        }
    }

    // Normalize + blend into existing scores.
    let max_li = li_scores.values().copied().fold(0.0f32, f32::max);
    if max_li <= 0.0 {
        return;
    }
    let max_cur = results.iter().map(|r| r.score).fold(0.0f32, f32::max);
    for r in results.iter_mut() {
        let li = li_scores.get(&r.file).copied().unwrap_or(0.0) / max_li;
        let cur_norm = if max_cur > 0.0 { r.score / max_cur } else { 0.0 };
        r.score = cur_norm + li * LATE_WEIGHT;
    }
}

/// Grammar-free term-to-phrase similarity (0.0..1.0).
/// Pure string math on the token level — no embeddings, no dictionaries.
fn term_similarity(term: &str, phrase: &str) -> f32 {
    let t = term.as_bytes();
    let p = phrase.as_bytes();
    if t == p {
        return 1.0;
    }
    // Exact after porter-stem (the index stores stemmed phrases).
    let stemmed = crate::porter_stem(term);
    if stemmed.as_bytes() == p {
        return 0.95;
    }
    // Prefix overlap: one is a prefix of the other (compound identifiers).
    if t.len() >= 3 && p.len() >= 3 {
        let min_len = t.len().min(p.len());
        if t[..min_len] == p[..min_len] {
            return 0.7 + 0.2 * (min_len as f32 / t.len().max(p.len()) as f32);
        }
    }
    // Containment (phrase contains term or vice versa — snake_case compounds).
    if t.len() >= 3 && p.len() >= 3
        && (phrase.contains(term) || term.contains(phrase)) {
            return 0.6;
        }
    // Edit distance <= 2 (typos, pluralization).
    if edit_distance(term, phrase) <= 2 {
        return 0.5;
    }
    // Shared bigrams — weak soft match.
    let shared = shared_bigrams(term, phrase);
    if shared > 0.0 {
        return 0.3 + 0.15 * shared;
    }
    0.0
}

/// Levenshtein distance (bounded at 3 for speed).
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len().abs_diff(b.len()) > 2 {
        return 3;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Fraction of bigrams shared between two strings (0.0..1.0).
fn shared_bigrams(a: &str, b: &str) -> f32 {
    let mut a_bg: Vec<(u8, u8)> = a.as_bytes().windows(2).map(|w| (w[0], w[1])).collect();
    let mut b_bg: Vec<(u8, u8)> = b.as_bytes().windows(2).map(|w| (w[0], w[1])).collect();
    if a_bg.is_empty() || b_bg.is_empty() {
        return 0.0;
    }
    a_bg.sort_unstable();
    b_bg.sort_unstable();
    let mut i = 0usize;
    let mut j = 0usize;
    let mut shared = 0usize;
    while i < a_bg.len() && j < b_bg.len() {
        if a_bg[i] == b_bg[j] {
            shared += 1;
            i += 1;
            j += 1;
        } else if a_bg[i] < b_bg[j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    shared as f32 / a_bg.len().max(b_bg.len()) as f32
}

/// V23: Proximity bonus — when query terms appear within N lines of each other in
/// a file, that file gets a bonus. Helps q4 (call chain terms cluster in scheduler
/// files). Grammar-free: pure math on the occurrence table.
fn apply_proximity_bonus(db: &Connection, terms: &[String], results: &mut [SearchResult]) {
    if results.len() < 2 || terms.len() < 2 {
        return;
    }
    let max_gap = 50usize;
    let top_n = results.len().min(20);

    // For each top candidate, query line numbers for each term.
    for r in results.iter_mut().take(top_n) {
        let fid: Option<i64> = match db.query_row(
            "SELECT id FROM file_map WHERE file_path = ?1",
            [&r.file],
            |row| row.get(0),
        ).optional() {
            Ok(v) => v,
            Err(e) => { eprintln!("[search] file_map lookup failed for {}: {}", r.file, e); None }
        };
        let fid = match fid { Some(f) => f, None => continue };

        let mut line_sets: Vec<Vec<usize>> = Vec::with_capacity(terms.len());
        for term in terms {
            let mut stmt = match db.prepare(
                "SELECT o.line FROM occurrence o
                 JOIN phrases p ON p.id = o.phrase_id
                 WHERE o.file_id = ?1 AND p.phrase = ?2
                 LIMIT 50",
            ) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let lines: Vec<usize> = stmt.query_map(rusqlite::params![fid, term], |row| row.get::<_, i64>(0))
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).map(|n| n as usize).collect())
                .unwrap_or_default();
            if !lines.is_empty() {
                line_sets.push(lines);
            }
        }

        if line_sets.len() < 2 {
            continue;
        }
        // Compute proximity.
        let refs: Vec<&[usize]> = line_sets.iter().map(|v| v.as_slice()).collect();
        let bonus = proximity_bonus(&refs, max_gap);
        if bonus > 0.0 {
            r.score += (bonus * 2.0) as f32;
        }
    }
}

/// V23: Line-number proximity bonus (ported from stria).
/// For each pair of term line-sets, find min distance. If within max_gap,
/// contribute bonus = (max_gap - min_dist + 1) / max_gap. Average over pairs.
pub fn proximity_bonus(line_sets: &[&[usize]], max_gap: usize) -> f64 {
    if line_sets.len() < 2 {
        return 0.0;
    }
    let mut total = 0.0f64;
    let mut pairs = 0u32;
    for i in 0..line_sets.len() {
        for j in (i + 1)..line_sets.len() {
            let mut min_dist = usize::MAX;
            for &a in line_sets[i] {
                for &b in line_sets[j] {
                    let dist = a.abs_diff(b);
                    if dist < min_dist {
                        min_dist = dist;
                    }
                }
            }
            if min_dist <= max_gap {
                total += (max_gap - min_dist + 1) as f64 / max_gap as f64;
            }
            pairs += 1;
        }
    }
    if pairs == 0 { 0.0 } else { total / pairs as f64 * 0.5 }
}

/// Get index stats
pub fn get_index_stats(db: &Connection) -> (i64, i64, i64) {
    let files = db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0)).unwrap_or(0);
    let phrases = db.query_row("SELECT COUNT(*) FROM phrases", [], |r| r.get(0)).unwrap_or(0);
    let occs = db.query_row("SELECT COUNT(*) FROM phrase_occ", [], |r| r.get(0)).unwrap_or(0);
    (files, phrases, occs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quale_rerank_empty_results() {
        let db = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&db).unwrap();
        let mut results = vec![];
        quale_rerank(&db, &["test".to_string()], &mut results);
        assert!(results.is_empty());
    }

    #[test]
    fn quale_rerank_empty_terms() {
        let db = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&db).unwrap();
        let mut results = vec![SearchResult { file: "a.rs".into(), score: 1.0, line: None, zone: None }];
        quale_rerank(&db, &[], &mut results);
        assert_eq!(results[0].score, 1.0, "no terms = no change");
    }

    #[test]
    fn quale_rerank_short_terms_skipped() {
        let db = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&db).unwrap();
        let mut results = vec![SearchResult { file: "a.rs".into(), score: 1.0, line: None, zone: None }];
        quale_rerank(&db, &["ab".to_string()], &mut results);
        assert_eq!(results[0].score, 1.0, "terms < 3 chars skipped");
    }

    #[test]
    fn term_similarity_exact_and_fuzzy() {
        // Exact match.
        assert_eq!(term_similarity("consume", "consume"), 1.0);
        // Porter-stem match (index stores stemmed).
        let s = term_similarity("consuming", "consum");
        assert!(s >= 0.9, "stem match should be ~0.95, got {s}");
        // Compound prefix (snake_case).
        let s = term_similarity("block_on", "block_on_inner");
        assert!(s >= 0.7, "prefix match should be >= 0.7, got {s}");
        // Containment.
        let s = term_similarity("find_ref", "find_references");
        assert!(s >= 0.6, "containment should be >= 0.6, got {s}");
        // Typo (edit distance 1).
        let s = term_similarity("spawn", "spawn");
        assert_eq!(s, 1.0);
        let s = term_similarity("spwan", "spawn");
        assert!(s >= 0.5, "typo should be >= 0.5, got {s}");
        // Unrelated.
        let s = term_similarity("zebra", "consume");
        assert_eq!(s, 0.0, "unrelated terms = 0");
    }

    #[test]
    fn late_interaction_boosts_soft_match_file() {
        // Two files: one has exact "consume", one has "consume_inner" only.
        // The soft-match file should get a boost from late interaction even
        // though BM25 wouldn't score it (no exact term).
        let db = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&db).unwrap();
        // Insert phrases + occurrences for two files.
        let _ = db.execute_batch(
            "INSERT INTO file_map (file_path) VALUES ('a.rs'), ('b.rs');
             INSERT INTO phrases (id, phrase) VALUES (1, 'consume'), (2, 'consume_inner');
             INSERT INTO phrase_occ (phrase_id, file_blob)
               VALUES (1, x'0101'), (2, x'0201');
             INSERT INTO occurrence (phrase_id, file_id, line, col, is_def, block_id)
               VALUES (1, 1, 10, 1, 0, 1), (2, 2, 20, 1, 0, 1);",
        );
        let mut results = vec![
            SearchResult { file: "a.rs".into(), score: 1.0, line: None, zone: None },
            SearchResult { file: "b.rs".into(), score: 0.5, line: None, zone: None },
        ];
        let terms = vec!["consume".to_string()];
        late_interaction_rerank(&db, &terms, &mut results);
        // a.rs (exact) should still lead, but b.rs (soft) should gain.
        assert!(results[0].score >= results[1].score);
        assert!(results[1].score > 0.5, "soft-match file should be boosted above BM25 base");
    }
}
