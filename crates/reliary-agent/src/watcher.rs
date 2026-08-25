//! Arc 30 Phase 4 — Background file watcher.
//!
//! Watches a directory for file changes and triggers incremental re-indexing
//! of changed files. Uses the `notify` crate for cross-platform FS events.
//!
//! Design:
//! - `start_watcher(workdir)` spawns a thread that:
//!   1. Detects the project root via `.reliary/index.sqlite`
//!   2. Watches the workdir recursively
//!   3. On file event: debounce 500ms, then reindex the changed file
//!   4. Logs the re-index event to stderr
//! - The watcher holds an `Arc<Mutex<Vec<ChangeRecord>>>` for status queries.

use notify::{Watcher, RecursiveMode, Event, EventKind, RecommendedWatcher};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
// tracing removed

/// Record of a single file change processed by the watcher.
#[derive(Clone, Debug)]
pub struct ChangeRecord {
    pub file: String,
    pub ts: Instant,
    pub outcome: String, // "ok" | "error" | "skipped"
}

/// Shared state for the watcher — cloneable handle.
#[derive(Clone)]
pub struct WatcherHandle {
    pub last_changes: Arc<Mutex<VecDeque<ChangeRecord>>>,
    pub workdir: PathBuf,
    pub started_at: Instant,
}

impl WatcherHandle {
    pub fn recent_changes(&self) -> Vec<ChangeRecord> {
        self.last_changes.lock().map(|v| v.iter().cloned().collect()).unwrap_or_default()
    }
}

fn is_supported(path: &Path) -> bool {
    // Grammar-free content check (Arc 39): skip only binary files.
    // Anything that looks like text is supported, regardless of extension.
    !reliary_search::is_likely_binary(path, 8192)
}

fn detect_project_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        // Prefer .git/ as the project marker (works even before auto-trust)
        if cur.join(".git").exists() {
            return Some(cur);
        }
        // Fall back to existing .reliary/ (for non-git repos already trusted)
        if cur.join(".reliary").join("index.sqlite").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Spawn the watcher thread. Returns a handle for status queries.
/// `workdir` should be the directory to watch (typically cwd).
pub fn start_watcher(workdir: &Path) -> Result<WatcherHandle, String> {
    let workdir = workdir.to_path_buf();
    let project_root = detect_project_root(&workdir)
        .ok_or_else(|| format!("no .reliary/index.sqlite in {} (run `reliary trust` first)", workdir.display()))?;
    let watched_dir = project_root.clone();

    let changes = Arc::new(Mutex::new(VecDeque::<ChangeRecord>::new()));
    let changes_for_handler = changes.clone();
    let project_for_handler = project_root.clone();

    // Map of path → last event time (debounce).
    let debounce: Arc<Mutex<HashMap<PathBuf, Instant>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut watcher: RecommendedWatcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
        match res {
            Ok(event) => handle_event(&event, &changes_for_handler, &project_for_handler, &debounce),
            Err(e) => eprintln!("watcher error: {}", e),
        }
    }) {
        Ok(w) => w,
        Err(e) => return Err(format!("create watcher: {}", e)),
    };

    if let Err(e) = watcher.watch(&watched_dir, RecursiveMode::Recursive) {
        return Err(format!("watch {}: {}", watched_dir.display(), e));
    }

    eprintln!("watching {} (debounce 500ms)", watched_dir.display());

    // Keep watcher alive on its own thread.
    let started = Instant::now();
    let handle = WatcherHandle {
        last_changes: changes,
        workdir: watched_dir,
        started_at: started,
    };

    // Move watcher into a thread. The thread runs until the program exits.
    std::thread::spawn(move || {
        // Keep the watcher in scope until the thread is asked to stop (program exit).
        let _w = watcher;
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    });

    Ok(handle)
}

fn handle_event(
    event: &Event,
    changes: &Arc<Mutex<VecDeque<ChangeRecord>>>,
    project_root: &PathBuf,
    debounce: &Arc<Mutex<HashMap<PathBuf, Instant>>>,
) {
    // Filter to relevant events.
    let is_data_change = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_)
    );
    if !is_data_change {
        return;
    }
    for path in &event.paths {
        if !path.exists() || !is_supported(path) {
            continue;
        }
        // Skip files inside .reliary/ — don't re-index the index.
        if path.starts_with(project_root.join(".reliary")) {
            continue;
        }
        // Debounce per-file (500ms).
        let should_process = {
            let mut db = debounce.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let now = Instant::now();
            let last = db.get(path).copied();
            db.insert(path.clone(), now);
            match last {
                Some(t) => now.duration_since(t) > Duration::from_millis(500),
                None => true,
            }
        };
        if !should_process { continue; }

        let path_str = path.to_string_lossy().to_string();
        let outcome = match std::fs::read_to_string(path) {
            Ok(content) => {
                let db_path = project_root.join(".reliary").join("index.sqlite");
                let count = crate::reindex::reindex_single_file(
                    &db_path.to_string_lossy(),
                    &path_str,
                    &content,
                );
                format!("ok:{}phrases", count)
            }
            Err(e) => {
                eprintln!("read {}: {}", path_str, e);
                "error:read".to_string()
            }
        };
        if let Ok(mut v) = changes.lock() {
            v.push_back(ChangeRecord {
                file: path_str.clone(),
                ts: Instant::now(),
                outcome: outcome.clone(),
            });
            // Keep only last 100.
            if v.len() > 100 { v.pop_front(); }
        }
        eprintln!("watch: reindexed {} ({})", path_str, outcome);
    }
}