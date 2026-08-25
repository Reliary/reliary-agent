//! SQLite schema for FTS5 phrase index.

use rusqlite::Connection;

pub const SCHEMA_VERSION: i32 = 5;

pub fn create_new_db(db: &Connection) -> rusqlite::Result<()> {
    // Arc 34: kept MEMORY+OFF despite trying WAL. Bench showed WAL is 12-25%
    // slower per-INSERT on tokio + drivers, and the WAL file + SHM updates
    // don't help for our use case (single huge transaction, no concurrent
    // readers during ingest). The 5 GB in-memory journal at COMMIT is
    // reclaimed after COMMIT — RAM pressure only during indexing.
    db.execute_batch(
        "PRAGMA page_size = 65536;
         PRAGMA synchronous = OFF;
         PRAGMA journal_mode = MEMORY;
         PRAGMA cache_size = -200000;
         PRAGMA temp_store = MEMORY;
         PRAGMA lock_timeout = 5000;",
    )?;
    create_tables(db)?;
    db.execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION))
}

pub fn open_existing_db(db: &Connection) -> rusqlite::Result<()> {
    // Arc 34: kept MEMORY+OFF (see create_new_db comment).
    db.execute_batch(
        "PRAGMA synchronous = OFF;
         PRAGMA journal_mode = MEMORY;
         PRAGMA cache_size = -200000;
         PRAGMA temp_store = MEMORY;
         PRAGMA lock_timeout = 5000;",
    )?;
    run_migrations(db)
}

/// Read-only open with crash-safe PRAGMAs (Bug 61).
/// Use this for daemon startup and search queries where crash safety matters.
/// Trade-off: slightly slower than open_existing_db() but protected against corruption.
pub fn open_existing_db_safe(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -200000;
         PRAGMA temp_store = MEMORY;
         PRAGMA lock_timeout = 5000;",
    )?;
    run_migrations(db)
}

fn run_migrations(db: &Connection) -> rusqlite::Result<()> {
    let version: i32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == SCHEMA_VERSION { return Ok(()); }

    if version == 4 {
        // Schema v4 → v5: add is_source column to file_map, add file_phrases table.
        let _ = db.execute_batch(
            "ALTER TABLE file_map ADD COLUMN is_source INTEGER NOT NULL DEFAULT 1;
             CREATE TABLE IF NOT EXISTS file_phrases (
                 file_id INTEGER NOT NULL,
                 phrase_id INTEGER NOT NULL,
                 PRIMARY KEY (file_id, phrase_id)
             );
             CREATE INDEX IF NOT EXISTS idx_file_phrases_file ON file_phrases(file_id);
             PRAGMA user_version = 5;"
        );
        return Ok(());
    }

    Err(rusqlite::Error::InvalidColumnName(format!(
        "Schema version mismatch: DB has {}, expected {}. Run `reliary-agent index` to rebuild.",
        version, SCHEMA_VERSION
    )))
}

fn create_tables(db: &Connection) -> rusqlite::Result<()> {
    // Base tables first
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS file_map (
            id INTEGER PRIMARY KEY,
            file_path TEXT NOT NULL UNIQUE,
            is_source INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS phrases (
            id INTEGER PRIMARY KEY,
            phrase TEXT NOT NULL UNIQUE
        );
        -- V54: covering index for prefix LIKE lookups (closest_symbols, phrase fallback).
        -- UNIQUE(phrase) already gives exact-match lookup; this adds prefix scans
        -- with LENGTH ordering for did-you-mean suggestions on empty results.
        CREATE INDEX IF NOT EXISTS idx_phrases_prefix ON phrases(phrase);
        CREATE TABLE IF NOT EXISTS phrase_occ (
            phrase_id INTEGER PRIMARY KEY,
            file_blob BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS file_phrases (
            file_id INTEGER NOT NULL,
            phrase_id INTEGER NOT NULL,
            PRIMARY KEY (file_id, phrase_id)
        );
        CREATE INDEX IF NOT EXISTS idx_file_phrases_file ON file_phrases(file_id);
        CREATE TABLE IF NOT EXISTS count_overflow (
            phrase_id INTEGER,
            file_id INTEGER,
            count INTEGER NOT NULL,
            PRIMARY KEY (phrase_id, file_id)
        ) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS file_stats (
            file_id INTEGER PRIMARY KEY,
            token_len INTEGER DEFAULT 0,
            content_len INTEGER DEFAULT 0,
            unique_def_count INTEGER DEFAULT 0,
            total_def_count INTEGER DEFAULT 0,
            comment_ratio REAL DEFAULT 0.0
        );
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value REAL
        );",
    )?;
    // Occurrence-level tables (schema v2) — additive, no break to v1 tables.
    // occurrence: one row per token occurrence (NOT aggregated to file level).
    // block: indentation-anchored boundary (grammar-free, see ingest.rs).
    // These enable symbol-level queries (find-references, goto-def, callgraph, etc.)
    // that the file-level phrase_occ table cannot answer.
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS occurrence (
            occ_id INTEGER PRIMARY KEY,
            phrase_id INTEGER NOT NULL,
            file_id INTEGER NOT NULL,
            line INTEGER NOT NULL,
            col INTEGER NOT NULL,
            is_def INTEGER NOT NULL,
            block_id INTEGER NOT NULL,
            tag INTEGER NOT NULL DEFAULT 0
        );
        -- Arc 32: Drop redundant single-column indexes on occurrence.
        -- Composite indexes (created in Arc 31 Phase D) cover the same access
        -- patterns and save ~11 MiB on tokio DB.
        DROP INDEX IF EXISTS idx_occ_phrase;
        DROP INDEX IF EXISTS idx_occ_block;
        DROP INDEX IF EXISTS idx_occ_file;
        -- Arc 31 Phase D: composite covering indexes (replaces the dropped ones).
        CREATE INDEX IF NOT EXISTS idx_occ_phrase_isdef ON occurrence(phrase_id, is_def, file_id);
        CREATE INDEX IF NOT EXISTS idx_occ_block_file ON occurrence(block_id, file_id);
        CREATE TABLE IF NOT EXISTS block (
            block_id INTEGER PRIMARY KEY,
            file_id INTEGER NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            indent INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_block_file ON block(file_id);
        -- P1-5: Covering index for block_id_at (file_id, start_line, end_line).
        -- Old: O(N) scan of all blocks in file to find smallest enclosing block.
        -- New: O(log N) index lookup, ordered by end_line DESC for smallest-match.
        CREATE INDEX IF NOT EXISTS idx_block_range ON block(file_id, start_line, end_line);
        -- C1: scope_binding and method_occurrence tables removed (always empty
        -- since compat stubs returned 0). Tables, indexes, and UNIQUE constraints
        -- were dropped in v0.10. New indexes on existing tables continue."
    )?;
    Ok(())
}

// --- Zone classification (grammar-free byte DFA) ---
pub fn classify_line(line: &str) -> u8 {
    let s = line.trim();
    if s.is_empty() { return 1; }
    let bytes = s.as_bytes();

    if !bytes.is_empty() && bytes[0] == b'/' && bytes.len() >= 2
        && (bytes[1] == b'/' || bytes[1] == b'*') { return 1; }

    if !bytes.is_empty() && bytes[0] == b'#' {
        if bytes.len() >= 2 && bytes[1] == b'!' { return 0; }
        return 1;
    }

    if bytes.starts_with(b"*") || bytes.starts_with(b"<!--") || bytes.starts_with(b">") { return 1; }

    let mut structural = 0u32;
    let mut lower = 0u32;
    let slen = s.len().max(1) as f64;

    for &b in bytes {
        match b {
            b'a'..=b'z' => lower += 1,
            b'{' | b'}' | b'(' | b')' | b'[' | b']' | b'<' | b'>' | b';' | b':' | b'=' | b'|'
            | b'&' | b'!' | b'@' | b'#' | b'$' | b'%' | b'^' | b'*' | b'-' | b'+' | b'/' | b'?' | b'\\' => structural += 1,
            _ => {}
        }
    }

    let mut idents = 0u32;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let mut count = 1u32;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1; count += 1;
            }
            if count >= 3 { idents += 1; }
        } else { i += 1; }
    }

    if slen > 0.0 {
        let prose_ratio = lower as f64 / slen;
        let struct_ratio = structural as f64 / slen;
        if prose_ratio > 0.65 && struct_ratio < 0.08 && idents < 3 { return 1; }
        if idents == 0 { return 1; }
        let (word_sum, word_count) = s.split_whitespace().fold((0usize, 0usize), |(sum, cnt), w| (sum + w.len(), cnt + 1));
        if word_count > 0 {
            let avg = word_sum as f64 / word_count as f64;
            if prose_ratio > 0.5 && avg < 6.0 && struct_ratio < 0.05 && idents < 2 { return 1; }
        }
    }

    // Final heuristic: English prose has lowercase+word boundaries
    if bytes.len() >= 10 {
        let special_count = structural as f64;
        if special_count > 2.0 { return 0; }
        let lower_ratio = lower as f64 / slen;
        if lower_ratio > 0.7 && idents <= 1 { return 1; }
    }

    0
}

// --- Packing helpers (bit-packing for storage efficiency) ---
const COUNT_OVERFLOW: u8 = 31;

pub fn pack_flags(is_def: i32, zone_int: i32, count: u32) -> [u8; 1] {
    let is_def_packed = ((is_def + 1) as u8) & 0x03;
    let zone_packed = (zone_int as u8) & 0x01;
    let count_packed = if count <= 30 { count as u8 } else { COUNT_OVERFLOW };
    [is_def_packed | (zone_packed << 2) | (count_packed << 3)]
}

pub fn unpack_is_def(flags: u8) -> i32 { ((flags & 0x03) as i32) - 1 }
#[inline(always)]
pub fn unpack_zone_int(flags: u8) -> i32 { ((flags >> 2) & 0x01) as i32 }
#[inline(always)]
pub fn unpack_count(flags: u8) -> u32 { (flags >> 3) as u32 }

#[deprecated(note = "Arc 36: line_nos column removed from phrase_occ. These functions are no-ops kept for backward compatibility. Use occurrence.table for line numbers.")]
pub fn pack_line_nos(line: u32) -> [u8; 2] { (line as u16).to_le_bytes() }
#[deprecated(note = "Arc 36: line_nos column removed from phrase_occ. Returns 0 (no data).")]
pub fn unpack_line_nos(_blob: &[u8]) -> u32 {
    0
}

/// Arc 37 Tier C #1: pack a slice of (file_id, flags) pairs into a single BLOB.
/// Format: varint(file_id_0), flags_0, varint(file_id_1), flags_1, ...
/// Caller must provide file_ids in strictly increasing order.
pub fn pack_file_blob(pairs: &[(i64, u8)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pairs.len() * 3);
    for (fid, flags) in pairs {
        encode_varint(*fid, &mut out);
        out.push(*flags);
    }
    out
}

/// Iterate (file_id, flags) pairs from a packed blob.
pub fn unpack_file_blob<'a>(bytes: &'a [u8]) -> impl Iterator<Item = (i64, u8)> + 'a {
    let mut i = 0;
    std::iter::from_fn(move || {
        if i >= bytes.len() {
            return None;
        }
        let (fid, consumed) = match decode_varint(&bytes[i..]) {
            Some(v) => v,
            None => return None,
        };
        i += consumed;
        if i >= bytes.len() {
            return None;
        }
        let flags = bytes[i];
        i += 1;
        Some((fid, flags))
    })
}

/// Encode a `file_id` (i64) as a LEB128 varint.
pub fn encode_varint(n: i64, out: &mut Vec<u8>) {
    let mut n = n as u64;
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// Decode a varint from bytes; returns (value, bytes_consumed).
pub fn decode_varint(bytes: &[u8]) -> Option<(i64, usize)> {
    let mut result: i64 = 0;
    let mut shift = 0;
    for (i, &b) in bytes.iter().enumerate() {
        result |= ((b & 0x7F) as i64) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            return Some((result, i + 1));
        }
        if i >= 9 {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_code() {
        assert_eq!(classify_line("fn foo() {"), 0);
        assert_eq!(classify_line("    let x = 1;"), 0);
    }

    #[test]
    fn test_classify_prose() {
        assert_eq!(classify_line("# This is a comment"), 1);
        assert_eq!(classify_line(""), 1);
    }

    #[test]
    fn test_pack_roundtrip() {
        let packed = pack_flags(0, 1, 5);
        assert_eq!(unpack_is_def(packed[0]), 0);
        assert_eq!(unpack_zone_int(packed[0]), 1);
        assert_eq!(unpack_count(packed[0]), 5);
    }

    #[test]
    #[allow(deprecated)]
    fn test_line_number_pack() {
        // pack_line_nos still works (returns the bytes) — backward-compat shim.
        let packed = pack_line_nos(42);
        assert_eq!(packed[0], 42);
        assert_eq!(packed[1], 0);
        // unpack_line_nos is now a no-op (returns 0). Real line data lives
        // in the lazy `occurrence` table.
        assert_eq!(unpack_line_nos(&packed), 0);
    }

    #[test]
    fn test_overflow_count() {
        let packed = pack_flags(0, 0, 100);
        assert_eq!(unpack_count(packed[0]), 31); // overflow sentinel
    }
}
