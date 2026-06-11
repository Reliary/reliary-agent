/// Append-only project chronicle stored in SQLite.
/// Every daemon action is recorded. Queried by risk thresholds, scavenger backoff,
/// and paper-inspired function-level memory navigation.

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
        CREATE INDEX IF NOT EXISTS idx_chronicle_t ON chronicle(t);
        CREATE INDEX IF NOT EXISTS idx_chronicle_detail ON chronicle(detail);"
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

/// Paper-inspired: Query events associated with a function name in the detail field
pub fn function_memories(db: &Connection, file: &str, func_name: &str, hours: i64) -> Vec<ChronicleEvent> {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64 - hours * 3600;
    let pattern = format!("%{}%", func_name);
    let mut stmt = db.prepare(
        "SELECT t, event, file, detail, outcome FROM chronicle WHERE file = ?1 AND detail LIKE ?2 AND t >= ?3 ORDER BY t DESC LIMIT 20"
    ).unwrap();
    let rows = stmt.query_map(rusqlite::params![file, pattern, cutoff], |row| {
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

/// Paper-inspired: Compute adaptive compression aggressiveness per file
/// Returns a scale 0.0 (compress max) to 1.0 (compress min) based on:
///   - Recent edit failures (more failures = min compression)
///   - Recent veto blocks (fewer = max compression)
///   - Recent successful edits (more = max compression)
pub fn compression_policy(db: &Connection, file: &str) -> f64 {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64 - 86400; // 24h
    if let Ok(mut stmt) = db.prepare(
        "SELECT event, outcome FROM chronicle WHERE file = ?1 AND t >= ?2 AND event IN ('edit', 'veto')"
    ) {
        if let Ok(rows) = stmt.query_map(rusqlite::params![file, cutoff], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            let mut failures = 0_f64;
            let mut vetoes = 0_f64;
            let mut successes = 0_f64;
            for r in rows.flatten() {
                match r.0.as_str() {
                    "edit" if r.1.starts_with("revert") => failures += 1.0,
                    "edit" if r.1 == "pass" => successes += 1.0,
                    "veto" => vetoes += 1.0,
                    _ => {}
                }
            }
            // Scale: more failures = protect (min compress), more success/veto = safe (max compress)
            let fail_penalty = (failures * 0.2).min(0.6);
            let success_bonus = (successes * 0.1).min(0.3);
            let veto_penalty = (vetoes * 0.05).min(0.1);
            let base = 0.3_f64;
            let policy = base + fail_penalty - success_bonus + veto_penalty;
            policy.clamp(0.0, 1.0)
        } else {
            0.3 // default moderate compression
        }
    } else {
        0.3
    }
}

#[derive(Debug, Clone)]
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
