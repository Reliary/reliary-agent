//! Arc 30 Phase 4 — Background file watcher.
//!
//! Watches a directory for file changes and triggers incremental re-indexing
//! of changed files. Uses the `notify` crate for cross-platform FS events.
//!
//! Design:
//! - `start_watcher(workdir)` spawns a thread that:
//!   1. Detects the project root via `.reliary/index.sqlite`
//!   2. Watches the workdir recursively
//!   3. Records the last-event time per path (trailing-edge debounce)
//!   4. A worker loop reindexes a path only after 500ms of quiet — so a fast
//!      truncate+write pair indexes the FINAL content, not the intermediate
//!      empty state (the old leading-edge debounce did exactly the opposite)
//!   5. Remove events reindex the path with empty content, stripping its rows
//! - The watcher holds an `Arc<Mutex<Vec<ChangeRecord>>>` for status queries.

use notify::{Watcher, RecursiveMode, Event, EventKind, RecommendedWatcher};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
// tracing removed

const DEBOUNCE_MS: u64 = 500;
const TICK_MS: u64 = 100;

/// Record of a single file change processed by the watcher.
#[derive(Clone, Debug)]
pub struct ChangeRecord {
    #[allow(dead_code)]
    pub file: String,
    #[allow(dead_code)]
    pub ts: Instant,
    #[allow(dead_code)]
    pub outcome: String, // "ok" | "error" | "skipped"
}

/// Shared state for the watcher — cloneable handle.
#[derive(Clone)]
pub struct WatcherHandle {
    #[allow(dead_code)]
    pub last_changes: Arc<Mutex<VecDeque<ChangeRecord>>>,
    pub workdir: PathBuf,
    #[allow(dead_code)]
    pub started_at: Instant,
}

impl WatcherHandle {
    #[allow(dead_code)]
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

/// Debounce state: path → last event time. A `removed` flag marks paths whose
/// latest event was a Remove (index them as empty).
struct Pending {
    last: Instant,
    removed: bool,
}

/// Spawn the watcher thread. Returns a handle for status queries.
/// `workdir` should be the directory to watch (typically cwd).
pub fn start_watcher(workdir: &Path) -> Result<WatcherHandle, String> {
    let workdir = workdir.to_path_buf();
    let project_root = detect_project_root(&workdir)
        .ok_or_else(|| format!("no .reliary/index.sqlite in {} (run `reliary trust` first)", workdir.display()))?;
    let watched_dir = project_root.clone();

    let changes = Arc::new(Mutex::new(VecDeque::<ChangeRecord>::new()));
    let _changes_for_handler = changes.clone();
    let project_for_handler = project_root.clone();

    // Trailing-edge debounce state: path → (last event time, removed?).
    let pending: Arc<Mutex<HashMap<PathBuf, Pending>>> = Arc::new(Mutex::new(HashMap::new()));
    let pending_for_handler = pending.clone();

    let mut watcher: RecommendedWatcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
        // V74: a panic in the callback would silently kill the notify thread
        // and the watcher would stop seeing events with no message. Contain it.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match res {
                Ok(event) => record_event(&event, &project_for_handler, &pending_for_handler),
                Err(e) => eprintln!("watcher error: {}", e),
            }
        }));
        if outcome.is_err() {
            eprintln!("watcher: callback panicked (event dropped, watcher continues)");
        }
    }) {
        Ok(w) => w,
        Err(e) => return Err(format!("create watcher: {}", e)),
    };

    if let Err(e) = watcher.watch(&watched_dir, RecursiveMode::Recursive) {
        return Err(format!("watch {}: {}", watched_dir.display(), e));
    }

    eprintln!("watching {} (trailing debounce {}ms)", watched_dir.display(), DEBOUNCE_MS);

    // Keep watcher alive on its own thread, and run the debounce worker there.
    let started = Instant::now();
    let project_for_worker = project_root.clone();
    let changes_for_worker = changes.clone();
    std::thread::spawn(move || {
        let _w = watcher; // keep alive until program exit
        loop {
            std::thread::sleep(Duration::from_millis(TICK_MS));
            // Collect paths that have been quiet for the debounce window.
            let due: Vec<(PathBuf, bool)> = {
                let mut map = pending.lock().unwrap_or_else(|p| p.into_inner());
                let now = Instant::now();
                let due_paths: Vec<PathBuf> = map
                    .iter()
                    .filter(|(_, p)| now.duration_since(p.last) >= Duration::from_millis(DEBOUNCE_MS))
                    .map(|(k, _)| k.clone())
                    .collect();
                due_paths
                    .into_iter()
                    .filter_map(|k| map.remove(&k).map(|p| (k, p.removed)))
                    .collect()
            };
            for (path, removed) in due {
                let path_str = path.to_string_lossy().to_string();
                let content = if removed { String::new() } else {
                    match std::fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("read {}: {}", path_str, e);
                            push_change(&changes_for_worker, &path_str, "error:read");
                            continue;
                        }
                    }
                };
                let db_path = project_for_worker.join(".reliary").join("index.sqlite");
                let count = crate::reindex::reindex_single_file(
                    &db_path.to_string_lossy(),
                    &path_str,
                    &content,
                );
                let outcome = if removed {
                    format!("ok:removed({}phrases)", count)
                } else {
                    format!("ok:{}phrases", count)
                };
                push_change(&changes_for_worker, &path_str, &outcome);
                eprintln!("watch: reindexed {} ({})", path_str, outcome);
            }
        }
    });

    Ok(WatcherHandle {
        last_changes: changes,
        workdir: watched_dir,
        started_at: started,
    })
}

fn push_change(changes: &Arc<Mutex<VecDeque<ChangeRecord>>>, file: &str, outcome: &str) {
    if let Ok(mut v) = changes.lock() {
        v.push_back(ChangeRecord {
            file: file.to_string(),
            ts: Instant::now(),
            outcome: outcome.to_string(),
        });
        if v.len() > 100 { v.pop_front(); }
    }
}

/// Record an event for trailing-edge debounce. NO reindexing happens here —
/// the worker loop processes paths after they go quiet.
fn record_event(
    event: &Event,
    project_root: &Path,
    pending: &Arc<Mutex<HashMap<PathBuf, Pending>>>,
) {
    let is_change = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    );
    if !is_change {
        return;
    }
    let removed = matches!(event.kind, EventKind::Remove(_));
    for path in &event.paths {
        // Skip files inside .reliary/ — don't re-index the index.
        if path.starts_with(project_root.join(".reliary")) {
            continue;
        }
        // For creations/modifications the file must exist and be text-like.
        // Removals are always recorded (the worker indexes empty content).
        if !removed
            && (!path.exists() || !is_supported(path)) {
                continue;
            }
        let mut map = pending.lock().unwrap_or_else(|p| p.into_inner());
        // A Modify after a Remove (or vice versa) keeps the LATEST intent.
        map.insert(path.clone(), Pending { last: Instant::now(), removed });
    }
}
