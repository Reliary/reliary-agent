/// Append-only project chronicle stored in SQLite.
/// Every daemon action is recorded. Queried by risk thresholds and scavenger backoff.

use rusqlite::Connection;
/// Initialize chronicle table (idempotent)
pub fn init(db_path: &str) -> Result<Connection, String> {
    let db = Connection::open(db_path).map_err(|e| format!("chronicle open: {}", e))?;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS chronicle (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            t INTEGER NOT NULL,
            event TEXT NOT NULL,
            file TEXT NOT NULL DEFAULT '',
            detail TEXT NOT NULL DEFAULT '',
            outcome TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_chronicle_file ON chronicle(file);
        CREATE INDEX IF NOT EXISTS idx_chronicle_event ON chronicle(event);
        CREATE INDEX IF NOT EXISTS idx_chronicle_t ON chronicle(t);"
    ).map_err(|e| format!("chronicle schema: {}", e))?;
    Ok(db)
}

/// Append an event to the chronicle
pub fn append(db: &Connection, event: &str, file: &str, detail: &str, outcome: &str) {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    db.execute(
        "INSERT INTO chronicle (t, event, file, detail, outcome) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![t, event, file, detail, outcome],
    ).ok();
}

/// Query events for a file in the last N hours
pub fn recent_events(db: &Connection, file: &str, hours: i64) -> Vec<ChronicleEvent> {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64 - hours * 3600;
    let mut stmt = db.prepare(
        "SELECT t, event, file, detail, outcome FROM chronicle WHERE file = ?1 AND t >= ?2 ORDER BY t DESC LIMIT 50"
    ).unwrap();
    let rows = stmt.query_map(rusqlite::params![file, cutoff], |row| {
        Ok(ChronicleEvent {
            t: row.get(0)?,
            event: row.get(1)?,
            file: row.get(2)?,
            detail: row.get(3)?,
            outcome: row.get(4)?,
        })
    }).unwrap();
    rows.filter_map(|r| r.ok()).collect()
}

/// Query events by type in the last N hours
pub fn recent_events_by_type(db: &Connection, event_type: &str, hours: i64) -> Vec<ChronicleEvent> {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64 - hours * 3600;
    let mut stmt = db.prepare(
        "SELECT t, event, file, detail, outcome FROM chronicle WHERE event = ?1 AND t >= ?2 ORDER BY t DESC LIMIT 100"
    ).unwrap();
    let rows = stmt.query_map(rusqlite::params![event_type, cutoff], |row| {
        Ok(ChronicleEvent {
            t: row.get(0)?,
            event: row.get(1)?,
            file: row.get(2)?,
            detail: row.get(3)?,
            outcome: row.get(4)?,
        })
    }).unwrap();
    rows.filter_map(|r| r.ok()).collect()
}

#[derive(Debug, Clone)]
pub struct ChronicleEvent {
    pub t: i64,
    pub event: String,
    pub file: String,
    pub detail: String,
    pub outcome: String,
}
