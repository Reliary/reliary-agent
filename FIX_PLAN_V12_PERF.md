# FIX PLAN V12 — Performance Overhaul

**Source:** 4 parallel audit agents found ~85 raw performance bugs. Deduplicated to 45 distinct items across 4 severity tiers. All fixes are grammar-free (no AST/parser/tree-sitter).

**Estimated total effort:** ~2 weeks (9-13 working days).
**Estimated latency win:** 5-20× on search/reindex, 2-5× on callgraph/pack.

---

## Phase 1: CRITICAL (5 items — 5-20× latency win) [2-3 days]

### P1-1: `search.rs:143-152` — Disk I/O per result file per query

**Bug:** Every search query opens and reads 4KB from disk for each candidate file to compute `is_source_like`:
```rust
// search.rs:143
let is_source = {
    let mut buf = [0u8; 4096];
    std::fs::File::open(&file_path)
        .and_then(|mut f| f.read(&mut buf))
        .map(|n| is_source_like(&buf[..n]))
        .unwrap_or(true)
};
```
**Impact:** 50-300 disk reads per search query. ~100-500ms on large corpora.

**Fix:** Compute `is_source` ONCE at ingest time, store in `file_map`:
```sql
-- schema.rs migration:
ALTER TABLE file_map ADD COLUMN is_source INTEGER NOT NULL DEFAULT 1;
```
```rust
// ingest.rs — populate during file insert:
let is_source = is_source_like(&first_4kb); // compute once
// INSERT INTO file_map (..., is_source) VALUES (..., ?)

// search.rs — replace disk read with SQL filter:
// Change the file_map query to include is_source, filter in SQL or HashMap.
```
**Files:** `schema.rs` (migration), `ingest.rs` (populate), `search.rs` (filter).

---

### P1-2: `search.rs:80-95` — Full `file_map` table scan + correlated subquery per query

**Bug:** Loads ALL files into a HashMap every search, with a correlated subquery per row:
```rust
// search.rs:80-95
let mut file_map: FxHashMap<i64, (String, f64)> = FxHashMap::default();
// ...
"SELECT fm.id, fm.file_path, COALESCE((SELECT token_len FROM file_stats WHERE file_id = fm.id), 50) as token_len FROM file_map fm"
```
**Impact:** On Linux kernel (60K files): 60K-row scan + 60K correlated subqueries per query.

**Fix:** (a) Replace correlated subquery with `LEFT JOIN`. (b) Store `total_files`/`avg_tokens` in `meta` table. (c) Only load file_map entries for matched file_ids (filter via `IN` clause from phrase results):
```rust
// search.rs — batch lookup only needed files:
// 1. Collect matched file_ids from phrase results
// 2. SELECT fm.id, fm.file_path, COALESCE(fs.token_len, 50)
//    FROM file_map fm LEFT JOIN file_stats fs ON fs.file_id = fm.id
//    WHERE fm.id IN (?, ?, ...)
// 3. Store total_files/avg_tokens in meta table, read once
```
**Files:** `search.rs`, `ingest.rs` (write meta on index build).

---

### P1-3: `type_flow.rs:672-676,1054-1265` — `compute_views` recomputes anchor FileInfo per candidate

**Bug:** Inside the per-candidate loop (line 1067), `compute_views` (line 659) calls `get_file_info(anchor_file)` (line 672) AND `get_file_info(cand_file)` (line 673) every iteration. The anchor is invariant:
```rust
// type_flow.rs:672 (inside compute_views, called per-candidate):
let anchor_info = get_file_info(anchor_file);  // SAME every iteration!
let cand_info = get_file_info(cand_file);       // already in meta_cache!
```
**Impact:** 2N mutex locks + N redundant FileInfo clones per query.

**Fix:** Hoist anchor computation before the loop at line ~1051:
```rust
// Before line 1054 loop:
let anchor_info = get_file_info(anchor_file);
let anchor_impl = enclosing_impl_at(&anchor_info, anchor_line);

// In the loop, change compute_views signature to accept precomputed values:
fn compute_views(anchor_info: &FileInfo, anchor_impl: &str, cand_info: &FileInfo, ...) -> Views { ... }

// For candidate: reuse meta_cache[oi.2] instead of get_file_info(cand_file)
let cand_info = meta_cache.get(&oi.2).expect("pre-populated");
```
**Files:** `type_flow.rs`.

---

### P1-4: `type_flow.rs:1126,1265` — `get_brace_graph` bypasses meta_cache

**Bug:** Per-candidate `get_brace_graph(&oi.2)` at lines 1126 and 1265 deep-clones the brace tree, ignoring `meta_cache` which already holds `meta.brace_graph` (used at line 1122):
```rust
// type_flow.rs:1126 (already correct — uses meta_cache):
get_brace_graph(&oi.2)  // Wait, line 1126 actually does this WRONG

// type_flow.rs:1265 (also wrong):
(anchor_brace_graph.as_ref(), get_brace_graph(&oi.2))
```
**Impact:** N full BraceNode tree clones per query (each 2-50 KB allocation).

**Fix:** Replace `get_brace_graph(&oi.2)` with `meta.map(|m| &m.brace_graph)`:
```rust
// Replace both sites:
let cand_graph = meta.as_ref().map(|m| &m.brace_graph);
```
**Files:** `type_flow.rs`.

---

### P1-5: `reindex.rs:81` — Full-table scan of `phrase_occ` per single-file reindex

**Bug:** Every file edit triggers a full table scan to find which phrases reference the file:
```rust
// reindex.rs:81
let mut stmt = match db.prepare("SELECT phrase_id, file_blob FROM phrase_occ") {
    // Iterates ALL phrase rows, unpacks each blob, filters for file_id
```
**Impact:** On kernel-scale index (4M phrase rows): 1-10 seconds per edit. Blocks watcher thread.

**Fix:** Add a reverse-index side table populated at ingest:
```sql
-- schema.rs:
CREATE TABLE IF NOT EXISTS file_phrases (
    file_id INTEGER NOT NULL,
    phrase_id INTEGER NOT NULL,
    PRIMARY KEY (file_id, phrase_id)
);
CREATE INDEX IF NOT EXISTS idx_file_phrases_file ON file_phrases(file_id);
```
```rust
// ingest.rs — populate alongside phrase_occ blob:
db.execute("INSERT OR IGNORE INTO file_phrases (file_id, phrase_id) VALUES (?, ?)", params![file_id, phrase_id])?;

// reindex.rs — replace full scan with targeted lookup:
let mut stmt = db.prepare("SELECT phrase_id FROM file_phrases WHERE file_id = ?")?;
let affected: Vec<i64> = stmt.query_map(params![file_id], |r| r.get(0))?.filter_map(|r| r.ok()).collect();
```
**Files:** `schema.rs`, `ingest.rs`, `reindex.rs`.

---

## Phase 2: HIGH (12 items — 2-5× on specific paths) [3-4 days]

### P2-1: `brace_graph.rs:205-228` — `get_brace_graph` deep-clones entire tree on every cache hit

**Bug:**
```rust
// brace_graph.rs:209
return Some(graph.clone());  // Full recursive clone of BraceNode tree
// brace_graph.rs:226
cache.insert(file_path.to_string(), graph.clone());  // Another clone on insert
```
**Impact:** Dozens of full-tree clones per query (type_flow, callgraph, func_profile). Each clone is 2-50 KB of allocations.

**Fix:** Change cache to store `Arc<BraceNode>`:
```rust
use std::sync::Arc;

static BRACE_CACHE: OnceLock<Mutex<FxHashMap<String, Arc<BraceNode>>>> = OnceLock::new();

pub fn get_brace_graph(file_path: &str) -> Option<Arc<BraceNode>> {
    let cache = BRACE_CACHE.get_or_init(|| Mutex::new(FxHashMap::default()));
    let c = cache.lock().unwrap();
    if let Some(graph) = c.get(file_path) {
        return Some(Arc::clone(graph));  // Cheap refcount bump
    }
    drop(c);
    // ... build graph ...
    let graph = Arc::new(graph);
    cache.lock().unwrap().insert(file_path.to_string(), Arc::clone(&graph));
    Some(graph)
}
```
Also update `FileMeta.brace_graph` to `Arc<BraceNode>` so clones in `func_profile.rs:61` and `callgraph_v2.rs:156,312,427,559` become refcount bumps.

**Files:** `brace_graph.rs`, `file_meta.rs`, `func_profile.rs`, `callgraph_v2.rs`, `type_flow.rs`.

---

### P2-2: `callgraph_v2.rs:363,424` — Full file reads via `std::fs::read_to_string`

**Bug:**
```rust
// callgraph_v2.rs:363 (build_call_graph):
let content = match std::fs::read_to_string(file_path) { ... };

// callgraph_v2.rs:424 (depth-2 expansion):
if let Ok(content) = std::fs::read_to_string(df) { ... };
```
**Impact:** Up to 6 full-file disk reads per callgraph query. Same file often read multiple times.

**Fix:** Route through `file_meta::get`:
```rust
let lines = crate::file_meta::get(file_path)
    .map(|m| m.lines.clone())
    .unwrap_or_else(|| {
        std::fs::read_to_string(file_path)
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default()
    });
```
**Files:** `callgraph_v2.rs`.

---

### P2-3: `callgraph_v2.rs:487,753` — `read_to_string` fallback for single line

**Bug:** When `file_meta` cache misses, reads entire file to extract one line, doesn't populate cache:
```rust
// callgraph_v2.rs:487 (build_callers fallback):
// reads whole file via read_to_string, does .lines().nth(h.line)

// callgraph_v2.rs:753 (read_line):
// same pattern — full file read for one line
```
**Impact:** N disk reads of large files for single-line extraction. Compounds across candidates.

**Fix:** Always go through `file_meta::get` (which populates the cache on miss):
```rust
fn read_line(file_path: &str, line: usize) -> Option<String> {
    crate::file_meta::get(file_path)
        .and_then(|m| m.lines.get(line).cloned())
}
```
**Files:** `callgraph_v2.rs`.

---

### P2-4: `func_profile.rs:93-105` — O(functions × all_stems) linear scan

**Bug:**
```rust
// func_profile.rs:93-105
for node in &func_nodes {
    for stem in &all_stems {  // scans ALL stems per function
        if stem.line >= node.start && stem.line <= node.end { ... }
    }
}
```
**Impact:** 100 functions × 5000 stems = 500K comparisons per file. ~100ms on large files.

**Fix:** `all_stems` is already sorted by `ORDER BY o.line`. Single-pass cursor:
```rust
let mut cursor = 0;
for node in &func_nodes {
    // Advance cursor past stems before this function
    while cursor < all_stems.len() && all_stems[cursor].line < node.start {
        cursor += 1;
    }
    // Collect stems within this function
    let mut inner = cursor;
    while inner < all_stems.len() && all_stems[inner].line <= node.end {
        profile.stems.push(all_stems[inner].phrase_id);
        inner += 1;
    }
}
// Total: O(functions + stems) — single pass.
```
**Files:** `func_profile.rs`.

---

### P2-5: `lazy_occurrence.rs:401-404` — `ensure_blocks_for_file` called inside per-line loop

**Bug:**
```rust
// lazy_occurrence.rs:392-404
for (li, line) in lines.iter().enumerate() {
    // ...
    if let Err(e) = crate::lazy_tables::ensure_blocks_for_file(db, file_id) {  // PER LINE!
        eprintln!(...);
    }
    let block_id = block_id_at_line(db, *file_id, li as i32)?;
```
**Impact:** A 2000-line file does 2000 `EXISTS(SELECT ...)` checks, each a prepared-statement round-trip. Dominant cost of file-level JIT.

**Fix:** Hoist out of the loop — call once before line 392:
```rust
// Before the loop:
if let Err(e) = crate::lazy_tables::ensure_blocks_for_file(db, file_id) {
    eprintln!("[lazy_occurrence] ensure_blocks_for_file failed: {}", e);
    return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(e)));
}

// Optionally pre-build line→block_id lookup:
let block_map: Vec<i64> = {
    let mut stmt = db.prepare("SELECT start_line, end_line, block_id FROM block WHERE file_id = ? ORDER BY start_line")?;
    let ranges: Vec<(i32, i32, i64)> = stmt.query_map(params![file_id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?.filter_map(|r| r.ok()).collect();
    // Build Vec<i64> indexed by line for O(1) lookup
    let mut map = vec![-1i64; lines.len()];
    for (start, end, bid) in ranges {
        for l in (start as usize)..=(end as usize).min(map.len().saturating_sub(1)) {
            map[l] = bid;
        }
    }
    map
};

// In the loop, replace block_id_at_line call:
let block_id = block_map.get(li).copied().unwrap_or(-1);
```
**Files:** `lazy_occurrence.rs`.

---

### P2-6: `symbol.rs:795-849` — O(P × O) N+1 in symbol_callgraph

**Bug:** For each phrase (P), queries all occurrences (O), per occurrence calls `block_bag` (GROUP BY SQL):
```rust
// symbol.rs:795-849
for (pid, ...) in &anchor_phrases {
    // SELECT ... FROM occurrence WHERE phrase_id = ?
    for occ in occs {
        // SELECT phrase_id, COUNT(*) FROM occurrence WHERE block_id = ? GROUP BY phrase_id
        let bag = block_bag(db, occ.block_id);
    }
}
```
**Impact:** 5000+ DB round-trips for a typical callgraph query.

**Fix:** Batch into one query:
```rust
// Collect all (pid, block_id) pairs needed
let block_ids: Vec<i64> = anchor_occs.iter().map(|o| o.block_id).collect();
// ONE query:
let mut stmt = db.prepare(
    "SELECT phrase_id, block_id, COUNT(*) as cnt
     FROM occurrence WHERE block_id IN (SELECT value FROM json_each(?))
     GROUP BY phrase_id, block_id"
)?;
// Pass block_ids as JSON array
```
**Files:** `symbol.rs`.

---

### P2-7: `pack/lib.rs:1228-1259` — `is_common_word` O(200) linear scan + per-call alloc

**Bug:**
```rust
// pack/lib.rs:1228-1259
fn is_common_word(word: &str) -> bool {
    const COMMON: &[&str] = &[ ... 200 entries ... ];
    let lower = word.to_ascii_lowercase();  // allocation per call
    COMMON.contains(&lower.as_str())  // O(200) linear scan
}
```
Called inside `build_cross_refs_from_index` triple-nested loop. Tens of millions of calls on large corpora.

**Fix:** Static `FxHashSet` + take `&str`:
```rust
use rustc_hash::FxHashSet;

static COMMON_SET: std::sync::OnceLock<FxHashSet<&'static str>> = std::sync::OnceLock::new();

fn is_common_word_lower(lower: &str) -> bool {
    let set = COMMON_SET.get_or_init(|| {
        COMMON.iter().copied().collect()
    });
    set.contains(lower)
}
// Callers lowercase once before passing.
```
**Files:** `pack/lib.rs`.

---

### P2-8: `pack/lib.rs:422-495` — O(selected × all_files × pairs) no inverted index

**Bug:**
```rust
// pack/lib.rs:461-468
for sym in selected {
    let usage_files: Vec<i64> = file_to_phrases
        .iter()  // ALL files
        .filter(|(_, pairs)| pairs.iter().any(|(p, is_def)| *p == sym.name && !*is_def))
        ...
```
**Impact:** 50 symbols × 700 files × 500 pairs = 17.5M comparisons.

**Fix:** Build inverted index once (same as `build_cross_refs_from_index` line 2121):
```rust
let phrase_to_non_def_files: FxHashMap<String, Vec<i64>> = {
    let mut idx: FxHashMap<String, Vec<i64>> = FxHashMap::default();
    for (file_id, pairs) in &file_to_phrases {
        for (phrase, is_def) in pairs {
            if !is_def {
                idx.entry(phrase.clone()).or_default().push(*file_id);
            }
        }
    }
    idx
};
// Then O(1) lookup:
let usage_files = phrase_to_non_def_files.get(&sym.name).cloned().unwrap_or_default();
```
**Files:** `pack/lib.rs`.

---

### P2-9: `pack/lib.rs:162` — `LIKE '%_%'` correctness AND perf bug

**Bug:**
```sql
-- pack/lib.rs:162
AND (p.phrase LIKE '%_%' OR substr(p.phrase, 2, 1) GLOB '[a-z]' ...)
```
In SQL `LIKE`, `_` is a single-char wildcard. `%_%` matches ANY string with ≥1 char. The complexity score's underscore-detection is completely broken (always true).

**Fix:**
```sql
-- Escape the underscore:
AND (p.phrase LIKE '%\_%' ESCAPE '\' OR substr(p.phrase, 2, 1) GLOB '[a-z]' ...)
```
Or compute specificity in Rust after loading phrases (avoids LIKE scan entirely).

**Files:** `pack/lib.rs`.

---

### P2-10: `pack/lib.rs:949+2211` — Duplicate full-file reads (two independent caches)

**Bug:** `extract_symbols_from_index` (line 949) and `read_symbols_and_frequency` (line 2211) each maintain independent local `HashMap<String, Vec<String>>` caches. `generate_pack` calls both sequentially on the same files — each reads from disk independently.

**Impact:** Doubles disk I/O. ~700 extra reads on reliary8.

**Fix:** Lift a shared cache to `generate_pack` call site:
```rust
fn generate_pack(...) -> String {
    let mut file_cache: FxHashMap<String, Vec<String>> = FxHashMap::default();
    // Pass &mut file_cache to both:
    let symbols = extract_symbols_from_index(&db, &mut file_cache, ...);
    let (sources, freq) = read_symbols_and_frequency(&db, &mut file_cache, ...);
}
```
**Files:** `pack/lib.rs`.

---

### P2-11: `watcher.rs:171` — `Vec::remove(0)` in hot change-buffer path

**Bug:**
```rust
// watcher.rs (inside Mutex lock):
if v.len() > 100 { v.remove(0); }  // O(n) memmove of 99 elements
```
**Fix:**
```rust
use std::collections::VecDeque;
// Change changes_for_handler to VecDeque<ChangeRecord>
if v.len() > 100 { v.pop_front(); }  // O(1)
```
**Files:** `watcher.rs`.

---

### P2-12: `type_flow.rs:1153,1156,1260` — Dead stub computation per candidate

**Bug:** `infer_receiver_type` (compat.rs:12) returns `None`, `type_jaccard` (compat.rs:17) returns `0.0`, `function_profile_wasserstein` (compat.rs:104) returns `0.0`. The entire `type_score` block (lines 1152-1158) and `ws_boost` (line 1260) compute nothing.

**Fix:** Short-circuit:
```rust
// Before the loop:
const STUBS_ENABLED: bool = false;

// In the loop:
let type_score = if STUBS_ENABLED {
    let ti_str = infer_receiver_type(&oi.2, ...);
    type_jaccard(&ti_str, &anchor_type_str)
} else { 0.0 };
```
Or simply delete the dead calls and the `ws_boost`/`type_score` terms from the formula.

**Files:** `type_flow.rs`, optionally `compat.rs`.

---

## Phase 3: MEDIUM (15 items) [3-4 days]

### P3-1: `file_meta.rs:83`, `brace_graph.rs:220`, `func_profile.rs:49` — Non-LRU arbitrary eviction

**Bug:** All three caches evict via `keys().take(50)` or `keys()[..len/2]` — HashMap iteration order is random, evicting hot entries.
```rust
// file_meta.rs:83
let keys: Vec<String> = c.keys().take(50).cloned().collect();
for k in keys { c.remove(&k); }
```
**Fix:** Use `indexmap::IndexMap` (insertion-order tracking) and evict oldest, or use the `lru` crate for true LRU.
```rust
// Option A: IndexMap (insertion order)
use indexmap::IndexMap;
if c.len() >= 100 {
    let to_remove: Vec<String> = c.keys().take(50).cloned().collect();
    for k in to_remove { c.shift_remove(&k); }
}
// Option B: lru crate
use lru::LruCache;
let cache: LruCache<String, FileMeta> = LruCache::new(NonZeroUsize::new(100).unwrap());
```
**Files:** `file_meta.rs`, `brace_graph.rs`, `func_profile.rs`.

---

### P3-2: `type_flow.rs:331-335` — `rcache()` unbounded (memory leak in long-lived MCP server)

**Bug:** `rcache()` is `OnceLock<Mutex<HashMap<...>>>` with `with_capacity(500)` but no cap. Grows forever across all queries.
```rust
// type_flow.rs:331
static rcache: OnceLock<Mutex<FxHashMap<(String, i32, String), Option<String>>>> = ...
```
**Fix:** Add 512-entry cap with half-eviction (matching brace_graph pattern):
```rust
let mut c = cache.lock().unwrap();
if c.len() >= 512 {
    let keys: Vec<_> = c.keys().take(256).cloned().collect();
    for k in keys { c.remove(&k); }
}
c.insert(key, val);
```
**Files:** `type_flow.rs`.

---

### P3-3: `type_flow.rs:559` — `LBT_CACHE` keyed `(line, var)` — collides across files (correctness bug)

**Bug:** Cache key omits `file_path` — line 42 in `foo.rs` and line 42 in `bar.rs` share a slot.
```rust
// type_flow.rs:559
static LBT_CACHE: OnceLock<Mutex<FxHashMap<(usize, String), Option<String>>>> = ...
```
**Fix:** Add `file_path` to key:
```rust
static LBT_CACHE: OnceLock<Mutex<FxHashMap<(String, usize, String), Option<String>>>> = ...
// Key: (file_path.to_string(), line as usize, var.to_string())
```
**Files:** `type_flow.rs`.

---

### P3-4: `pack/lib.rs:2143-2168` — Inner loop never breaks early

**Bug:** `refs.len() >= 5` break (line 2165) is OUTSIDE the inner pair loop. Scans every pair even after 5 refs found.
```rust
// pack/lib.rs:2143-2168
for (phrase, is_def) in pairs {  // inner loop
    if !is_def && !is_common_word(phrase) {
        refs.push(phrase.clone());
    }
}
if refs.len() >= 5 { break; }  // WRONG: only checks after inner loop completes
```
**Fix:** Move break inside inner loop:
```rust
for (phrase, is_def) in pairs {
    if !is_def && !is_common_word(phrase) {
        refs.push(phrase.clone());
        if refs.len() >= 5 { break; }
    }
}
```
**Files:** `pack/lib.rs`.

---

### P3-5: `pack/lib.rs:251,824` — Full cross-ref graph built even for L2L3 packs

**Bug:** `build_cross_refs_from_index` (600K+ rows, triple-nested loop) called unconditionally. L2L3 packs only need 6 cross-ref names.
**Fix:** Gate:
```rust
let cross_refs = if matches!(format, PackFormat::Full) {
    build_cross_refs_from_index(&db, ...)
} else {
    build_cross_refs_for_subset(&db, &selected, &symbols)  // lightweight
};
```
**Files:** `pack/lib.rs`.

---

### P3-6: `type_flow.rs:1097,1211,1327` — `format!(".{}(", raw_name)` 3× per candidate

**Fix:** Hoist before loop at line ~1019:
```rust
let dot_paren = format!(".{}(", raw_name);
// In loop:
let cand_is_method_call = cand_line_text.contains(&dot_paren);
```
**Files:** `type_flow.rs`.

---

### P3-7: `type_flow.rs:1105,1152,1349` — `Instant::now()` + `eprintln!` per candidate

**Fix:** Gate behind feature flag:
```rust
#[cfg(feature = "trace")]
let start = std::time::Instant::now();
// ...
#[cfg(feature = "trace")]
eprintln!("[type_flow] candidate took {}ms", start.elapsed().as_millis());
```
Or use `log::debug!`.
**Files:** `type_flow.rs`.

---

### P3-8: `lazy_occurrence.rs:253` — `scan_identifiers` allocates FxHashSet+Vec per line

**Bug:** `scan_identifiers` builds an `FxHashSet` for dedup + returns `Vec<String>` with `.to_ascii_lowercase()` per token per line. In the JIT hot loop this is the dominant cost.

**Fix:** Add a col-returning variant without dedup (occurrence-level data wants duplicates):
```rust
pub fn scan_identifiers_with_cols(text: &str) -> Vec<(usize, String)> {
    let mut result = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut col = 0;
    while i < bytes.len() {
        // ... same identifier scanning logic ...
        result.push((col, word.to_ascii_lowercase()));
    }
    result
}
```
Use in `lazy_occurrence.rs:253` and `lazy_occurrence.rs:409`.
**Files:** `lib.rs`, `lazy_occurrence.rs`.

---

### P3-9: `mcp.rs:189,466,490,709` — Fresh DB connection per MCP tool call

**Bug:** Every tool call does `Connection::open` + PRAGMAs + schema validation. ~1-3ms each.
**Fix:** Cache connections:
```rust
static DB_POOL: OnceLock<Mutex<FxHashMap<String, Connection>>> = OnceLock::new();

fn get_db(db_path: &str) -> MutexGuard<'static, FxHashMap<String, Connection>> {
    let pool = DB_POOL.get_or_init(|| Mutex::new(FxHashMap::default()));
    let mut guard = pool.lock().unwrap();
    if !guard.contains_key(db_path) {
        let conn = Connection::open(db_path).unwrap();
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA journal_mode=WAL;").unwrap();
        guard.insert(db_path.to_string(), conn);
    }
    guard
}
// Note: Connection is not Send+Sync without Mutex. For multi-threaded MCP,
// use r2d2 connection pool or a Mutex<Connection> per db_path.
```
**Files:** `mcp.rs`.

---

### P3-10: `pack/lib.rs:641-805` — `slice_pack_for_query` no caching

**Bug:** Re-parses entire pack string + rebuilds BM25 index per query.
**Fix:** Introduce `PackIndex` struct:
```rust
struct PackIndex {
    entries: Vec<PackEntry>,
    doc_tokens: Vec<Vec<String>>,
    df: FxHashMap<String, usize>,
    avgdl: f64,
}

impl PackIndex {
    fn build(pack: &str) -> Self { ... }  // parse + tokenize once
    fn slice(&self, query: &str, top_k: usize) -> Vec<&PackEntry> { ... }  // reuse
}
```
**Files:** `pack/lib.rs`.

---

### P3-11: `pack/lib.rs:265-266,361-362` — `.cloned()` deep-clones per symbol

**Bug:**
```rust
let refs = cross_refs.get(&sym.name).cloned().unwrap_or_default();  // Vec<String> clone
let source = symbol_sources.get(&sym.name).cloned().unwrap_or_default();  // String clone
```
**Fix:** Borrow:
```rust
let refs: &[String] = cross_refs.get(&sym.name).map(|v| v.as_slice()).unwrap_or(&[]);
let source: &str = symbol_sources.get(&sym.name).map(|s| s.as_str()).unwrap_or("");
```
Adjust `render_entry` to take `&[String]` and `&str` (it already does).
**Files:** `pack/lib.rs`.

---

### P3-12: `reindex.rs:174` — `ensure_all_for_file_with_content` runs outside transaction

**Bug:** Lazy-table rebuild runs after COMMIT, each sub-insert autocommits (fsync per insert).
**Fix:** Wrap in short transaction:
```rust
db.execute_batch("BEGIN IMMEDIATE")?;
reliary_search::lazy_tables::ensure_all_for_file_with_content(&db, file_id, content)?;
db.execute_batch("COMMIT")?;
```
**Files:** `reindex.rs`.

---

### P3-13: `callgraph_v2.rs:188-265` — `find_definition` N+1 `find_function_body` per row

**Bug:** For each callee, queries up to 20 candidate rows, calling `find_function_body` (mutex + disk/meta lookup) per row.
**Fix:** Batch: collect all (file, line) pairs, warm `file_meta` for unique files, then resolve.
**Files:** `callgraph_v2.rs`.

---

### P3-14: `symbol.rs:1070-1155` — `find_references_role` bypasses all caches

**Bug:** Uses local `HashMap` + `std::fs::read_to_string` instead of `file_meta` cache.
**Fix:** Route through `file_meta::get(path).lines`.
**Files:** `symbol.rs`.

---

### P3-15: `type_flow.rs:1011` — Anchor file read separately from file_meta cache

**Bug:** `read_lines(anchor_file)` does `read_to_string` + collect, while `file_meta::get(anchor_file)` (called moments later) reads the same file.
**Fix:** Use `file_meta::get(anchor_file).lines`.
**Files:** `type_flow.rs`.

---

## Phase 4: LOW (13 items) [1-2 days]

### P4-1: `pack/lib.rs:2356` — `derive_crate_name` per-symbol split/collect
**Fix:** Precompute crate name per file into `HashMap<String, String>` during symbol extraction.

### P4-2: `callgraph_v2.rs:135` — `extract_call_patterns` clones full line per pattern
**Fix:** Store `&str` offsets (start/end into body) or `Arc<str>`.

### P4-3: `callgraph_v2.rs:385,432` — `STOPWORDS.contains` O(180) linear scan
**Fix:** Build `FxHashSet<&'static str>` via `OnceLock`.

### P4-4: `func_profile.rs:111-133` — `collect_stems_in_function` dead code
**Fix:** Delete (replaced by batched `compute_profiles`).

### P4-5: `callgraph_v2.rs:135-138` — `buf.push_str` and `let _ = (&ident, &line_no)` dead code
**Fix:** Delete both lines.

### P4-6: `lib.rs:99-112` — `scan_identifiers` allocates FxHashSet + Vec
**Fix:** Return iterator or accept reusable `&mut FxHashSet`.

### P4-7: `search.rs:114` — `unpack_file_blob(...).collect()` just for `.len()`
**Fix:** Iterate directly, count during iteration.

### P4-8: `symbol.rs:341,389,407,537,709` — Repeated `bag_cache.insert+get` pattern
**Fix:** Use `Entry::or_insert_with`.

### P4-9: `reindex.rs:119-136` — Per-phrase INSERT+SELECT (2 round-trips)
**Fix:** Use `INSERT ... ON CONFLICT DO UPDATE SET phrase=phrase RETURNING id` (SQLite 3.35+).

### P4-10: `reindex.rs:144-148` — `Vec::with_capacity(3)` too small for varint
**Fix:** `Vec::with_capacity(9)` (max varint len + flag byte).

### P4-11: `pack/lib.rs:280-301` — `symbol_hotspot_score` O(N²) dead code
**Fix:** Delete if dead (superseded by `compute_inbound_ref_counts`).

### P4-12: `pack/lib.rs:2170-2185` — Redundant double sort+truncate
**Fix:** Delete second pass, insert directly into `cross_refs` at line 2175.

### P4-13: `watcher.rs:132` — `project_root.join(".reliary")` recomputed per event
**Fix:** Precompute once outside callback.

---

## Execution Order

1. **Phase 1 (P1-1 through P1-5)** — 2-3 days. Biggest win: 5-20× search/reindex.
2. **Phase 2 (P2-1 through P2-12)** — 3-4 days. 2-5× callgraph/pack.
3. **Phase 3 (P3-1 through P3-15)** — 3-4 days. 1.3-2× across all paths.
4. **Phase 4 (P4-1 through P4-13)** — 1-2 days. Micro wins + dead code removal.

**Dependencies:**
- P2-1 (`Arc<BraceNode>`) should be done before P1-3/P1-4 (type_flow changes) to avoid rework.
- P2-5 (lazy_occurrence hoist) should be done before P3-8 (scan_identifiers variant).
- P2-7 (is_common_word) and P2-8 (inverted index) should be done together (both in pack/lib.rs).

**Verification after each phase:**
- Run `cargo test` — all tests must pass.
- Run long bench (`reliary8_session_bench.py`) — score must not regress.
- Measure query latency before/after with `hyperfine` on representative queries.
