//! Content cache — stores original content by hash, retrievable on demand.
//! Enables aggressive compression of tool results and reasoning: if the LLM
//! needs originals, it calls `reliary retrieve <hash>` to get them back.
//! Storage: SQLite at .reliary/cache.sqlite in the project root.

use rusqlite::{Connection, params};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_TTL_SECS: u64 = 3600;
const DEFAULT_MAX_ENTRIES: usize = 500;

#[inline(always)]
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Hash content for storage key. Returns 16-char hex of 64-bit SipHash.
/// D26: Keep DefaultHasher for now — speed is adequate for cache key use.
pub fn hash_content(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    content.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Open or create the cache database at the given path.
pub fn open(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| format!("open: {}", e))?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
        .map_err(|e| format!("pragma: {}", e))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS content_cache (
            hash TEXT PRIMARY KEY,
            original BLOB NOT NULL,
            stored_at INTEGER NOT NULL,
            accessed_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_accessed ON content_cache(accessed_at);",
    )
    .map_err(|e| format!("create: {}", e))?;
    Ok(conn)
}

/// Store original content, return hash.
pub fn store(conn: &Connection, content: &str) -> Result<String, String> {
    let hash = hash_content(content);
    let now = now();
    conn.execute(
        "INSERT OR REPLACE INTO content_cache (hash, original, stored_at, accessed_at) VALUES (?1, ?2, ?3, ?3)",
        params![hash, content.as_bytes(), now],
    )
    .map_err(|e| format!("insert: {}", e))?;
    Ok(hash)
}

/// Retrieve original content by hash. Updates accessed_at in one round trip.
pub fn retrieve(conn: &Connection, hash: &str) -> Result<Option<String>, String> {
    let now = now();
    let mut stmt = conn
        .prepare_cached("UPDATE content_cache SET accessed_at = ?1 WHERE hash = ?2 RETURNING original")
        .map_err(|e| format!("prepare: {}", e))?;
    match stmt.query_row(params![now, hash], |row| row.get::<_, Vec<u8>>(0)) {
        Ok(blob) => String::from_utf8(blob).map(Some).map_err(|e| format!("utf8: {}", e)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("query: {}", e)),
    }
}

/// Evict entries older than TTL or beyond max_entries count.
pub fn evict(conn: &Connection, ttl_secs: u64, max_entries: usize) -> Result<usize, String> {
    let now = now();
    let expired = conn
        .execute(
            "DELETE FROM content_cache WHERE accessed_at < ?1",
            params![now.saturating_sub(ttl_secs)],
        )
        .map_err(|e| format!("evict ttl: {}", e))?;
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM content_cache", [], |r| r.get(0))
        .unwrap_or(0);
    let mut trimmed = 0;
    if (count as usize) > max_entries {
        let excess = count as usize - max_entries;
        trimmed = conn
            .execute(
                "DELETE FROM content_cache WHERE hash IN (
                    SELECT hash FROM content_cache ORDER BY accessed_at ASC LIMIT ?1
                )",
                params![excess],
            )
            .map_err(|e| format!("evict max: {}", e))?;
    }
    Ok(expired + trimmed)
}

/// Stats for the `reliary stats` command.
/// D29: Combined into single query.
pub fn stats(conn: &Connection) -> Result<(usize, u64), String> {
    let (count, bytes): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(original)), 0) FROM content_cache",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));
    Ok((count as usize, bytes as u64))
}

#[inline(always)]
pub fn default_ttl() -> u64 {
    DEFAULT_TTL_SECS
}

#[inline(always)]
pub fn default_max_entries() -> usize {
    DEFAULT_MAX_ENTRIES
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_db() -> Connection {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = env::temp_dir().join(format!("reliary_test_{}_{}_{}.db", std::process::id(), now(), n));
        let _ = std::fs::remove_file(&path);
        open(&path).unwrap()
    }

    #[test]
    fn store_and_retrieve() {
        let conn = temp_db();
        let hash = store(&conn, "hello world").unwrap();
        assert_eq!(retrieve(&conn, &hash).unwrap(), Some("hello world".to_string()));
    }

    #[test]
    fn different_content_different_hash() {
        let conn = temp_db();
        let h1 = store(&conn, "alpha").unwrap();
        let h2 = store(&conn, "beta").unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn missing_hash_returns_none() {
        let conn = temp_db();
        assert_eq!(retrieve(&conn, "nonexistent").unwrap(), None);
    }

    #[test]
    fn evict_by_count() {
        let conn = temp_db();
        for i in 0..10 {
            store(&conn, &format!("entry_{}", i)).unwrap();
        }
        let removed = evict(&conn, u64::MAX, 5).unwrap();
        assert_eq!(removed, 5);
        let (count, _) = stats(&conn).unwrap();
        assert_eq!(count, 5);
    }

    #[test]
    fn stats_reports_count_and_bytes() {
        let conn = temp_db();
        store(&conn, "one").unwrap();
        store(&conn, "two").unwrap();
        let (count, bytes) = stats(&conn).unwrap();
        assert_eq!(count, 2);
        assert!(bytes >= 6, "expected at least 6 bytes, got {}", bytes);
    }
}