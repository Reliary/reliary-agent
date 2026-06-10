use std::sync::Arc;
use std::time::Duration;
use crate::session_state::SessionState;
use crate::chronicle;

/// Advisory-only scavenger: scans for orphaned code every 120s, logs to chronicle,
/// never writes to disk. gate.js queries the chronicle on session start and injects
/// an advisory into the system prompt if orphans were found.
pub fn scavenger_loop(state: Arc<SessionState>) {
    loop {
        std::thread::sleep(Duration::from_secs(120));

        if !state.is_scavenger_allowed() { continue; }

        let workdir = state.workdir.to_string_lossy().to_string();
        let db_path = state.chronicle_path.to_string_lossy().to_string();

        let entries = match std::fs::read_dir(&workdir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let mut candidates = Vec::new();
        let config = reliary_dead::DeadConfig::default();

        for entry in entries.flatten() {
            let fp = entry.path();
            let ext = fp.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(ext, "py" | "rs" | "js") { continue; }
            let p = match fp.to_str() { Some(s) => s.to_string(), None => continue };
            let content = match std::fs::read_to_string(&p) { Ok(c) => c, Err(_) => continue };
            candidates.extend(reliary_dead::analyze_file(&p, &content, &config));
        }

        let chronicle_db = match chronicle::init(&db_path) {
            Ok(db) => db,
            Err(_) => continue,
        };

        let new_orphans: Vec<_> = candidates.iter()
            .filter(|c| c.confidence == reliary_dead::Confidence::High)
            .filter(|c| {
                let recent = chronicle::recent_events(&chronicle_db, &c.file, 24);
                !recent.iter().any(|e| e.event == "scavenge_advisory" && e.detail.contains(&c.name))
            })
            .collect();

        if new_orphans.is_empty() { continue; }

        for c in &new_orphans {
            chronicle::append(&chronicle_db, "scavenge_advisory", &c.file, &c.name, "advisory");
        }
        eprintln!("[reliary] scavenger: {} orphaned functions found", new_orphans.len());
    }
}
