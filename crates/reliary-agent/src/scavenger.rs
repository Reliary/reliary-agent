use std::sync::Arc;
use std::time::{Duration, Instant};
use rayon::prelude::*;
use crate::session_state::SessionState;
use crate::chronicle;

pub fn scavenger_loop(state: Arc<SessionState>) {
    let mut sleep_secs: u64 = 120;
    loop {
        std::thread::sleep(Duration::from_secs(sleep_secs));

        if !state.is_scavenger_allowed() { continue; }

        let workdir = state.workdir.to_string_lossy().to_string();
        let db_path = state.chronicle_path.to_string_lossy().to_string();
        let cycle_start = Instant::now();

        // 1. Parallel incremental re-index for changed files
        crate::reindex::incremental_reindex(&workdir);

        // 2. Collect file paths then scan for dead code in parallel
        let file_tasks: Vec<_> = walkdir::WalkDir::new(&workdir)
            .follow_links(false)
            .max_depth(20)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() {
                    let name = e.file_name().to_string_lossy();
                    !matches!(name.as_ref(),
                        ".git" | ".reliary" | "node_modules" | "target" | "__pycache__"
                        | ".venv" | ".cargo" | ".rustup" | ".npm" | ".cache"
                        | ".local" | "venv" | ".next" | "dist" | "build"
                        | "vendor" | "bundle" | ".bundle")
                } else {
                    true
                }
            })
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
        let _ = chronicle_db.execute_batch(" wal_checkpoint();");

        // edit_cache TTL sweep: delete entries older than 24h to bound table growth.
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64 - 86400;
        let _ = chronicle_db.execute(
            "DELETE FROM edit_cache WHERE timestamp < ?1",
            rusqlite::params![cutoff],
        );

        // WSL2 drvfs detection: skip heal subprocess (cargo test) on /mnt/ paths
        let on_drvfs = workdir.starts_with("/mnt/");

        // Process candidates sequentially (heal calls need serial DB)
        for c in candidates.iter() {
            if c.confidence != reliary_dead::Confidence::High { continue; }
            let recent = chronicle::recent_events(&chronicle_db, &c.file, 24);
            if recent.iter().any(|e| e.event == "scavenge" && e.detail.contains(&c.name)) { continue; }
            if let Ok(content) = std::fs::read_to_string(&c.file) {
                let fixes = vec![(c.name.clone(), String::new())];
                let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);
                if count > 0 && modified != content {
                    let result = if on_drvfs {
                        Err("WSL2 drvfs: heal skipped".into())
                    } else {
                        crate::heal::heal_edit(&c.file, &modified, &workdir)
                    };
                    match result {
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

        // Adaptive backoff: if cycle took longer than 60s, double sleep interval
        let elapsed = cycle_start.elapsed().as_secs();
        if elapsed > 60 {
            sleep_secs = (sleep_secs * 2).min(1800); // max 30 min
        } else {
            sleep_secs = 120;
        }
    }
}
