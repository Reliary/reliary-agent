/// HDC memory: 10K-bit hypervectors with Hebbian updates, SQLite persistence.
/// Ported from cortex-rs (github.com/Reliary/cortex-rs).

use std::collections::HashMap;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub type Hypervector = Vec<i8>;

/// Generate a deterministic random hypervector from a seed
pub fn make_hv(seed: u64, dims: usize) -> Hypervector {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..dims).map(|_| if rng.gen_bool(0.5) { 1 } else { -1 }).collect()
}

/// Bundle: sum + bipolar clamp
pub fn bundle(hv: &mut Hypervector, other: &Hypervector) {
    for (a, b) in hv.iter_mut().zip(other) {
        *a += b;
    }
}

/// Bipolar clamp: limit to {-1, 0, +1}
pub fn bipolar_clamp(hv: &mut Hypervector) {
    for v in hv.iter_mut() {
        *v = v.signum();
    }
}

/// Dot product of two hypervectors (cosine similarity)
pub fn dot(a: &Hypervector, b: &Hypervector) -> f64 {
    let dot: i64 = a.iter().zip(b).map(|(x, y)| (*x as i64) * (*y as i64)).sum();
    let norm_a = (a.iter().map(|x| (*x as i64).pow(2)).sum::<i64>() as f64).sqrt();
    let norm_b = (b.iter().map(|x| (*x as i64).pow(2)).sum::<i64>() as f64).sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { 0.0 } else { dot as f64 / (norm_a * norm_b) }
}

/// Memory record
#[derive(Debug, Clone)]
pub struct MemoryRecord {
    pub id: i64,
    pub content: String,
    pub source: String,
    pub timestamp: i64,
    pub tier: i32,      // 0=episodic, 1=semantic, 2=consolidated
    pub recall_count: i32,
    pub error_flag: i32,
    pub entropy: f64,
}

/// Scored memory (from a search query)
#[derive(Debug, Clone)]
pub struct ScoredMemory {
    pub memory: MemoryRecord,
    pub score: f64,
}

/// Open or create a persistent SQLite-backed memory store
pub fn open_persistent(path: &str) -> Result<MemoryStore, String> {
    let conn = rusqlite::Connection::open(path).map_err(|e| format!("DB: {}", e))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cortex_memories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT DEFAULT '',
            timestamp INTEGER NOT NULL,
            tier INTEGER DEFAULT 0,
            recall_count INTEGER DEFAULT 0
        );"
    ).map_err(|e| format!("schema: {}", e))?;

    let mut store = MemoryStore::new(100);

    let mut stmt = conn.prepare("SELECT id, content, source, timestamp, tier, recall_count, 0, 0.0 FROM cortex_memories ORDER BY id").map_err(|e| format!("query: {}", e))?;
    let rows = stmt.query_map([], |row| {
        Ok(MemoryRecord {
            id: row.get(0)?,
            content: row.get(1)?,
            source: row.get(2)?,
            timestamp: row.get(3)?,
            tier: row.get(4)?,
            recall_count: row.get(5)?,
            error_flag: row.get(6)?,
            entropy: row.get(7)?,
        })
    }).map_err(|e| format!("rows: {}", e))?;

    for row in rows.flatten() {
        store.memories.push(row);
    }
    Ok(store)
}

/// Save all memories to SQLite
pub fn save_persistent(store: &MemoryStore, path: &str) -> Result<(), String> {
    let conn = rusqlite::Connection::open(path).map_err(|e| format!("DB: {}", e))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cortex_memories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT DEFAULT '',
            timestamp INTEGER NOT NULL,
            tier INTEGER DEFAULT 0,
            recall_count INTEGER DEFAULT 0
        );"
    ).map_err(|e| format!("schema: {}", e))?;

    conn.execute("DELETE FROM cortex_memories", []).ok();
    for m in &store.memories {
        conn.execute(
            "INSERT INTO cortex_memories (content, source, timestamp, tier, recall_count) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![m.content, m.source, m.timestamp, m.tier, m.recall_count],
        ).ok();
    }
    Ok(())
}

pub struct MemoryStore {
    pub memories: Vec<MemoryRecord>,
    token_hvs: HashMap<String, Hypervector>,
    cooccur: HashMap<(String, String), u64>,
    pub dims: usize,
}

impl MemoryStore {
    pub fn new(dims: usize) -> Self {
        Self { memories: Vec::new(), token_hvs: HashMap::new(), cooccur: HashMap::new(), dims }
    }    pub fn ensure_token_hv(&mut self, token: &str) -> Hypervector {
        let dims = self.dims;
        self.token_hvs.entry(token.to_string())
            .or_insert_with(|| make_hv(token.as_bytes().len() as u64, dims))
            .clone()
    }

    /// Ingest a memory: tokenize, encode, store, update co-occurrence
    pub fn retain(&mut self, content: &str, source: &str, error: bool) -> i64 {
        let tokens = scan_tokens(content);
        if tokens.is_empty() { return 0; }

        // Co-occurrence update
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len().min(i + 5) {
                let key = (tokens[i].clone(), tokens[j].clone());
                *self.cooccur.entry(key).or_insert(0) += 1;
                // Hebbian: pull vectors toward each other
                self.hebbian_update(&tokens[i], &tokens[j]);
            }
        }

        let now = chrono::Utc::now().timestamp();
        let id = self.memories.len() as i64 + 1;
        self.memories.push(MemoryRecord {
            id,
            content: content.to_string(),
            source: source.to_string(),
            timestamp: now,
            tier: 0,
            recall_count: 0,
            error_flag: if error { 1 } else { 0 },
            entropy: 0.0,
        });
        id
    }

    fn hebbian_update(&mut self, a: &str, b: &str) {
        let mut hv_a = self.ensure_token_hv(a);
        let mut hv_b = self.ensure_token_hv(b);
        let epsilon = 0.1;
        for i in 0..self.dims {
            hv_a[i] = (hv_a[i] as f64 + epsilon * hv_b[i] as f64) as i8;
            hv_b[i] = (hv_b[i] as f64 + epsilon * hv_a[i] as f64) as i8;
        }
        bipolar_clamp(&mut hv_a);
        bipolar_clamp(&mut hv_b);
        // Store back (via ensure_token_hv mutable reference)
        if let Some(stored) = self.token_hvs.get_mut(a) {
            *stored = hv_a;
        }
        if let Some(stored) = self.token_hvs.get_mut(b) {
            *stored = hv_b;
        }
    }

    /// Encode a query into a hypervector (sum of token HVs)
    pub fn encode_query(&self, query: &str) -> Hypervector {
        let tokens = scan_tokens(query);
        let mut hv = vec![0i8; self.dims];
        for t in tokens {
            if let Some(thv) = self.token_hvs.get(&t) {
                bundle(&mut hv, thv);
            }
        }
        hv
    }

    /// Search memories by query similarity
    pub fn recall(&self, query: &str, top_n: usize) -> Vec<ScoredMemory> {
        if self.memories.is_empty() { return vec![]; }
        let q_hv = self.encode_query(query);
        let mut scored: Vec<ScoredMemory> = self.memories.iter().map(|m| {
            let m_hv = self.encode_query(&m.content);
            let score = dot(&q_hv, &m_hv);
            ScoredMemory { memory: m.clone(), score }
        }).collect();
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_n);
        scored
    }

    /// Co-occurrence prediction: given query tokens, predict most associated tokens
    pub fn predict(&self, query: &str, top_n: usize) -> Vec<(String, u64)> {
        let tokens = scan_tokens(query);
        let mut scores: HashMap<String, u64> = HashMap::new();
        for t in &tokens {
            for ((a, b), count) in &self.cooccur {
                if a == t { *scores.entry(b.clone()).or_insert(0) += count; }
                if b == t { *scores.entry(a.clone()).or_insert(0) += count; }
            }
        }
        let mut sorted: Vec<(String, u64)> = scores.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted.truncate(top_n);
        sorted
    }

    /// Promote memories to higher tiers
    pub fn consolidate(&mut self) {
        let now = chrono::Utc::now().timestamp();
        for m in &mut self.memories {
            if m.recall_count >= 10 && m.tier < 2 { m.tier = 2; }
            if m.recall_count >= 3 && m.tier < 1 && now - m.timestamp < 7 * 86400 { m.tier = 1; }
        }
    }
}

/// Grammar-free identifier scanning
fn scan_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 3 && t.chars().any(|c| c.is_alphabetic()))
        .map(|t| t.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_hv() {
        let hv = make_hv(42, 10000);
        assert_eq!(hv.len(), 10000);
        assert!(hv.iter().any(|&x| x == 1));
        assert!(hv.iter().any(|&x| x == -1));
    }

    #[test]
    fn test_bundle_and_clamp() {
        let mut a = vec![1i8; 100];
        let b = vec![-1i8; 100];
        bundle(&mut a, &b);
        for v in &a { assert_eq!(*v, 0i8); }
    }

    #[test]
    fn test_retain_and_recall() {
        let mut store = MemoryStore::new(100);
        store.retain("Alice works at Google", "user", false);
        let results = store.recall("Alice", 5);
        assert_eq!(results.len(), 1);
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn test_prediction() {
        let mut store = MemoryStore::new(100);
        store.retain("Alice works at Google", "user", false);
        store.retain("Bob works at Google too", "user", false);
        let preds = store.predict("Alice", 5);
        assert!(!preds.is_empty());
        assert!(preds.iter().any(|(t, _)| t.contains("google")));
    }

    #[test]
    fn test_empty_store() {
        let store = MemoryStore::new(100);
        assert!(store.recall("anything", 5).is_empty());
    }
}
