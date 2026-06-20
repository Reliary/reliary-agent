//! FTS5 query and BM25 scoring against the inverted index.
use rusqlite::{params, Connection};

pub struct SearchResult {
    pub file: String,
    pub score: f64,
    pub line: Option<u32>,
    pub zone: Option<u8>,
}

/// Search the FTS5 index. Returns top-N results sorted by BM25 score.
pub fn search_fts5(db: &Connection, query: &str, top_n: usize) -> Vec<SearchResult> {
    let fts_query = query
        .split_whitespace()
        .filter(|t| t.len() >= 2)
        .map(|t| format!("\"{}\"", t))
        .collect::<Vec<_>>()
        .join(" OR ");

    if fts_query.is_empty() { return vec![]; }

    let total_files: f64 = db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0)).unwrap_or(1.0);
    let avg_tokens: f64 = db.query_row("SELECT AVG(token_len) FROM file_stats", [], |r| r.get(0)).unwrap_or(1.0);

    // Single query with all joins — eliminates N+1 per-row subqueries.
    let sql = "
        SELECT f.file_path, p.id as phrase_id, occ.flags, occ.line_nos,
               (SELECT COUNT(*) FROM phrase_occ WHERE phrase_id = p.id) as doc_freq,
               COUNT(occ2.phrase_id) as tf_in_file,
               COALESCE(fs.token_len, 50) as token_len
        FROM phrases_fts fts
        JOIN phrases p ON fts.rowid = p.id
        JOIN phrase_occ occ ON occ.phrase_id = p.id
        JOIN file_map f ON occ.file_id = f.id
        LEFT JOIN phrase_occ occ2 ON occ2.phrase_id = p.id AND occ2.file_id = occ.file_id
        LEFT JOIN file_stats fs ON fs.file_id = f.id
        WHERE phrases_fts MATCH ?1
        GROUP BY f.file_path, p.id
    ";

    let mut stmt = match db.prepare(sql) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let mut results: Vec<SearchResult> = Vec::new();
    let mut seen_files = rustc_hash::FxHashSet::default();

    if let Ok(rows) = stmt.query_map(params![fts_query], |row| {
        let file: String = row.get(0)?;
        let _phrase_id: i64 = row.get(1)?;
        let flags: Vec<u8> = row.get(2)?;
        let line_nos: Vec<u8> = row.get(3)?;
        let doc_freq: f64 = row.get(4)?;
        let tf: f64 = row.get(5)?;
        let token_len: f64 = row.get(6)?;
        Ok((file, flags, line_nos, doc_freq, tf, token_len))
    }) {
        for row in rows.flatten() {
            let (file, flags, line_nos, doc_freq, tf, token_len) = row;
            let zone = if !flags.is_empty() { Some(crate::schema::unpack_zone_int(flags[0]) as u8) } else { None };
            let line = if line_nos.len() >= 2 { Some(crate::schema::unpack_line_nos(&line_nos)) } else { None };

            let idf = crate::bm25_idf(total_files, doc_freq);
            let score = crate::bm25_score(idf, tf, token_len, avg_tokens);

            if seen_files.contains(&file) {
                if let Some(existing) = results.iter_mut().find(|r: &&mut SearchResult| r.file == file) {
                    if score > existing.score {
                        existing.score = score;
                        existing.line = line;
                        existing.zone = zone;
                    }
                }
            } else {
                seen_files.insert(file.clone());
                results.push(SearchResult { file, score, line, zone });
            }
        }
    }

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(top_n);
    results
}

/// Find all files that reference a given identifier (excluding the source file).
/// Uses the FTS5 phrase index. Returns (file_path, occurrence_count) pairs.
pub fn who_calls(db: &Connection, identifier: &str, exclude_file: &str) -> Vec<(String, u64)> {
    let stemmed = crate::porter_stem(&identifier.to_lowercase());
    let phrase_id: Option<i64> = db.query_row(
        "SELECT id FROM phrases WHERE phrase = ?1",
        params![stemmed],
        |r| r.get(0),
    ).ok(); // GUARDED: intentional — returns None on absent identifier
    let phrase_id = match phrase_id {
        Some(id) => id,
        None => return vec![],
    };

    let mut stmt = match db.prepare(
        "SELECT f.file_path, COUNT(*) as cnt
         FROM phrase_occ occ
         JOIN file_map f ON occ.file_id = f.id
         WHERE occ.phrase_id = ?1 AND f.file_path != ?2
         GROUP BY f.file_path
         ORDER BY cnt DESC
         LIMIT 20"
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let rows = match stmt.query_map(params![phrase_id, exclude_file], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
    }) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    rows.filter_map(|r| r.ok()).collect()
}

/// Get index stats
pub fn get_index_stats(db: &Connection) -> (i64, i64, i64) {
    let files = db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0)).unwrap_or(0);
    let phrases = db.query_row("SELECT COUNT(*) FROM phrases", [], |r| r.get(0)).unwrap_or(0);
    let occs = db.query_row("SELECT COUNT(*) FROM phrase_occ", [], |r| r.get(0)).unwrap_or(0);
    (files, phrases, occs)
}
