//! Arc 35: full lazy mode for block / scope_binding / method_occurrence.
//!
//! At trust time, ONLY these tables are populated:
//!   - file_map, phrases, phrase_occ, count_overflow, file_stats, meta
//!
//! The following tables are EMPTY at trust time and built on first query:
//!   - block          (indentation-anchored boundaries per file)
//!   - scope_binding  (let/var/const/static assignments per file)
//!   - method_occurrence  (impl Type { fn method(...) } per file)
//!
//! Together with `lazy_occurrence.rs`, this puts trust time on par with
//! stria (~60s for Linux kernel). The cost is query-time JIT: each table
//! is rebuilt per file the first time it's queried, ~10-50ms per file.
//!
//! All query paths that need these tables call `ensure_*_for_file()` first.
//! The function is idempotent: if the table already has rows for the file,
//! it's a fast no-op (one indexed SELECT).

use rusqlite::{params, Connection};
use std::fs;

/// Fast guard: do we have any rows in `block` for this file_id?
pub fn has_blocks(db: &Connection, file_id: i64) -> rusqlite::Result<bool> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM block WHERE file_id = ?1)",
        params![file_id],
        |r| r.get(0),
    )?;
    Ok(exists)
}

/// Read source file, return lines as owned Vec<String>.
fn read_lines(file_path: &str) -> Option<Vec<String>> {
    let content = fs::read_to_string(file_path).ok()?;
    Some(content.lines().map(String::from).collect())
}

/// JIT build `block` rows for a single file by reading source and running
/// `detect_blocks`. Inserts via batched multi-VALUES INSERT.
///
/// Returns count of inserted rows. No-op if already populated.
pub fn ensure_blocks_for_file(db: &Connection, file_id: i64) -> rusqlite::Result<usize> {
    // Check before opening any transaction — avoids tx leak on early return.
    if has_blocks(db, file_id)? {
        return Ok(0);
    }

    // Look up file path.
    let file_path: Option<String> = db.query_row(
        "SELECT file_path FROM file_map WHERE id = ?1",
        params![file_id],
        |r| r.get(0),
    )?;
    let path = match file_path { Some(p) => p, None => return Ok(0) };

    let lines = match read_lines(&path) {
        Some(l) => l,
        None => return Ok(0),
    };
    let line_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();

    // detect_blocks is the same grammar-free function used at trust time.
    let out = crate::ingest::detect_blocks(&line_refs);

    if out.blocks.is_empty() {
        return Ok(0);
    }

    const BATCH: usize = 200;
    let mut total = 0usize;
    // C6: try BEGIN, fall back to SAVEPOINT if already inside a transaction.
    let in_tx = !db.is_autocommit();
    if in_tx {
        db.execute_batch("SAVEPOINT ensure_blocks")?;
    } else {
        db.execute_batch("BEGIN IMMEDIATE")?;
    }
    for chunk in out.blocks.chunks(BATCH) {
        let m = chunk.len();
        let placeholders = std::iter::repeat_n("(?,?,?,?)", m)
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "INSERT INTO block (file_id, start_line, end_line, indent) VALUES {}",
            placeholders
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(m * 4);
        for b in chunk {
            params.push(&file_id);
            params.push(&b.start_line);
            params.push(&b.end_line);
            params.push(&b.indent);
        }
        db.execute(&sql, params.as_slice())?;
        total += m;
    }
    if in_tx {
        db.execute_batch("RELEASE SAVEPOINT ensure_blocks")?;
    } else {
        db.execute_batch("COMMIT")?;
    }
    { crate::lazy_occurrence::bump_gen_if_inserted(total); Ok(total) }
}

/// Bulk-build block + occurrence tables for a single file.
/// C1: scope_binding and method_occurrence tables were removed (always empty).
pub fn ensure_all_for_file(db: &Connection, file_id: i64) -> rusqlite::Result<usize> {
    let mut total = ensure_blocks_for_file(db, file_id)?;
    total += crate::lazy_occurrence::ensure_occurrence_for_file(db, file_id)?;
    { crate::lazy_occurrence::bump_gen_if_inserted(total); Ok(total) }
}

/// Content-aware variant: avoids re-reading the file from disk for each
/// sub-function. The caller (reindex) already has the content in memory.
pub fn ensure_all_for_file_with_content(db: &Connection, file_id: i64, content: &str) -> rusqlite::Result<usize> {
    let mut total = ensure_blocks_for_file_with_content(db, file_id, content)?;
    total += crate::lazy_occurrence::ensure_occurrence_for_file_with_content(db, file_id, content)?;
    { crate::lazy_occurrence::bump_gen_if_inserted(total); Ok(total) }
}

/// JIT build `block` rows using pre-read content (avoids disk re-read).
pub fn ensure_blocks_for_file_with_content(db: &Connection, file_id: i64, content: &str) -> rusqlite::Result<usize> {
    if has_blocks(db, file_id)? {
        return Ok(0);
    }
    let lines: Vec<String> = content.lines().map(String::from).collect();
    let line_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    let out = crate::ingest::detect_blocks(&line_refs);
    if out.blocks.is_empty() {
        return Ok(0);
    }
    let n = out.blocks.len();
    const BATCH: usize = 200;
    let mut total = 0usize;
    // C6: nested-tx detection — if outer tx is active, use SAVEPOINT instead.
    let nested_in_tx = !db.is_autocommit();
    if nested_in_tx {
        db.execute_batch("SAVEPOINT ensure_blocks_v2")?;
    } else {
        db.execute_batch("BEGIN IMMEDIATE")?;
    }
    for chunk in out.blocks.chunks(BATCH) {
        let m = chunk.len();
        let placeholders = std::iter::repeat_n("(?,?,?,?)", m)
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "INSERT INTO block (file_id, start_line, end_line, indent) VALUES {}",
            placeholders
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(m * 4);
        for b in chunk {
            params.push(&file_id);
            params.push(&b.start_line);
            params.push(&b.end_line);
            params.push(&b.indent);
        }
        db.execute(&sql, params.as_slice())?;
        total += m;
    }
    if nested_in_tx {
        db.execute_batch("RELEASE SAVEPOINT ensure_blocks_v2")?;
    } else {
        db.execute_batch("COMMIT")?;
    }
    let _ = n;
    { crate::lazy_occurrence::bump_gen_if_inserted(total); Ok(total) }
}

/// Count files missing each lazy table — for CLI to decide whether to
/// kick off the full build.
pub fn count_lazy_pending(db: &Connection) -> rusqlite::Result<(i64, i64, i64, i64)> {
    let files: i64 = db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0))?;
    let occ_unresolved: i64 = db.query_row(
        "SELECT COUNT(DISTINCT p.id) FROM phrases p
         WHERE NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.phrase_id = p.id)",
        [], |r| r.get(0),
    )?;
    let _ = files;
    Ok((files, occ_unresolved, files, files))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_blocks_empty() {
        let db = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&db).unwrap();
        assert!(!has_blocks(&db, 1).unwrap());
        // C1: scope_binding and method_occurrence tables removed. Assert they're gone.
        let scope_err = db.execute("SELECT 1 FROM scope_binding LIMIT 1", []).unwrap_err();
        assert!(scope_err.to_string().contains("no such table"));
        let method_err = db.execute("SELECT 1 FROM method_occurrence LIMIT 1", []).unwrap_err();
        assert!(method_err.to_string().contains("no such table"));
    }

    #[test]
    fn test_count_lazy_pending_zero() {
        let db = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&db).unwrap();
        let (files, occ, blocks, _methods) = count_lazy_pending(&db).unwrap();
        assert_eq!(files, 0);
        assert_eq!(occ, 0);
        assert_eq!(blocks, 0);
    }
}