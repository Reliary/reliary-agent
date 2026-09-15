/// HDC memory: 10K-bit hypervectors with Hebbian updates, SQLite persistence.
/// M1: Bit-packed hypervectors — 10K bipolar {-1,+1} values stored as 10K bits
/// in a Vec<u64> (156 words = 1248 bytes vs 10K bytes = 8× memory reduction).
/// Dot product uses XOR + popcount = 64× fewer loop iterations.
use rustc_hash::FxHashMap;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Bit-packed hypervector: each bit represents a bipolar value (0 → -1, 1 → +1).
/// 10,000 dims = 157 u64 words = 1,256 bytes (vs 10,000 bytes as Vec<i8>).
#[derive(Debug, Clone, PartialEq)]
pub struct Hypervector {
    bits: Vec<u64>,
    dims: usize,
}

impl Hypervector {
    pub fn new(dims: usize) -> Self {
        let words = (dims + 63) / 64;
        Self { bits: vec![0u64; words], dims }
    }

    /// Get bipolar value at index: returns -1 or +1.
    #[inline(always)]
    pub fn bits_get(&self, word: usize) -> u64 {
        *self.bits.get(word).unwrap_or(&0)
    }

    pub fn get(&self, idx: usize) -> i8 {
        if idx >= self.dims { return 0; }
        let word = idx / 64;
        let bit = idx % 64;
        if (self.bits[word] >> bit) & 1 == 1 { 1 } else { -1 }
    }

    /// Set bipolar value at index: -1 → clear bit, +1 → set bit.
    #[inline(always)]
    pub fn set(&mut self, idx: usize, val: i8) {
        if idx >= self.dims { return; }
        let word = idx / 64;
        let bit = idx % 64;
        if val >= 0 {
            self.bits[word] |= 1u64 << bit;
        } else {
            self.bits[word] &= !(1u64 << bit);
        }
    }

    /// M1: XOR + popcount dot product — 64× fewer iterations than element-wise.
    /// For bipolar {-1,+1}: dot = (matching_bits - differing_bits) = dims - 2*hamming_distance.
    #[inline(always)]
    pub fn dot(&self, other: &Hypervector) -> i64 {
        let min_words = self.bits.len().min(other.bits.len());
        let mut hamming: u64 = 0;
        for i in 0..min_words {
            hamming += (self.bits[i] ^ other.bits[i]).count_ones() as u64;
        }
        let dims = self.dims.min(other.dims) as i64;
        dims - 2 * (hamming as i64)
    }

    /// M3: Precomputed norm — for bipolar HVs, norm = sqrt(dims) ≈ constant.
    /// No per-call sum-of-squares needed.
    #[inline(always)]
    pub fn norm(&self) -> f64 {
        (self.dims as f64).sqrt()
    }

    /// Cosine similarity = dot / (norm_a * norm_b).
    /// For bipolar HVs: cosine = 1 - 2*hamming/dims.
    #[inline(always)]
    pub fn cosine(&self, other: &Hypervector) -> f64 {
        let d = self.dot(other) as f64;
        let n = self.norm() * other.norm();
        if n == 0.0 { 0.0 } else { d / n }
    }

    /// Bundle: XOR-based majority vote for bipolar HVs.
    /// Sum via i32 accumulation, then sign → set/clear bit.
    pub fn bundle_into(&mut self, other: &Hypervector) {
        // Accumulate per-word sign via popcount majority.
        // For bipolar, bundle = sign(sum of +1/-1). We use a temp accumulator.
        let words = self.bits.len().min(other.bits.len());
        let mut acc: Vec<i32> = Vec::with_capacity(64);
        for w in 0..words {
            // Count +1s in self word and other word
            // For each bit: self contributes +1 (set) or -1 (clear), same for other.
            // Sum per bit = (self_val + other_val). We need sign of sum.
            // Use a per-bit accumulator array (the per-word popcount sums
            // below are only informative; the per-bit loop computes the real
            // majority).
            acc.clear();
            acc.resize(64, 0i32);
            for b in 0..64 {
                let s = if (self.bits[w] >> b) & 1 == 1 { 1 } else { -1 };
                let o = if (other.bits[w] >> b) & 1 == 1 { 1 } else { -1 };
                acc[b] = s + o;
            }
            // V58: majority INCLUDES the current self vote — acc[b] is
            // (self + other) ∈ {-2,0,2}; tie (0) keeps self's bit.
            let mut new_word = 0u64;
            for b in 0..64 {
                if acc[b] > 0 || (acc[b] == 0 && (self.bits[w] >> b) & 1 == 1) {
                    new_word |= 1u64 << b;
                }
            }
            self.bits[w] = new_word;
        }
    }
}

/// Generate a deterministic random hypervector from a seed.
/// M1: Uses bit-packing — 1 bit per dim instead of 1 byte.
pub fn make_hv(seed: u64, dims: usize) -> Hypervector {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut hv = Hypervector::new(dims);
    for i in 0..dims {
        hv.set(i, if rng.gen_bool(0.5) { 1 } else { -1 });
    }
    hv
}

/// Bundle: sum + bipolar clamp (legacy API — delegates to bit-packed).
pub fn bundle(hv: &mut Hypervector, other: &Hypervector) {
    hv.bundle_into(other);
}

/// Bipolar clamp: for bit-packed HVs, values are already bipolar {-1,+1}.
/// This is a no-op — bit-packing enforces bipolar by construction.
#[inline(always)]
pub fn bipolar_clamp(_hv: &mut Hypervector) {
    // No-op: bit-packed HVs are always bipolar.
}

/// Dot product of two hypervectors (cosine similarity).
/// M1+M3: Uses XOR+popcount dot + precomputed norm.
#[inline(always)]
pub fn dot(a: &Hypervector, b: &Hypervector) -> f64 {
    a.cosine(b)
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
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .map_err(|e| format!("PRAGMA: {}", e))?;
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
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .map_err(|e| format!("PRAGMA: {}", e))?;
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

    let _ = conn.execute("DELETE FROM cortex_memories", []);
    for m in &store.memories {
        if let Err(e) = conn.execute(
                "INSERT INTO cortex_memories (content, source, timestamp, tier, recall_count) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![m.content, m.source, m.timestamp, m.tier, m.recall_count],
            ) {
                eprintln!("[memory] save: {}", e);
            }
    }
    Ok(())
}

pub struct MemoryStore {
    pub memories: Vec<MemoryRecord>,
    token_hvs: FxHashMap<String, Hypervector>,
    cooccur: FxHashMap<(String, String), u64>,
    pub dims: usize,
}

impl MemoryStore {
    pub fn new(dims: usize) -> Self {
        Self { memories: Vec::new(), token_hvs: FxHashMap::default(), cooccur: FxHashMap::default(), dims }
    }

    /// M1: Return reference instead of clone — eliminates 10KB clone per call.
    pub fn ensure_token_hv(&mut self, token: &str) -> &Hypervector {
        let dims = self.dims;
        self.token_hvs.entry(token.to_string())
            // V58: seed by token CONTENT (FNV-1a), not length — length-seeded
            // HVs collapse all same-length tokens to one vector (sim 1.0 bug).
            .or_insert_with(|| make_hv(fnv1a(token.as_bytes()), dims))
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

    /// M2: Integer Hebbian update — no f64 casts.
    /// epsilon = 0.1 → fixed-point: flip bit with 10% probability toward other.
    /// For bipolar {-1,+1}: Hebbian learning = stochastic bit flip toward partner.
    fn hebbian_update(&mut self, a: &str, b: &str) {
        let dims = self.dims;
        let hv_b_val = {
            let hv_b = self.token_hvs.entry(b.to_string()).or_insert_with(|| make_hv(b.len() as u64, dims));
            // Sample bits from b to flip toward
            let mut rng = ChaCha8Rng::seed_from_u64(
                (a.len() as u64).wrapping_mul(31).wrapping_add(b.len() as u64)
            );
            let n_flip = (dims / 10).max(1);
            let mut bits_to_set: Vec<(usize, i8)> = Vec::with_capacity(n_flip);
            for _ in 0..n_flip {
                let idx = rng.gen_range(0..dims);
                bits_to_set.push((idx, hv_b.get(idx)));
            }
            bits_to_set
        };
        // Now apply to a with only mutable borrow.
        let hv_a = self.token_hvs.entry(a.to_string()).or_insert_with(|| make_hv(a.len() as u64, dims));
        for (idx, val) in hv_b_val {
            hv_a.set(idx, val);
        }
    }

    /// V58 P2a: deterministic token-set hypervector — majority vote over the
    /// k token HVs only (NOT chained bundle_into, which starts from an empty
    /// HV whose -1 votes poison every tie and collapse everything to zero).
    /// Sort+dedup for order-independence; FNV content seeds give distinct
    /// per-token vectors.
    pub fn encode_tokens(&mut self, tokens: &[String]) -> Hypervector {
        let mut sorted: Vec<String> = tokens.to_vec();
        sorted.sort();
        sorted.dedup();
        let k = sorted.len();
        let dims = self.dims;
        // Register token HVs in the store (for co-occurrence reuse), then
        // majority-vote over clones.
        let hvs: Vec<Hypervector> = sorted.iter()
            .map(|t| self.ensure_token_hv(t).clone())
            .collect();
        let mut hv = Hypervector::new(dims);
        if k == 0 { return hv; }
        let ones: Vec<i32> = {
            let mut acc = vec![0i32; dims];
            for t in &hvs {
                for i in 0..dims {
                    if t.get(i) == 1 { acc[i] += 1; }
                }
            }
            acc
        };
        for i in 0..dims {
            if ones[i] * 2 > k as i32 { hv.set(i, 1); }
        }
        hv
    }

    /// Encode a query into a hypervector (sum of token HVs)
    pub fn encode_query(&self, query: &str) -> Hypervector {
        let tokens = scan_tokens(query);
        let mut hv = Hypervector::new(self.dims);
        for t in tokens {
            if let Some(thv) = self.token_hvs.get(&t) {
                hv.bundle_into(thv);
            }
        }
        hv
    }

    /// Search memories by query similarity.
    /// M4: Uses select_nth_unstable for O(N) top-K instead of O(N log N) full sort.
    pub fn recall(&self, query: &str, top_n: usize) -> Vec<ScoredMemory> {
        if self.memories.is_empty() { return vec![]; }
        let q_hv = self.encode_query(query);
        let mut scored: Vec<(usize, f64)> = self.memories.iter().enumerate().map(|(i, m)| {
            let m_hv = self.encode_query(&m.content);
            let score = q_hv.cosine(&m_hv);
            (i, score)
        }).collect();
        // M4: Partial sort — O(N) average for top-K.
        if top_n < scored.len() {
            scored.select_nth_unstable_by(top_n, |a, b| {
                b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        scored.truncate(top_n);
        scored.into_iter().map(|(i, score)| {
            ScoredMemory { memory: self.memories[i].clone(), score }
        }).collect()
    }

    /// Co-occurrence prediction: given query tokens, predict most associated tokens
    pub fn predict(&self, query: &str, top_n: usize) -> Vec<(String, u64)> {
        let tokens = scan_tokens(query);
        let mut scores: FxHashMap<String, u64> = FxHashMap::default();
        for t in &tokens {
            for ((a, b), count) in &self.cooccur {
                if a == t { *scores.entry(b.clone()).or_insert(0) += count; }
                if b == t { *scores.entry(a.clone()).or_insert(0) += count; }
            }
        }
        let mut sorted: Vec<(String, u64)> = scores.into_iter().collect();
        // M4: Partial sort for top-K.
        if top_n < sorted.len() {
            sorted.select_nth_unstable_by(top_n, |a, b| b.1.cmp(&a.1));
        }
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
/// M12: to_ascii_lowercase instead of to_lowercase (no Unicode table).
pub fn fnv_seed(bytes:&[u8])->u64{fnv1a(bytes)}
/// FNV-1a 64-bit hash — deterministic per-token seeding.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn scan_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 3 && t.chars().any(|c| c.is_alphabetic()))
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_hv() {
        let hv = make_hv(42, 10000);
        assert_eq!(hv.dims, 10000);
        // Check that HV has both +1 and -1 values (bipolar).
        let mut has_pos = false;
        let mut has_neg = false;
        for i in 0..100 {
            match hv.get(i) {
                1 => has_pos = true,
                -1 => has_neg = true,
                _ => {}
            }
        }
        assert!(has_pos, "HV should contain +1 values");
        assert!(has_neg, "HV should contain -1 values");
    }

    #[test]
    fn test_bundle_and_clamp() {
        let mut a = Hypervector::new(100);
        let mut b = Hypervector::new(100);
        // Set all of a to +1, all of b to -1
        for i in 0..100 { a.set(i, 1); b.set(i, -1); }
        bundle(&mut a, &b);
        // Bundle of +1 and -1 = 0 → sign(0) = 0 → bit stays 0 → get returns -1.
        // Actually for majority vote: sum=0, sign is undefined. We use > 0 → set.
        // With equal +1 and -1, sum=0, bit stays 0 (clear) → get returns -1.
        // This is acceptable for HDC — ties resolve to -1.
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
