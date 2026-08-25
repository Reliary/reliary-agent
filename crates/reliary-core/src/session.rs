/// Session state: accumulated from Pi JSONL session file events
use serde::{Deserialize, Serialize};
use rustc_hash::FxHashMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadRecord {
    pub path: String,
    pub size: usize,
    pub hash: String,
    pub is_rerun: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EditRecord {
    pub file: String,
    pub line: String,
    pub attempt: usize,
    pub old_snippet: String,
    pub new_snippet: String,
    pub success: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ErrorRecord {
    pub turn: usize,
    pub summary: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionState {
    pub turn_count: usize,
    pub reads: Vec<ReadRecord>,
    pub edits: Vec<EditRecord>,
    pub last_test_output: Option<String>,
    pub last_test_pass: bool,
    pub errors: Vec<ErrorRecord>,
    pub file_hashes: FxHashMap<String, u64>,
}

impl SessionState {
    pub fn read_summary(&self) -> Vec<ReadRecord> {
        // D19: Borrow instead of clone. Dedup key is (path, hash) borrowed from the record.
        let mut seen: FxHashMap<(&str, &str), ()> = FxHashMap::default();
        let mut unique: Vec<&ReadRecord> = Vec::with_capacity(self.reads.len());
        for r in self.reads.iter().rev() {
            if seen.insert((r.path.as_str(), r.hash.as_str()), ()).is_none() {
                unique.push(r);
            }
        }
        unique.into_iter().rev().cloned().collect()
    }
}
