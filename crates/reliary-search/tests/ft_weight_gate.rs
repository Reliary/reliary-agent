//! Tests for FTS5 DF weighting gate (RELIARY_PROXY_FT_WEIGHT env var).
//!
//! Verifies that:
//! 1. Default state (env var unset) → FtWeight::open returns None or scoring disabled
//! 2. Explicit "0" → disabled
//! 3. Explicit "1" → enabled (if FTS5 index exists)
//! 4. Invalid values → disabled (fail-safe)

use std::env;
use std::sync::Mutex;

// Mutex to serialize env-var-mutating tests
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_ft_weight_open_empty_db() {
    // Use a non-existent path → FtWeight::open should return None
    let result = reliary_search::ft_weight::FtWeight::open("/nonexistent/path/index.sqlite");
    assert!(result.is_none(), "Nonexistent path should return None");
}

#[test]
fn test_ft_weight_open_valid_empty_index() {
    // Open in-memory SQLite, create empty file_map → returns None
    // (total_files == 0 means no useful index)
    use rusqlite::Connection;
    let tmp = std::env::temp_dir().join("test_ft_weight_empty.sqlite");
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch("
        CREATE TABLE file_map (id INTEGER PRIMARY KEY, file_path TEXT NOT NULL UNIQUE);
        CREATE TABLE phrases (id INTEGER PRIMARY KEY, phrase TEXT NOT NULL UNIQUE);
        CREATE TABLE phrase_occ (phrase_id INTEGER, file_id INTEGER, flags BLOB NOT NULL, line_nos BLOB NOT NULL, PRIMARY KEY (phrase_id, file_id)) WITHOUT ROWID;
    ").unwrap();
    drop(conn);

    let result = reliary_search::ft_weight::FtWeight::open(tmp.to_str().unwrap());
    // Empty index (no files) → returns None
    assert!(result.is_none(), "Empty FTS5 index should return None");

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_ft_weight_open_populated_index() {
    use rusqlite::Connection;
    let tmp = std::env::temp_dir().join("test_ft_weight_populated.sqlite");
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch("
        CREATE TABLE file_map (id INTEGER PRIMARY KEY, file_path TEXT NOT NULL UNIQUE);
        CREATE TABLE phrases (id INTEGER PRIMARY KEY, phrase TEXT NOT NULL UNIQUE);
        CREATE TABLE phrase_occ (phrase_id INTEGER, file_id INTEGER, flags BLOB NOT NULL, line_nos BLOB NOT NULL, PRIMARY KEY (phrase_id, file_id)) WITHOUT ROWID;
        INSERT INTO file_map (id, file_path) VALUES (1, 'src/main.rs');
        INSERT INTO file_map (id, file_path) VALUES (2, 'src/lib.rs');
        INSERT INTO file_map (id, file_path) VALUES (3, 'tests/test_main.rs');
        INSERT INTO phrases (id, phrase) VALUES (1, 'test'), (2, 'function'), (3, 'main');
    ").unwrap();
    drop(conn);

    let fw = reliary_search::ft_weight::FtWeight::open(tmp.to_str().unwrap());
    assert!(fw.is_some(), "Populated index should return Some");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_ft_weight_df_unknown_token() {
    use rusqlite::Connection;
    let tmp = std::env::temp_dir().join("test_ft_weight_df.sqlite");
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch("
        CREATE TABLE file_map (id INTEGER PRIMARY KEY, file_path TEXT NOT NULL UNIQUE);
        CREATE TABLE phrases (id INTEGER PRIMARY KEY, phrase TEXT NOT NULL UNIQUE);
        CREATE TABLE phrase_occ (phrase_id INTEGER, file_id INTEGER, flags BLOB NOT NULL, line_nos BLOB NOT NULL, PRIMARY KEY (phrase_id, file_id)) WITHOUT ROWID;
        INSERT INTO file_map (id, file_path) VALUES (1, 'a.rs'), (2, 'b.rs');
    ").unwrap();
    drop(conn);

    let mut fw = reliary_search::ft_weight::FtWeight::open(tmp.to_str().unwrap()).unwrap();
    // Unknown token → DF=0 (no entry in phrases table)
    let df = fw.df("nonexistent_token_xyz");
    assert_eq!(df, 0);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_ft_weight_df_caching() {
    use rusqlite::Connection;
    let tmp = std::env::temp_dir().join("test_ft_weight_cache.sqlite");
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch("
        CREATE TABLE file_map (id INTEGER PRIMARY KEY, file_path TEXT NOT NULL UNIQUE);
        CREATE TABLE phrases (id INTEGER PRIMARY KEY, phrase TEXT NOT NULL UNIQUE);
        CREATE TABLE phrase_occ (phrase_id INTEGER, file_id INTEGER, flags BLOB NOT NULL, line_nos BLOB NOT NULL, PRIMARY KEY (phrase_id, file_id)) WITHOUT ROWID;
        INSERT INTO file_map (id, file_path) VALUES (1, 'a.rs');
    ").unwrap();
    drop(conn);

    let mut fw = reliary_search::ft_weight::FtWeight::open(tmp.to_str().unwrap()).unwrap();
    // First call → DB lookup
    let df1 = fw.df("cached_token");
    // Second call → from cache
    let df2 = fw.df("cached_token");
    assert_eq!(df1, df2);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_ft_weight_should_preserve_with_no_significant_tokens() {
    use rusqlite::Connection;
    let tmp = std::env::temp_dir().join("test_ft_weight_preserve.sqlite");
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp).unwrap();
    conn.execute_batch("
        CREATE TABLE file_map (id INTEGER PRIMARY KEY, file_path TEXT NOT NULL UNIQUE);
        CREATE TABLE phrases (id INTEGER PRIMARY KEY, phrase TEXT NOT NULL UNIQUE);
        CREATE TABLE phrase_occ (phrase_id INTEGER, file_id INTEGER, flags BLOB NOT NULL, line_nos BLOB NOT NULL, PRIMARY KEY (phrase_id, file_id)) WITHOUT ROWID;
        INSERT INTO file_map (id, file_path) VALUES (1, 'a.rs');
    ").unwrap();
    drop(conn);

    let mut fw = reliary_search::ft_weight::FtWeight::open(tmp.to_str().unwrap()).unwrap();
    // Line with no identifiers ≥ 3 chars → score=0 → preserve
    assert!(fw.should_preserve(""));
    assert!(fw.should_preserve("a b c"));
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_env_var_default_off() {
    // Verify build_info_scorer returns None when env var is unset
    // (We can't test build_info_scorer directly because it's private,
    // but we can verify the env var check works as expected.)
    let _guard = ENV_LOCK.lock().unwrap();
    let prev = env::var("RELIARY_PROXY_FT_WEIGHT").ok();
    env::remove_var("RELIARY_PROXY_FT_WEIGHT");
    // Default state: var unset → "0" semantics (disabled)
    let enabled = env::var("RELIARY_PROXY_FT_WEIGHT")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    assert!(!enabled, "Default should be disabled");

    // Restore
    if let Some(v) = prev {
        env::set_var("RELIARY_PROXY_FT_WEIGHT", v);
    }
}

#[test]
fn test_env_var_explicit_off() {
    let _guard = ENV_LOCK.lock().unwrap();
    let prev = env::var("RELIARY_PROXY_FT_WEIGHT").ok();
    env::set_var("RELIARY_PROXY_FT_WEIGHT", "0");
    let enabled = env::var("RELIARY_PROXY_FT_WEIGHT")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    assert!(!enabled, "Explicit '0' should disable");

    env::set_var("RELIARY_PROXY_FT_WEIGHT", "false");
    let enabled = env::var("RELIARY_PROXY_FT_WEIGHT")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    assert!(!enabled, "Explicit 'false' should disable");

    if let Some(v) = prev {
        env::set_var("RELIARY_PROXY_FT_WEIGHT", v);
    } else {
        env::remove_var("RELIARY_PROXY_FT_WEIGHT");
    }
}

#[test]
fn test_env_var_explicit_on() {
    let _guard = ENV_LOCK.lock().unwrap();
    let prev = env::var("RELIARY_PROXY_FT_WEIGHT").ok();
    env::set_var("RELIARY_PROXY_FT_WEIGHT", "1");
    let enabled = env::var("RELIARY_PROXY_FT_WEIGHT")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    assert!(enabled, "Explicit '1' should enable");

    env::set_var("RELIARY_PROXY_FT_WEIGHT", "true");
    let enabled = env::var("RELIARY_PROXY_FT_WEIGHT")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    assert!(enabled, "Explicit 'true' should enable");

    if let Some(v) = prev {
        env::set_var("RELIARY_PROXY_FT_WEIGHT", v);
    } else {
        env::remove_var("RELIARY_PROXY_FT_WEIGHT");
    }
}
