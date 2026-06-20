use std::sync::Arc;
use std::time::Duration;
use rayon::prelude::*;
use crate::session_state::SessionState;
use crate::chronicle;

pub fn scavenger_loop(state: Arc<SessionState>) {
    loop {
        std::thread::sleep(Duration::from_secs(120));

        if !state.is_scavenger_allowed() { continue; }

        let workdir = state.workdir.to_string_lossy().to_string();
        let db_path = state.chronicle_path.to_string_lossy().to_string();

        // 1. Parallel incremental re-index for changed files
        crate::reindex::incremental_reindex(&workdir);

        // 2. Collect file paths then scan for dead code in parallel
        let file_tasks: Vec<_> = walkdir::WalkDir::new(&workdir)
            .into_iter()
            .filter_map(|e| e.ok())
            .map(|e| e.path().to_path_buf())
            .filter(|fp| {
                let ext = fp.extension().and_then(|e| e.to_str()).unwrap_or("");
                matches!(ext, "py" | "rs" | "js")
            })
            .collect();

        let config = reliary_dead::DeadConfig::default();

        let candidates: Vec<_> = file_tasks.par_iter().filter_map(|fp| {
            let p = fp.to_str()?;
            let content = std::fs::read_to_string(p).ok()?;
            Some(reliary_dead::analyze_file(p, &content, &config))
        }).flatten().collect();

        let chronicle_db = match chronicle::init(&db_path) {
            Ok(db) => db,
            Err(_) => continue,
        };

        // WAL checkpoint: truncate the WAL file to reclaim disk space (passive mode blocks briefly).
        let _ = chronicle_db.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");

        // edit_cache TTL sweep: delete entries older than 24h to bound table growth.
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64 - 86400;
        let _ = chronicle_db.execute(
            "DELETE FROM edit_cache WHERE timestamp < ?1",
            rusqlite::params![cutoff],
        );

        // Process candidates sequentially (heal calls need serial DB)
        for c in candidates.iter() {
            if c.confidence != reliary_dead::Confidence::High { continue; }
            let recent = chronicle::recent_events(&chronicle_db, &c.file, 24);
            if recent.iter().any(|e| e.event == "scavenge" && e.detail.contains(&c.name)) { continue; }
            if let Ok(content) = std::fs::read_to_string(&c.file) {
                let fixes = vec![(c.name.clone(), String::new())];
                let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);
                if count > 0 && modified != content {
                    match crate::heal::heal_edit(&c.file, &modified, &workdir) {
                        Ok(()) => {
                            let _ = std::fs::write(&c.file, &modified);
                            chronicle::append(&chronicle_db, "scavenge", &c.file, &c.name, "removed");
                            eprintln!("[reliary] scavenger: removed {} from {}", c.name, c.file);
                        }
                        Err(e) => {
                            chronicle::append(&chronicle_db, "scavenge", &c.file, &c.name, &format!("reverted: {}", e));
                        }
                    }
                }
            }
        }
    }
}
