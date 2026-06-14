/// Session state: accumulated from Pi JSONL session file events
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub file_hashes: HashMap<String, u64>,
}

impl SessionState {
    pub fn read_summary(&self) -> Vec<ReadRecord> {
        let mut seen = HashMap::new();
        let mut unique: Vec<ReadRecord> = Vec::new();
        for r in self.reads.iter().rev() {
            let key = format!("{}{}", r.path, r.hash);
            if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(key) {
                e.insert(true);
                unique.push(r.clone());
            }
        }
        unique.into_iter().rev().collect()
    }

    pub fn edited_files(&self) -> Vec<String> {
        let mut files: Vec<String> = self.edits.iter().map(|e| e.file.clone()).collect();
        files.sort();
        files.dedup();
        files
    }
}
