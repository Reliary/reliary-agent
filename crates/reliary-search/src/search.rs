/// FTS5 query and BM25 scoring against the inverted index.
/// Ported from stria src/search/mod.rs

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

    // Get phrase-level stats for BM25: phrase_id, file, flags, line, doc_freq (per phrase)
    let sql = "
        SELECT f.file_path, p.id as phrase_id, occ.flags, occ.line_nos,
               (SELECT COUNT(*) FROM phrase_occ WHERE phrase_id = p.id) as doc_freq
        FROM phrases_fts fts
        JOIN phrases p ON fts.rowid = p.id
        JOIN phrase_occ occ ON occ.phrase_id = p.id
        JOIN file_map f ON occ.file_id = f.id
        WHERE phrases_fts MATCH ?1
    ";

    let mut stmt = match db.prepare(sql) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let mut results: Vec<SearchResult> = Vec::new();
    let mut seen_files = std::collections::HashSet::new();

    if let Ok(rows) = stmt.query_map(params![fts_query], |row| {
        let file: String = row.get(0)?;
        let _phrase_id: i64 = row.get(1)?;
        let flags: Vec<u8> = row.get(2)?;
        let line_nos: Vec<u8> = row.get(3)?;
        let doc_freq: f64 = row.get(4)?;
        Ok((file, _phrase_id, flags, line_nos, doc_freq))
    }) {
        for row in rows.flatten() {
            let (file, _phrase_id, flags, line_nos, doc_freq) = row;
            let zone = if flags.len() >= 1 { Some(crate::schema::unpack_zone_int(flags[0]) as u8) } else { None };
            let line = if line_nos.len() >= 2 { Some(crate::schema::unpack_line_nos(&line_nos)) } else { None };

            // Proper BM25: idf from actual doc_freq per phrase
            let idf = crate::bm25_idf(total_files, doc_freq);
            let tf = db.query_row(
                "SELECT COUNT(*) FROM phrase_occ WHERE phrase_id = ?1 AND file_id = (SELECT id FROM file_map WHERE file_path = ?2)",
                params![_phrase_id, &file],
                |r| r.get::<_, f64>(0),
            ).unwrap_or(1.0);

            let file_token_len: f64 = db.query_row(
                "SELECT token_len FROM file_stats WHERE file_id = (SELECT id FROM file_map WHERE file_path = ?1)",
                params![&file],
                |r| r.get(0),
            ).unwrap_or(50.0);

            let score = crate::bm25_score(idf, tf, file_token_len, avg_tokens);

            // Dedup: keep highest score per file
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

/// Get index stats
pub fn get_index_stats(db: &Connection) -> (i64, i64, i64) {
    let files = db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0)).unwrap_or(0);
    let phrases = db.query_row("SELECT COUNT(*) FROM phrases", [], |r| r.get(0)).unwrap_or(0);
    let occs = db.query_row("SELECT COUNT(*) FROM phrase_occ", [], |r| r.get(0)).unwrap_or(0);
    (files, phrases, occs)
}
