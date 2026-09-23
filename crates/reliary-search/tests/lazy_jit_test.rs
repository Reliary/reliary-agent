//! Integration test for lazy occurrence JIT build.
use rusqlite::Connection;
use reliary_search::{schema, ingest, lazy_occurrence};

#[test]
fn test_jit_build_for_phrase() {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    // Honor TMPDIR: hardcoding /tmp fails on hosts where /tmp is a small
    // tmpfs (or is otherwise unwritable) even when a writable temp dir is
    // configured. env::temp_dir() reads TMPDIR/TMP/TEMP with /tmp fallback.
    let dir = std::env::temp_dir().join(format!("reliary_test_{}", nanos));
    let dir = dir.to_string_lossy().to_string();
    let _ = std::fs::create_dir_all(&dir);
    // Write a small Rust file with a known phrase.
    let src = r#"
fn park() { println!("park fn"); }
struct Park { x: i32 }
impl Park {
    fn park(&self) { self.x }
}
fn main() {
    let p = Park { x: 1 };
    p.park();
    park();
}
"#;
    let src_path = format!("{}/main.rs", dir);
    std::fs::write(&src_path, src).unwrap();

    // Index the dir.
    let db_path = format!("{}/idx.sqlite", dir);
    let _ = std::fs::remove_file(&db_path);
    let db = Connection::open(&db_path).unwrap();
    schema::create_new_db(&db).unwrap();
    let count = ingest::index_directory(&db, &dir).unwrap();
    assert!(count > 0, "should have indexed at least 1 file");

    // Look up phrase_id for "park".
    let phrase_id: i64 = db.query_row(
        "SELECT id FROM phrases WHERE phrase = ?1",
        rusqlite::params!["park"],
        |r| r.get(0),
    ).expect("phrase 'park' should exist");

    // occurrence should be empty (lazy).
    let n: i64 = db.query_row(
        "SELECT COUNT(*) FROM occurrence WHERE phrase_id = ?1",
        rusqlite::params![phrase_id],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(n, 0, "occurrence should be empty at trust time (got {})", n);

    // Trigger JIT.
    let built = lazy_occurrence::ensure_occurrence_for_phrase(&db, phrase_id).unwrap();
    eprintln!("JIT built {} rows for 'park'", built);
    assert!(built > 0, "JIT should have built at least one occurrence");

    // Now occurrence has rows.
    let n: i64 = db.query_row(
        "SELECT COUNT(*) FROM occurrence WHERE phrase_id = ?1",
        rusqlite::params![phrase_id],
        |r| r.get(0),
    ).unwrap();
    assert!(n > 0);

    // Subsequent call is a no-op.
    let built2 = lazy_occurrence::ensure_occurrence_for_phrase(&db, phrase_id).unwrap();
    assert_eq!(built2, 0, "second call should be a no-op (got {})", built2);

    // Cleanup.
    let _ = std::fs::remove_dir_all(&dir);
}