/// Shared daemon state for multi-threaded operation.
/// Owned by the daemon, shared across all agents via Arc.

use rustc_hash::FxHashMap;
use std::sync::{atomic::AtomicBool, Mutex};
use std::time::{Duration, Instant, SystemTime};
use std::path::PathBuf;
use rusqlite::Connection;

#[derive(Clone)]
pub struct ReadCacheEntry {
    pub hash: u64,
    pub len: usize,
    pub mtime: SystemTime,
}

/// Per-agent state tracked in memory (persisted to chronicle SQLite on changes)
pub struct SessionState {
    pub scavenger_muzzled: AtomicBool,
    pub muzzle_time: Mutex<Instant>,
    pub workdir: PathBuf,
    pub chronicle_path: PathBuf,
    read_cache: Mutex<FxHashMap<String, ReadCacheEntry>>,
    risk_cache: Mutex<FxHashMap<String, (String, Instant)>>,
    db_conn: Mutex<Option<Connection>>,
    reliary_root: PathBuf,
}

impl SessionState {
    pub fn new(workdir: &str) -> Self {
        let base = std::path::PathBuf::from(workdir).join(".reliary");
        let chronicle_path = base.join("chronicle.sqlite");
        std::fs::create_dir_all(&base).ok();
        Self {
            scavenger_muzzled: AtomicBool::new(false),
            muzzle_time: Mutex::new(Instant::now()),
            chronicle_path,
            workdir: PathBuf::from(workdir),
            read_cache: Mutex::new(FxHashMap::default()),
            risk_cache: Mutex::new(FxHashMap::default()),
            db_conn: Mutex::new(None),
            reliary_root: base,
        }
    }

    /// Get the database connection path for the FTS5 index
    pub fn index_path(&self) -> PathBuf {
        self.reliary_root.join("index.sqlite")
    }

    /// Get a cached FTS5 database connection, opening on first call
    pub fn get_db(&self) -> Option<Connection> {
        let mut guard = self.db_conn.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            if let Ok(db) = Connection::open(&self.reliary_root.join("index.sqlite")) {
                if reliary_search::schema::open_existing_db(&db).is_ok() {
                    *guard = Some(db);
                }
            }
        }
        match &*guard {
            Some(db) => {
                drop(guard);
                let path = self.reliary_root.join("index.sqlite");
                Connection::open(&path).ok().filter(|db| {
                    reliary_search::schema::open_existing_db(db).is_ok()
                })
            }
            None => None,
        }
    }

    pub fn read_cache_get(&self, path: &str) -> Option<ReadCacheEntry> {
        self.read_cache.lock().unwrap_or_else(|e| e.into_inner()).get(path).cloned()
    }

    pub fn read_cache_set(&self, path: String, entry: ReadCacheEntry) {
        self.read_cache.lock().unwrap_or_else(|e| e.into_inner()).insert(path, entry);
    }

    pub fn risk_cache_get(&self, path: &str) -> Option<(String, Instant)> {
        self.risk_cache.lock().unwrap_or_else(|e| e.into_inner()).get(path).cloned()
    }

    pub fn risk_cache_set(&self, path: String, risk: String) {
        self.risk_cache.lock().unwrap_or_else(|e| e.into_inner()).insert(path, (risk, Instant::now()));
    }

    /// Check if scavenger should run (not muzzled, or muzzle expired)
    pub fn is_scavenger_allowed(&self) -> bool {
        if !self.scavenger_muzzled.load(std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        let muzzle_start = self.muzzle_time.lock().unwrap_or_else(|e| e.into_inner());
        if muzzle_start.elapsed() > Duration::from_secs(1800) {
            return true;
        }
        false
    }

    pub fn set_muzzle(&self, on: bool) {
        self.scavenger_muzzled.store(on, std::sync::atomic::Ordering::Relaxed);
        if on {
            *self.muzzle_time.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        }
    }
}
