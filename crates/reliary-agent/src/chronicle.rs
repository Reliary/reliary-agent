/// Chronicled Bayesian Prior: query project history, build a compact prior block.
/// Wraps the SQL-based chronicle for session-start injection.

use std::path::Path;

fn chronicle_db_path(workdir: &str) -> String {
    format!("{}/.reliary/chronicle.sqlite", workdir.trim_end_matches('/'))
}

/// Build a compact prior block from the chronicle.
/// Returns empty string if no chronicle exists.
pub fn build_prior(workdir: &str) -> String {
    let db_path = chronicle_db_path(workdir);
    if !Path::new(&db_path).exists() {
        return String::new();
    }

    let db = match rusqlite::Connection::open(&db_path) {
        Ok(d) => d,
        Err(_) => return String::new(),
    };

    // Check if chronicle table exists
    let table_exists: bool = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='chronicle'")
        .and_then(|mut s| s.exists([]))
        .unwrap_or(false);

    if !table_exists {
        return String::new();
    }

    let mut results: Vec<String> = Vec::new();

    // Edit failures in last 24h
    if let Ok(mut s) = db.prepare(
        "SELECT file, outcome FROM chronicle WHERE event = 'edit' AND outcome = 'fail' AND timestamp > datetime('now', '-1 day') ORDER BY timestamp DESC LIMIT 5"
    ) {
        let rows: Vec<_> = s.query_map([], |row| {
            let file: String = row.get(0)?;
            let outcome: String = row.get(1)?;
            Ok(format!("{}: {}", file, outcome))
        }).into_iter().flatten().filter_map(|r| r.ok()).collect();
        if !rows.is_empty() {
            results.push(format!("edit-fails: {}", rows.join("; ")));
        }
    }

    // Veto blocks in last 24h
    if let Ok(mut s) = db.prepare(
        "SELECT file, outcome FROM chronicle WHERE event = 'veto' AND timestamp > datetime('now', '-1 day') ORDER BY timestamp DESC LIMIT 3"
    ) {
        let rows: Vec<_> = s.query_map([], |row| {
            let file: String = row.get(0)?;
            let outcome: String = row.get(1)?;
            Ok(format!("{}: {}", file, outcome))
        }).into_iter().flatten().filter_map(|r| r.ok()).collect();
        if !rows.is_empty() {
            results.push(format!("veto: {}", rows.join("; ")));
        }
    }

    // Successful edits in last 24h
    if let Ok(mut s) = db.prepare(
        "SELECT file FROM chronicle WHERE event = 'edit' AND outcome = 'pass' AND timestamp > datetime('now', '-1 day') ORDER BY timestamp DESC LIMIT 3"
    ) {
        let files: Vec<String> = s.query_map([], |row| row.get(0))
            .into_iter().flatten().filter_map(|r| r.ok()).collect();
        if !files.is_empty() {
            results.push(format!("edits: {}", files.join(", ")));
        }
    }

    if results.is_empty() {
        String::new()
    } else {
        format!("[prior] {}", results.join(" | "))
    }
}

/// Check if a specific file has recent edit failures.
/// Returns the number of failures in the last 24 hours.
pub fn recent_failures_for_file(file: &str) -> usize {
    // Derive workdir from file path
    let path = Path::new(file);
    let workdir = path.ancestors()
        .find(|p| p.join(".reliary").exists())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            path.parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string())
        });

    let db_path = chronicle_db_path(&workdir);
    if !Path::new(&db_path).exists() {
        return 0;
    }

    let db = match rusqlite::Connection::open(&db_path) {
        Ok(d) => d,
        Err(_) => return 0,
    };

    let table_exists: bool = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='chronicle'")
        .and_then(|mut s| s.exists([]))
        .unwrap_or(false);

    if !table_exists {
        return 0;
    }

    // Count edit + veto failures for this file in the last 24h
    if let Ok(mut s) = db.prepare(
        "SELECT COUNT(*) FROM chronicle WHERE (event = 'edit' AND outcome = 'fail') AND file = ?1 AND timestamp > datetime('now', '-1 day')"
    ) {
        if let Ok(count) = s.query_row([file], |row| row.get::<_, i64>(0)) {
            return count as usize;
        }
    }

    0
}
