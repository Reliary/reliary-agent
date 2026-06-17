//! Append-only project chronicle stored in SQLite.
// Every daemon action is recorded. Queried by risk thresholds and scavenger backoff.

use rusqlite::Connection;
use tracing::{warn, error};
// Initialize chronicle table (idempotent) with schema versioning
pub fn init(db_path: &str) -> Result<Connection, String> {
    let db = Connection::open(db_path).map_err(|e| format!("chronicle open: {}", e))?;
    // Set WAL mode for crash recovery + concurrent reads
    if let Err(e) = db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;") {
        warn!("chronicle PRAGMA: {}", e);
    }
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
        CREATE INDEX IF NOT EXISTS idx_chronicle_t ON chronicle(t);
        CREATE INDEX IF NOT EXISTS idx_chronicle_event_t ON chronicle(event, t);
        CREATE INDEX IF NOT EXISTS idx_chronicle_file_t ON chronicle(file, t);
        PRAGMA user_version = 1;"
    ).map_err(|e| format!("chronicle schema: {}", e))?;
    Ok(db)
}

// Append an event to the chronicle
pub fn append(db: &Connection, event: &str, file: &str, detail: &str, outcome: &str) {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if let Err(e) = db.execute(
        "INSERT INTO chronicle (t, event, file, detail, outcome) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![t, event, file, detail, outcome],
    ) {
        error!("chronicle append: {}", e);
    }
}

// Query events for a file in the last N hours
pub fn recent_events(db: &Connection, file: &str, hours: i64) -> Vec<ChronicleEvent> {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64 - hours * 3600;
    let mut stmt = match db.prepare(
        "SELECT t, event, file, detail, outcome FROM chronicle WHERE file = ?1 AND t >= ?2 ORDER BY t DESC LIMIT 50"
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("chronicle prepare failed for file '{}': {}", file, e);
            return Vec::new();
        }
    };
    let rows = match stmt.query_map(rusqlite::params![file, cutoff], |row| {
        Ok(ChronicleEvent {
            t: row.get(0)?,
            event: row.get(1)?,
            file: row.get(2)?,
            detail: row.get(3)?,
            outcome: row.get(4)?,
        })
    }) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("chronicle query_map failed for file '{}': {}", file, e);
            return Vec::new();
        }
    };
    rows.filter_map(|r| r.ok()).collect()
}

// Query events by type in the last N hours
pub fn recent_events_by_type(db: &Connection, event_type: &str, hours: i64) -> Vec<ChronicleEvent> {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64 - hours * 3600;
    let mut stmt = match db.prepare(
        "SELECT t, event, file, detail, outcome FROM chronicle WHERE event = ?1 AND t >= ?2 ORDER BY t DESC LIMIT 100"
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("chronicle prepare by type failed for '{}': {}", event_type, e);
            return Vec::new();
        }
    };
    let rows = match stmt.query_map(rusqlite::params![event_type, cutoff], |row| {
        Ok(ChronicleEvent {
            t: row.get(0)?,
            event: row.get(1)?,
            file: row.get(2)?,
            detail: row.get(3)?,
            outcome: row.get(4)?,
        })
    }) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("chronicle query_map by type failed for '{}': {}", event_type, e);
            return Vec::new();
        }
    };
    rows.filter_map(|r| r.ok()).collect()
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ChronicleEvent {
    pub t: i64,
    pub event: String,
    pub file: String,
    pub detail: String,
    pub outcome: String,
}

pub fn build_prior(workdir: &str) -> String {
    let db_path_str = format!("{}/.reliary/chronicle.sqlite", workdir.trim_end_matches('/'));
    if let Ok(db) = init(&db_path_str) {
        let mut prior = String::new();
        let events = recent_events_by_type(&db, "edit", 24);
        let fails: Vec<_> = events.iter().filter(|e| e.outcome.starts_with("revert")).collect();
        if !fails.is_empty() {
            prior.push_str(&format!("Recent edit failures: {} in last 24h\n", fails.len()));
            for f in fails.iter().take(3) {
                prior.push_str(&format!("  {}: {}\n", f.file, f.outcome));
            }
        }
        
        let mut blocked_identifiers = Vec::new();
        if let Ok(mut stmt) = db.prepare("SELECT detail, COUNT(*) as c FROM chronicle WHERE event = 'veto' GROUP BY detail HAVING c >= 2") {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(row)) = rows.next() {
                    if let Ok(ident) = row.get::<_, String>(0) {
                        blocked_identifiers.push(ident);
                    }
                }
            }
        }
        if !blocked_identifiers.is_empty() {
            prior.push_str(&format!("Blocked identifiers (hallucinated): {}\n", blocked_identifiers.join(", ")));
        }

        let scavenge = recent_events_by_type(&db, "scavenge", 24);
        if !scavenge.is_empty() {
            prior.push_str("Recent scavenger actions:\n");
            for s in scavenge.iter().take(3) {
                prior.push_str(&format!("  {}: {}\n", s.file, s.detail));
            }
        }

        if prior.is_empty() {
            "No prior events".to_string()
        } else {
            prior
        }
    } else {
        "ERROR: chronicle unavailable".to_string()
    }
}
