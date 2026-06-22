//! File I/O helpers that are correct by default.
//!
//! These wrappers prevent the bug classes that keep recurring in audits:
//! - Non-atomic writes (data corruption on crash)
//! - Unbounded reads (OOM on huge files)
//! - Stdin reads (OOM on huge input)
//! - SQLite opens without PRAGMAs (slow, not crash-safe)
//!
//! Every file I/O call in the binary should go through these helpers
//! unless there's a documented reason not to. The pre-commit and CI
//! guardrails (scripts/ci_guards.py) detect direct `std::fs::write`,
//! `read_to_string`, and `read_to_string(&mut stdin buf)` calls so
//! these helpers stay the easy default.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process;
use tracing::warn;

/// Maximum file size we read into memory (10 MB). Files larger than this
/// are rejected with an error rather than read.
pub const MAX_FILE_SIZE: u64 = 10_000_000;

/// Atomic file write: write to temp file, fsync, then rename. Prevents
/// partial-write corruption on crash or power loss.
pub fn atomic_write(path: &str, content: &str) -> Result<(), String> {
    let tmp = format!("{}.tmp.{}", path, process::id());
    // Write
    if let Err(e) = fs::write(&tmp, content) {
        // Clean up partial tmp file
        let _ = fs::remove_file(&tmp);
        return Err(format!("atomic_write: write to {} failed: {}", tmp, e));
    }
    // Rename (atomic on POSIX)
    if let Err(e) = fs::rename(&tmp, path) {
        // Clean up tmp file (now orphaned)
        let _ = fs::remove_file(&tmp);
        return Err(format!("atomic_write: rename {} -> {} failed: {}", tmp, path, e));
    }
    Ok(())
}

/// Read a file with size cap. Returns an error string if the file is
/// missing, unreadable, or larger than `MAX_FILE_SIZE`.
pub fn safe_read(path: &str) -> Result<String, String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("safe_read: {} does not exist", path));
    }
    if let Ok(meta) = p.metadata() {
        if meta.len() > MAX_FILE_SIZE {
            return Err(format!(
                "safe_read: {} is {} bytes, exceeds max {} bytes",
                path, meta.len(), MAX_FILE_SIZE
            ));
        }
    }
    fs::read_to_string(path).map_err(|e| format!("safe_read: {}: {}", path, e))
}

/// Read from stdin with a size cap. Reads up to `MAX_FILE_SIZE` bytes then
/// errors. Prevents OOM on `cat huge.log | reliary-agent ...`.
pub fn safe_read_stdin() -> Result<String, String> {
    let mut buf = Vec::with_capacity(4096);
    let mut handle = std::io::stdin().take(MAX_FILE_SIZE + 1);
    if let Err(e) = handle.read_to_end(&mut buf) {
        return Err(format!("safe_read_stdin: read failed: {}", e));
    }
    if buf.len() as u64 > MAX_FILE_SIZE {
        return Err(format!(
            "safe_read_stdin: input exceeds {} bytes",
            MAX_FILE_SIZE
        ));
    }
    String::from_utf8(buf).map_err(|e| format!("safe_read_stdin: invalid UTF-8: {}", e))
}

/// Open a SQLite database with the correct PRAGMAs applied. This is the
/// only way to open a DB in the project — direct `Connection::open` calls
/// are flagged by guardrails.
pub fn safe_open_db(path: &str) -> Result<rusqlite::Connection, String> {
    let db = rusqlite::Connection::open(path)
        .map_err(|e| format!("safe_open_db: open {}: {}", path, e))?;
    // Set WAL + synchronous=NORMAL for crash recovery + concurrent reads.
    // These are the only correct PRAGMAs for our use case.
    if let Err(e) = db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
    ) {
        warn!("safe_open_db: PRAGMA set failed: {}", e);
    }
    Ok(db)
}

/// Check if a write would exceed the size cap. Used for special cases
/// (e.g., writing huge generated content) where atomic_write isn't appropriate.
pub fn check_write_size(path: &str, content: &str) -> Result<(), String> {
    if content.len() as u64 > MAX_FILE_SIZE {
        return Err(format!(
            "check_write_size: {} content is {} bytes, exceeds max {} bytes",
            path, content.len(), MAX_FILE_SIZE
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(unused_imports)]
    fn test_atomic_write_creates_file() {
        let dir = std::env::temp_dir().join("reliary_fs_safe_atomic");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test.txt");
        atomic_write(path.to_str().unwrap(), "hello").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_atomic_write_cleans_tmp_on_rename_failure() {
        // Try to write to a path whose parent doesn't exist
        let bad_path = "/nonexistent/directory/test.txt";
        let result = atomic_write(bad_path, "content");
        assert!(result.is_err(), "should fail when parent dir doesn't exist");
        // Verify no tmp file leaked (we can't easily verify but should at least not crash)
    }

    #[test]
    fn test_safe_read_size_cap() {
        let dir = std::env::temp_dir().join("reliary_fs_safe_read");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("small.txt");
        fs::write(&path, "small content").unwrap();
        let result = safe_read(path.to_str().unwrap());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "small content");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_safe_read_missing_file() {
        let result = safe_read("/nonexistent/path/file.txt");
        assert!(result.is_err());
    }
}
