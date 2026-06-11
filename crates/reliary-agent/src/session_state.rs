/// Shared daemon state for multi-threaded operation.
/// Owned by the daemon, shared across all agents via Arc.

use rustc_hash::FxHashMap;
use std::sync::{atomic::AtomicBool, Mutex};
use std::time::{Duration, Instant};
use std::path::PathBuf;

/// Per-agent state tracked in memory (persisted to chronicle SQLite on changes)
pub struct SessionState {
    pub scavenger_muzzled: AtomicBool,
    pub muzzle_time: Mutex<Instant>,
    pub workdir: PathBuf,
    pub chronicle_path: PathBuf,
    pub index_path: PathBuf,
    pub read_cache: Mutex<FxHashMap<String, (u64, usize)>>,
    pub risk_cache: Mutex<FxHashMap<String, (String, Instant)>>,
}

impl SessionState {
    pub fn new(workdir: &str) -> Self {
        let base = PathBuf::from(workdir).join(".reliary");
        let chronicle_path = base.join("chronicle.sqlite");
        let index_path = base.join("index.sqlite");
        std::fs::create_dir_all(&base).ok();
        Self {
            scavenger_muzzled: AtomicBool::new(false),
            muzzle_time: Mutex::new(Instant::now()),
            chronicle_path,
            workdir: PathBuf::from(workdir),
            read_cache: Mutex::new(FxHashMap::default()),
            index_path,
            risk_cache: Mutex::new(FxHashMap::default()),
        }
    }

    /// Check if scavenger should run (not muzzled, or muzzle expired)
    pub fn is_scavenger_allowed(&self) -> bool {
        if !self.scavenger_muzzled.load(std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        // Auto-expire muzzle after 30 minutes (prevents deadlock if gate.js crashes)
        let muzzle_start = self.muzzle_time.lock().unwrap();
        if muzzle_start.elapsed() > Duration::from_secs(1800) {
            return true;
        }
        false
    }

    /// Set muzzle with timeout
    pub fn set_muzzle(&self, on: bool) {
        self.scavenger_muzzled.store(on, std::sync::atomic::Ordering::Relaxed);
        if on {
            *self.muzzle_time.lock().unwrap() = Instant::now();
        }
    }
}
