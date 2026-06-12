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

        // 1. Full index rebuild if any file changed (fast: ~400ms with rayon+mimalloc)
        let index_path = format!("{}/.reliary/index.sqlite", workdir);
        let idx_path = std::path::Path::new(&index_path);
        let needs_rebuild = if idx_path.exists() {
            let idx_mtime = std::fs::metadata(idx_path)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            !recently_modified(&workdir, idx_mtime).is_empty()
        } else {
            true
        };
        if needs_rebuild {
            eprintln!("[reliary] scavenger: files changed — rebuilding index...");
            // index_directory requires a DB connection; open one for it
            let crate_db_path = format!("{}/.reliary/index.sqlite", workdir);
            if let Ok(db) = rusqlite::Connection::open(&crate_db_path) {
                let _ = reliary_search::ingest::index_directory(&db, &workdir);
                eprintln!("[reliary] scavenger: index rebuild complete");
            }
        }

        // 2. Collect file paths then scan for dead code in parallel
        let entries = match std::fs::read_dir(&workdir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let file_tasks: Vec<_> = entries.flatten()
            .map(|e| e.path())
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
                            std::fs::write(&c.file, &modified).ok();
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

/// Check which files under workdir have been modified since a given timestamp.
fn recently_modified(workdir: &str, since: std::time::SystemTime) -> Vec<std::path::PathBuf> {
    let mut changed = Vec::new();
    let skip_dirs = [".git", ".reliary", "node_modules", "target", "__pycache__", ".venv"];
    let supported = ["rs", "py", "js", "ts", "go", "rb", "java", "md", "toml", "yaml", "json"];
    let mut stack = vec![std::path::PathBuf::from(workdir)];
    while let Some(path) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if !skip_dirs.contains(&name) { stack.push(p); }
                } else {
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if supported.contains(&ext) {
                        if let Ok(meta) = p.metadata() {
                            if let Ok(mtime) = meta.modified() {
                                if mtime > since {
                                    changed.push(p);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    changed
}
