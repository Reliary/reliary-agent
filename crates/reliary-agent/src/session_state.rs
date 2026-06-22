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
    // Bug 73: track last sweep time for time-based cache eviction.
    last_sweep: Mutex<Instant>,
}

impl SessionState {
    pub fn new(workdir: &str) -> Self {
        let base = std::path::PathBuf::from(workdir).join(".reliary");
        let chronicle_path = base.join("chronicle.sqlite");
        if let Err(e) = std::fs::create_dir_all(&base) {
            warn!("session_dir create_dir_all: {}", e);
        }
        Self {
            scavenger_muzzled: AtomicBool::new(true),
            muzzle_time: Mutex::new(Instant::now()),
            chronicle_path,
            workdir: PathBuf::from(workdir),
            read_cache: Mutex::new(FxHashMap::default()),
            risk_cache: Mutex::new(FxHashMap::default()),
            last_sweep: Mutex::new(Instant::now()),
        }
    }

    pub fn read_cache_get(&self, path: &str) -> Option<ReadCacheEntry> {
        self.read_cache.lock().unwrap_or_else(|e| e.into_inner()).get(path).cloned()
    }

    pub fn read_cache_set(&self, path: String, entry: ReadCacheEntry) {
        let mut cache = self.read_cache.lock().unwrap_or_else(|e| e.into_inner());
        // On insert, touch existing entry to update insertion order for LRU-like behavior.
        // FxHashMap doesn't preserve order, so we use remove+reinsert as a poor-man's LRU.
        if (cache.remove(&path).is_some() || cache.len() >= READ_CACHE_MAX)
            && cache.len() >= READ_CACHE_MAX {
                // Evict oldest half (by arbitrary order — true LRU needs LinkedHashMap)
                let evict = cache.len() / 2;
                let keys: Vec<String> = cache.keys().cloned().collect();
                for k in keys.iter().take(evict) {
                    cache.remove(k);
                }
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
        let now = Instant::now();
        // Bug 73: time-based sweep for small caches. Original code only swept
        // when cache.len() was a multiple of 20, which never happens for
        // small (< 20) caches — old entries would leak.
        {
            let last_sweep = self.last_sweep.lock().unwrap_or_else(|e| e.into_inner());
            if cache.len().is_multiple_of(20) || now.duration_since(*last_sweep) > Duration::from_secs(60) {
                cache.retain(|_, (_, t)| now.duration_since(*t) < RISK_CACHE_TTL);
            }
        }
        if cache.len().is_multiple_of(20) {
            let mut last_sweep = self.last_sweep.lock().unwrap_or_else(|e| e.into_inner());
            *last_sweep = now;
        }
        // Evict oldest half if too large
        if cache.len() >= RISK_CACHE_MAX {
            let evict = cache.len() / 2;
            let keys: Vec<String> = cache.keys().cloned().collect();
            for k in keys.iter().take(evict) {
                cache.remove(k);
            }
        }
        cache.insert(path, (risk, now));
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
