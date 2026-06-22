//! SQLite schema for FTS5 phrase index.

use rusqlite::Connection;

pub const SCHEMA_VERSION: i32 = 1;

pub fn create_new_db(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "PRAGMA page_size = 65536;
         PRAGMA synchronous = OFF;
         PRAGMA journal_mode = MEMORY;
         PRAGMA cache_size = -200000;
         PRAGMA mmap_size = 268435456;
         PRAGMA temp_store = MEMORY;
         PRAGMA lock_timeout = 5000;",
    )?;
    create_tables(db)?;
    db.execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION))
}

pub fn open_existing_db(db: &Connection) -> rusqlite::Result<()> {
    // Bug 61: synchronous=OFF + journal_mode=MEMORY sacrifices crash safety for
    // indexing speed. The index is rebuildable from source so this is acceptable
    // for ingest. For read-only access, callers should use open_existing_db_safe()
    // which uses WAL + NORMAL for crash safety.
    db.execute_batch(
        "PRAGMA synchronous = OFF;
         PRAGMA journal_mode = MEMORY;
         PRAGMA cache_size = -200000;
         PRAGMA mmap_size = 268435456;
         PRAGMA temp_store = MEMORY;
         PRAGMA lock_timeout = 5000;",
    )?;
    let version: i32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidColumnName(format!(
            "Schema version mismatch: DB has {}, expected {}. Run `reliary-agent index` to rebuild.",
            version, SCHEMA_VERSION
        )));
    }
    Ok(())
}

/// Read-only open with crash-safe PRAGMAs (Bug 61).
/// Use this for daemon startup and search queries where crash safety matters.
/// Trade-off: slightly slower than open_existing_db() but protected against corruption.
pub fn open_existing_db_safe(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -200000;
         PRAGMA mmap_size = 268435456;
         PRAGMA temp_store = MEMORY;
         PRAGMA lock_timeout = 5000;",
    )?;
    let version: i32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidColumnName(format!(
            "Schema version mismatch: DB has {}, expected {}. Run `reliary-agent index` to rebuild.",
            version, SCHEMA_VERSION
        )));
    }
    Ok(())
}

fn create_tables(db: &Connection) -> rusqlite::Result<()> {
    // Base tables first
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS file_map (
            id INTEGER PRIMARY KEY,
            file_path TEXT NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS phrases (
            id INTEGER PRIMARY KEY,
            phrase TEXT NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS phrase_occ (
            phrase_id INTEGER,
            file_id INTEGER,
            flags BLOB NOT NULL,
            line_nos BLOB NOT NULL,
            PRIMARY KEY (phrase_id, file_id)
        ) WITHOUT ROWID;
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
    // FTS5 virtual table — direct population (not external content)
    db.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS phrases_fts USING fts5(
            phrase, tokenize='trigram'
        );"
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
        let words: Vec<&str> = s.split_whitespace().collect();
        if !words.is_empty() {
            let avg = words.iter().map(|w| w.len()).sum::<usize>() as f64 / words.len() as f64;
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
pub fn unpack_zone_int(flags: u8) -> i32 { ((flags >> 2) & 0x01) as i32 }
pub fn unpack_count(flags: u8) -> u32 { (flags >> 3) as u32 }

pub fn pack_line_nos(line: u32) -> [u8; 2] { (line as u16).to_le_bytes() }
pub fn unpack_line_nos(blob: &[u8]) -> u32 {
    if blob.len() >= 2 { u16::from_le_bytes([blob[0], blob[1]]) as u32 } else { 0 }
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
    fn test_line_number_pack() {
        let packed = pack_line_nos(42);
        assert_eq!(unpack_line_nos(&packed), 42);
    }

    #[test]
    fn test_overflow_count() {
        let packed = pack_flags(0, 0, 100);
        assert_eq!(unpack_count(packed[0]), 31); // overflow sentinel
    }
}
