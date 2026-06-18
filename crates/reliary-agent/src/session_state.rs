//! Shared daemon state for multi-threaded operation.
// Owned by the daemon, shared across all agents via Arc.

use rustc_hash::FxHashMap;
use std::sync::{atomic::AtomicBool, Mutex};
use std::time::{Duration, Instant};
use tracing::warn;
use std::path::PathBuf;

const RISK_CACHE_TTL: Duration = Duration::from_secs(300);
const RISK_CACHE_MAX: usize = 500;
const READ_CACHE_MAX: usize = 200;

#[derive(Clone)]
pub struct ReadCacheEntry {
    pub hash: u64,
    pub len: usize,
}

// Per-agent state tracked in memory (persisted to chronicle SQLite on changes)
pub struct SessionState {
    pub scavenger_muzzled: AtomicBool,
    pub muzzle_time: Mutex<Instant>,
    pub workdir: PathBuf,
    pub chronicle_path: PathBuf,
    read_cache: Mutex<FxHashMap<String, ReadCacheEntry>>,
    risk_cache: Mutex<FxHashMap<String, (String, Instant)>>,
    // File co-occurrence: tracks which files are read after which other files.
    // Key: (previous_file, next_file) -> count
    file_cooccur: Mutex<FxHashMap<(String, String), u64>>,
    // Last file read in the session, for co-occurrence prediction
    last_file: Mutex<String>,
}

impl SessionState {
    pub fn new(workdir: &str) -> Self {
        let base = std::path::PathBuf::from(workdir).join(".reliary");
        let chronicle_path = base.join("chronicle.sqlite");
        if let Err(e) = std::fs::create_dir_all(&base) {
            warn!("session_dir create_dir_all: {}", e);
        }
        Self {
            scavenger_muzzled: AtomicBool::new(false),
            muzzle_time: Mutex::new(Instant::now()),
            chronicle_path,
            workdir: PathBuf::from(workdir),
            read_cache: Mutex::new(FxHashMap::default()),
            risk_cache: Mutex::new(FxHashMap::default()),
            file_cooccur: Mutex::new(FxHashMap::default()),
            last_file: Mutex::new(String::new()),
        }
    }

    /// Record that `file` was read. Updates co-occurrence with the previous file.
    pub fn record_file_read(&self, file: &str) {
        let mut last = self.last_file.lock().unwrap_or_else(|e| e.into_inner());
        if !last.is_empty() && *last != file {
            let mut co = self.file_cooccur.lock().unwrap_or_else(|e| e.into_inner());
            *co.entry((last.clone(), file.to_string())).or_insert(0) += 1;
        }
        *last = file.to_string();
    }

    /// Predict next files based on co-occurrence with the last file read.
    /// Returns file paths sorted by co-occurrence count, descending.
    pub fn predict_files(&self, top_n: usize) -> Vec<(String, u64)> {
        let last = self.last_file.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_empty() { return vec![]; }
        let co = self.file_cooccur.lock().unwrap_or_else(|e| e.into_inner());
        let mut results: Vec<(String, u64)> = co.iter()
            .filter(|((a, _), _)| a == &*last)
            .map(|((_, b), c)| (b.clone(), *c))
            .collect();
        results.sort_by_key(|b| std::cmp::Reverse(b.1));
        results.truncate(top_n);
        results
    }

    pub fn read_cache_get(&self, path: &str) -> Option<ReadCacheEntry> {
        self.read_cache.lock().unwrap_or_else(|e| e.into_inner()).get(path).cloned()
    }

    pub fn read_cache_set(&self, path: String, entry: ReadCacheEntry) {
        let mut cache = self.read_cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= READ_CACHE_MAX {
            // Evict oldest half
            let evict = cache.len() / 2;
            let mut removed = 0;
            cache.retain(|_, _| {
                removed += 1;
                removed > evict
            });
        }
        cache.insert(path, entry);
    }

    pub fn risk_cache_get(&self, path: &str) -> Option<(String, Instant)> {
        let cache = self.risk_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((risk, time)) = cache.get(path) {
            if time.elapsed() < RISK_CACHE_TTL {
                return Some((risk.clone(), *time));
            }
        }
        None
    }

    pub fn risk_cache_set(&self, path: String, risk: String) {
        let mut cache = self.risk_cache.lock().unwrap_or_else(|e| e.into_inner());
        // Evict expired entries
        let now = Instant::now();
        cache.retain(|_, (_, t)| now.duration_since(*t) < RISK_CACHE_TTL);
        // Evict oldest half if still too large
        if cache.len() >= RISK_CACHE_MAX {
            let evict = cache.len() / 2;
            let mut removed = 0;
            cache.retain(|_, _| {
                removed += 1;
                removed > evict
            });
        }
        cache.insert(path, (risk, Instant::now()));
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
