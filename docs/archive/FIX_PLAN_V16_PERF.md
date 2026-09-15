# FIX PLAN V16 — Performance: Every Single Issue

3 audit agents, 75 distinct performance issues across 7 files.
All fixes are grammar-free (no AST, no tree-sitter, no per-language code).

## Phase 1: Quick Wins (2h)

### P1-1: Hoist `format!(".{}(", raw_name)` out of candidate loop
- **File:** `crates/reliary-search/src/type_flow.rs` lines 1115, 1243, 1366
- **What:** `format!(".{}(", raw_name)` allocated 3× per candidate in the scoring loop
- **Fix:** Compute `let call_pattern = format!(".{}(", raw_name);` once before line 1085, reuse `&call_pattern` at all 3 sites
- **Impact:** HIGH — eliminates 60 heap allocations per query
- **Effort:** 5 min

### P1-2: Cache `current_dir()` in `OnceLock`
- **File:** `crates/reliary-agent/src/mcp.rs` lines 372, 899, 919, 1104, 1159, 1263, 1341
- **What:** `std::env::current_dir()` called up to 7× per tool dispatch (syscall each time)
- **Fix:** Add `static CACHED_CWD: OnceLock<PathBuf> = OnceLock::new();` at module level. Initialize at `serve_stdio` start. Read from it in all 7 sites.
- **Impact:** HIGH — eliminates 7 syscalls per call
- **Effort:** 30 min

### P1-3: Cache env vars (`RELIARY_NO_TRUNCATE`, `RELIARY_TRUNCATE_LIMIT`) in `OnceLock`
- **File:** `crates/reliary-agent/src/mcp.rs` lines 162, 170-173, 201-203
- **What:** `std::env::var()` called per tool call in `truncate_result` (called on every `tools/call`)
- **Fix:** `static NO_TRUNCATE: OnceLock<bool> = OnceLock::new();` and `static TRUNCATE_LIMIT: OnceLock<usize> = OnceLock::new();`. Initialize lazily via `get_or_init`.
- **Impact:** MEDIUM-HIGH — eliminates env scan + String alloc per call
- **Effort:** 30 min

### P1-4: Fix `truncate_result` to check length without allocating
- **File:** `crates/reliary-agent/src/mcp.rs` line 180
- **What:** `item.get_mut("text").and_then(|t| t.as_str().map(|s| s.to_string()))` — allocates full String copy just to check length
- **Fix:** Check `text.len() > limit` first via `item.get("text").and_then(|t| t.as_str())`. Only allocate for truncation when needed:
```rust
if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
    if text.len() > limit {
        let safe_end = text.floor_char_boundary(limit);
        let truncated = format!("{}\n[... {} more chars ...]", &text[..safe_end], text.len() - safe_end);
        if let Some(obj) = item.as_object_mut() {
            obj.insert("text".to_string(), Value::String(truncated));
        }
    }
}
```
- **Impact:** MEDIUM — eliminates full-field copy on every successful tool call
- **Effort:** 30 min

### P1-5: Add `idx_block_range` composite index
- **File:** `crates/reliary-search/src/schema.rs` (schema definition)
- **What:** `block_id_at` (symbol.rs:40-41) does `SELECT block_id FROM block WHERE file_id=?1 AND start_line<=?2 AND end_line>=?2 ORDER BY (end_line-start_line) ASC LIMIT 1`. Only index is `idx_block_file ON block(file_id)` — linear scan of all blocks in the file.
- **Fix:** Add `CREATE INDEX IF NOT EXISTS idx_block_range ON block(file_id, start_line, end_line);` to schema. Rewrite query to: `WHERE file_id=?1 AND start_line<=?2 ORDER BY end_line DESC LIMIT 1` (smallest enclosing block = largest end_line among those starting before line).
- **Impact:** HIGH — O(N) → O(log N) for the hottest lookup in the codebase
- **Effort:** 30 min

### P1-6: Cache SQLite connection in MCP server
- **File:** `crates/reliary-agent/src/mcp.rs` lines 316, 581, 605, 835
- **What:** Every tool call opens a fresh `rusqlite::Connection::open()`, runs PRAGMAs, validates schema, then drops the connection
- **Fix:** Add `static CACHED_DB: OnceLock<Mutex<Connection>> = OnceLock::new();`. Open once at `initialize` (after auto-trust). Reuse for all `tools/call`. The stdio server is single-threaded so `Mutex` is sufficient (lock only held during query).
- **Note:** Need to handle the case where auto-trust creates the DB after `initialize` — the connection should be opened AFTER auto-trust completes.
- **Impact:** HIGH — eliminates 1-5ms overhead per call + schema re-validation
- **Effort:** 1h

## Phase 2: `Arc<FileMeta>` (2h)

### P2-1: Make `file_meta::get()` return `Arc<FileMeta>`
- **File:** `crates/reliary-search/src/file_meta.rs` lines 28-35, 85-88
- **What:** `get()` returns `Option<FileMeta>` by deep-cloning all 4 `Vec<String>` fields. Called by type_flow, callgraph, symbol, ingest — every query pays thousands of String clones.
- **Fix:**
  1. Change cache type from `HashMap<String, FileMeta>` to `HashMap<String, Arc<FileMeta>>`
  2. `get()` returns `Option<Arc<FileMeta>>` — cheap Arc refcount bump
  3. `compute()` returns `Arc<FileMeta>` via `Arc::new(meta)`
  4. Update all callers: type_flow.rs (line 1045, 1094), callgraph_v2.rs (line 155), symbol.rs, ingest.rs (line 523)
- **Impact:** HIGH — eliminates thousands of String clones per query
- **Effort:** 1h

### P2-2: Fix `file_meta` eviction — random → true LRU
- **File:** `crates/reliary-search/src/file_meta.rs` lines 85-88
- **What:** When cache hits 100 entries, removes `keys().take(50)` — but `HashMap` iteration order is hash-bucket order (random), NOT insertion order. Freshly-cached files can be evicted immediately.
- **Fix:** Add `lru` crate dependency. Replace `HashMap<String, Arc<FileMeta>>` with `LruCache<String, Arc<FileMeta>>` (capacity 200). Use `LruCache::get` (which touches LRU order) and `LruCache::put` (which evicts LRU entry on overflow).
- **Impact:** HIGH — makes cache actually work predictably
- **Effort:** 30 min

### P2-3: Add hit/miss counters to file_meta cache
- **File:** `crates/reliary-search/src/file_meta.rs`
- **What:** No instrumentation — hit rate is unmeasured
- **Fix:** Add `static HITS: AtomicU64` and `static MISSES: AtomicU64`. Increment in `get()`. Add `pub fn stats() -> (u64, u64)` to expose. Call from `reliary doctor` or `reliary stats`.
- **Impact:** LOW (observability)
- **Effort:** 15 min

## Phase 3: Structural Classifier Optimization (3h)

### P3-1: Single-pass delimiter scan in `classify_structural`
- **File:** `crates/reliary-search/src/structural.rs` lines 82, 139, 141, 143, 151, 154, 167, 168
- **What:** `classify_structural` calls `find_byte_outside_string` 8+ times on the same line — scanning for `(`, `{`, `:`, `<` independently. Each call is O(line_length).
- **Fix:** Add a `struct LineDelimiters { paren: Option<usize>, brace: Option<usize>, colon: Option<usize>, angle: Option<usize>, last_paren: Option<usize>, last_angle: Option<usize> }`. Implement `fn scan_delimiters(line: &str) -> LineDelimiters` that does ONE pass over the line tracking string state and recording all delimiter positions. Replace all 8+ calls with one `scan_delimiters(trimmed)` call.
- **Impact:** HIGH — hottest function in codebase, called per-line during indexing + per-candidate during query
- **Effort:** 2h

### P3-2: Fix `is_preceded_by_dot` to use known position
- **File:** `crates/reliary-search/src/structural.rs` line 200
- **What:** `before.rfind(id)` — O(n) backward scan when the identifier position is already known from `scan_last_identifier`
- **Fix:** `scan_last_identifier` should return `(Option<&str>, usize)` where `usize` is the byte offset of the identifier start. Then `is_preceded_by_dot` becomes `start > 0 && before.as_bytes()[start - 1] == b'.'` — O(1).
- **Impact:** MEDIUM
- **Effort:** 30 min

### P3-3: Integrate `strip_line_comment` into single-pass scan
- **File:** `crates/reliary-search/src/structural.rs` line 359, `crates/reliary-search/src/ingest.rs` line 304
- **What:** `strip_line_comment` does another O(n) scan per line, redundant with `classify_structural`'s scans
- **Fix:** Add `comment_end: Option<usize>` to `LineDelimiters` (from P3-1). `scan_delimiters` also finds the first `//` outside strings. `strip_line_comment` reads from the struct instead of re-scanning.
- **Impact:** MEDIUM — eliminates one redundant scan per line in ingest hot loop
- **Effort:** 30 min

### P3-4: Unify duplicated `find_top_level_eq` implementations
- **File:** `crates/reliary-search/src/structural.rs` lines 320-338, `crates/reliary-search/src/type_flow.rs` lines 229-254
- **What:** Two independent implementations of the same logic. The structural.rs version has a bug (line 328: `b == b'"' || b == b'\''` toggles `in_string` for single quotes — but single quotes are char literals/lifetimes, not string delimiters per the fix at line 389).
- **Fix:** Delete the structural.rs version. Export `has_top_level_eq` from type_flow.rs and import it in structural.rs. Fix the single-quote bug in the unified version.
- **Impact:** LOW (correctness + dedup)
- **Effort:** 15 min

## Phase 4: JIT Occurrence Build Fixes (3h)

### P4-1: Drive `build_all_occurrence` by file, not phrase
- **File:** `crates/reliary-search/src/lazy_occurrence.rs` lines 323-334
- **What:** Iterates phrases, calls `ensure_occurrence_for_phrase` per phrase. Each call re-reads ALL files containing that phrase from disk. File F is read N times (once per phrase it contains).
- **Fix:** Replace the phrase-driven loop with a file-driven loop:
```rust
// Get all distinct file_ids from phrase_occ
let file_ids: Vec<i64> = db.prepare(
    "SELECT DISTINCT file_id FROM phrase_occ"
)?.query_map([], |r| r.get(0))?.filter_map(|x| x.ok()).collect();

for fid in &file_ids {
    let _ = ensure_occurrence_for_file(db, *fid);
}
```
This reads each file exactly once and builds all its phrases in a single pass.
- **Impact:** HIGH — O(N×F) disk reads → O(F)
- **Effort:** 1h

### P4-2: Fix `ensure_occurrence_for_phrase` per-token SQL queries
- **File:** `crates/reliary-search/src/lazy_occurrence.rs` line 283
- **What:** `block_id_at_line(db, *file_id, line_no)` called per token — O(T) SQL queries per file (T = tokens). The file-driven path (`ensure_occurrence_for_file` line 414-430) already has the fix (pre-builds `block_ids` array).
- **Fix:** Mirror the file-driven path: pre-build `block_ids: Vec<i64>` via one query before the per-line loop. Index by line number.
- **Impact:** HIGH — eliminates T SQL queries per file in the phrase-driven path
- **Effort:** 30 min

### P4-3: Hoist `ensure_blocks_for_file_with_content` out of per-line loop
- **File:** `crates/reliary-search/src/lazy_occurrence.rs` line 526
- **What:** `ensure_blocks_for_file_with_content(db, file_id, content)` called inside `for (li, line) in lines.iter().enumerate()` — L redundant EXISTS queries.
- **Fix:** Move the call to BEFORE the `for` loop (mirror lines 410-413 in the non-`_with_content` variant). Also pre-build `block_ids` array before the loop.
- **Impact:** HIGH — eliminates L redundant queries per file
- **Effort:** 15 min

### P4-4: Fix `ORDER BY (SELECT COUNT(*) ...)` correlated subquery
- **File:** `crates/reliary-search/src/lazy_occurrence.rs` lines 325-327
- **What:** `ORDER BY (SELECT COUNT(*) FROM phrase_occ po WHERE po.phrase_id = p.id) ASC` — correlated subquery runs per phrases row
- **Fix:** Replace with a JOIN:
```sql
SELECT p.id FROM phrases p
LEFT JOIN (SELECT phrase_id, COUNT(*) c FROM phrase_occ GROUP BY phrase_id) x ON x.phrase_id = p.id
ORDER BY COALESCE(x.c, 0) ASC
```
- **Impact:** MEDIUM
- **Effort:** 15 min

### P4-5: Add pre-filter for `porter_stem` in `ensure_occurrence_for_phrase`
- **File:** `crates/reliary-search/src/lazy_occurrence.rs` line 272
- **What:** Every token is stemmed to check if it matches `phrase_text`. Most tokens don't match — stemming is wasted.
- **Fix:** Pre-filter: `if token.len() < phrase_text.len().saturating_sub(2) || token.len() > phrase_text.len() + 2 { continue; }` and `if token.as_bytes().first() != phrase_text.as_bytes().first() { continue; }` before calling `porter_stem`.
- **Impact:** MEDIUM — skips ~90% of tokens without stemming
- **Effort:** 15 min

### P4-6: Remove `stemmed.clone()` into phrase_cache
- **File:** `crates/reliary-search/src/lazy_occurrence.rs` lines 464, 551
- **What:** `phrase_cache.insert(stemmed.clone(), id)` — clones String for cache key when `stemmed` could be moved
- **Fix:** Restructure: `let stemmed = porter_stem(&token);` then check cache with `&stemmed`, then `phrase_cache.insert(stemmed, id)` (move). Re-derive `stemmed` for the INSERT path if needed (or restructure to avoid the second use).
- **Impact:** LOW
- **Effort:** 15 min

## Phase 5: MCP Dispatch Cleanup (2h)

### P5-1: Remove `safe_path()` with discarded result
- **File:** `crates/reliary-agent/src/mcp.rs` line 1155
- **What:** `safe_path(&af, &dir).is_ok()` calls `Path::canonicalize()` (syscall) just to get a boolean, then recomputes the path via `current_dir().join(&af)`
- **Fix:** Use `safe_path(&af, &dir)` result directly as the absolute path. Remove the `is_ok()` + `current_dir().join()` recomputation.
- **Impact:** MEDIUM — saves 2 syscalls per `_with_source` call
- **Effort:** 15 min

### P5-2: Replace `to_string_lossy().to_string()` with single allocation
- **File:** `crates/reliary-agent/src/mcp.rs` lines 400, 431, 579, 603, 641, 669, 850
- **What:** `fp.to_string_lossy().to_string()` — double allocation (Cow → String)
- **Fix:** Use `fp.to_string_lossy().into_owned()` (one allocation when Cow is Owned, zero when Borrowed). Or for guaranteed-UTF-8 paths: `fp.into_os_string().into_string().unwrap_or_default()`.
- **Impact:** MEDIUM — halves allocation at 7+ sites
- **Effort:** 30 min

### P5-3: Fix split FREEZE caches (correctness bug)
- **File:** `crates/reliary-agent/src/mcp.rs` lines 218, 236
- **What:** Two `OnceLock<Mutex<HashMap>>` with same name `FREEZE` — read cache and write cache are separate HashMaps. Freeze never works.
- **Fix:** Use a single `OnceLock<Mutex<HashMap<u64, String>>>` for both read and write.
- **Impact:** MEDIUM (if sift is ever enabled; currently dead)
- **Effort:** 10 min

### P5-4: Remove dead `sift_and_freeze` code or wire correctly
- **File:** `crates/reliary-agent/src/mcp.rs` lines 200-251
- **What:** `sift_and_freeze` defined but never called in stdio path. If someone wires it, they hit P5-3 bug.
- **Fix:** Remove the function entirely. MCP tool compression was deliberately removed (V15 decision — "sift on MCP output is wrong layer"). The bash path (`reliary wrap`) doesn't use `sift_and_freeze` — it uses `compress_unified` directly.
- **Impact:** LOW (maintainability)
- **Effort:** 10 min

### P5-5: Remove dead `reliary_find_references_boltzmann` arm
- **File:** `crates/reliary-agent/src/mcp.rs` line 563
- **What:** Match arm in `handle_symbol_tool` includes `"reliary_find_references_boltzmann"` but it's already handled at line 350. Unreachable.
- **Fix:** Remove `"reliary_find_references_boltzmann"` from the line 563 pattern list.
- **Impact:** LOW
- **Effort:** 5 min

### P5-6: Pre-allocate Vecs with capacity
- **File:** `crates/reliary-agent/src/mcp.rs` lines 471, 900, 1213, 1344
- **What:** `Vec::new()` without capacity, then `push()` causes reallocation
- **Fix:** Use `Vec::with_capacity(hits.len())` or `Vec::with_capacity(dyn_limit)` where size is known.
- **Impact:** LOW
- **Effort:** 15 min

### P5-7: Use `prepare_cached` in search.rs
- **File:** `crates/reliary-search/src/search.rs` lines 72, 118
- **What:** `db.prepare(&sql)` (not cached) for dynamically-built SQL
- **Fix:** Use `db.prepare_cached(&sql)` where the SQL is stable across calls. For the IN-list that varies, use `json_each` to pass ids as a JSON array (stable SQL text).
- **Impact:** LOW
- **Effort:** 30 min

## Phase 6: Search Path Fixes (2h)

### P6-1: Restore FTS5 index on `phrases(phrase)` for search
- **File:** `crates/reliary-search/src/search.rs` lines 35, 65-70, `crates/reliary-search/src/schema.rs`
- **What:** `LIKE '%...%'` does a full table scan of `phrases` (no index usable with leading wildcard)
- **Fix:** Add `CREATE VIRTUAL TABLE IF NOT EXISTS phrases_fts USING fts5(phrase, content='phrases', content_rowid='id');` and a trigger to keep it in sync. Change `search_fts5` to use `SELECT p.id, p.phrase FROM phrases_fts WHERE phrases_fts MATCH ?1 JOIN phrases p ON p.id = phrases_fts.rowid`. Fall back to LIKE if FTS5 is unavailable.
- **Note:** The comment in search.rs says FTS5 was "dropped for size" — but a small FTS index on 12K phrases is ~100KB. Worth it.
- **Impact:** HIGH — search latency drops from O(N) scan to O(log N)
- **Effort:** 1h

### P6-2: Fix `needed_ids.contains` O(N²) dedup
- **File:** `crates/reliary-search/src/search.rs` lines 103-104
- **What:** `if !needed_ids.contains(&fid) { needed_ids.push(fid) }` — linear scan per file_id
- **Fix:** Use `FxHashSet<i64>` for dedup, collect into Vec afterward.
- **Impact:** MEDIUM
- **Effort:** 10 min

### P6-3: Eliminate double `unpack_file_blob` call
- **File:** `crates/reliary-search/src/search.rs` lines 101-107, 135-136
- **What:** `unpack_file_blob(blob)` called twice per phrase — once to collect needed_ids, once to score
- **Fix:** Collect `Vec<(i64, u8)>` once per phrase into a local Vec, reuse for both needed_ids and scoring.
- **Impact:** LOW-MEDIUM
- **Effort:** 15 min

### P6-4: Fix `who_calls` hardcoded count=1
- **File:** `crates/reliary-search/src/search.rs` lines 213-216
- **What:** `results.push((path, 1))` — discards actual occurrence count from `unpack_count(flags)`
- **Fix:** Sum `unpack_count(flags)` per file_id and use the real count.
- **Impact:** LOW (correctness)
- **Effort:** 10 min

## Phase 7: Symbol Query Fixes (3h)

### P7-1: Use `hit.block_id` instead of re-querying `block_id_at` in `goto_def`
- **File:** `crates/reliary-search/src/symbol.rs` lines 750-763
- **What:** `block_id_at(db, hit.file_id, hit.line)` called 2×D times (once for hit, once for best) inside the def-selection loop. `OccHit` already has `block_id` field.
- **Fix:** Use `hit.block_id` directly. Cache `best.block_id` before the loop.
- **Impact:** HIGH — turns 2D SQL queries into 0
- **Effort:** 30 min

### P7-2: Fix `find_references_role` unbounded file cache (memory leak)
- **File:** `crates/reliary-search/src/symbol.rs` lines 1137-1145
- **What:** `file_cache: HashMap<String, Vec<String>>` grows without bound across a corpus. Each entry holds full file lines.
- **Fix:** Remove the local `file_cache`. Use `file_meta::get(path)` which has bounded LRU cache (after P2-2 fix). Only the single line at `hit.line` is needed for role extraction — use `meta.lines.get(hit.line)`.
- **Impact:** HIGH — fixes memory leak + eliminates redundant disk reads
- **Effort:** 30 min

### P7-3: Fix `symbol_callgraph` N+1 query pattern
- **File:** `crates/reliary-search/src/symbol.rs` lines 795-848
- **What:** Iterates `anchor_bag.keys()`, runs `SELECT ... FROM occurrence WHERE phrase_id = ?1` per phrase — P separate occurrence scans
- **Fix:** Single query `WHERE phrase_id IN (...)` for all anchor-bag phrase_ids. Group results by `block_id` in Rust, compute cosine once per block.
- **Impact:** HIGH — P scans → 1 batched query
- **Effort:** 1h

### P7-4: Cache `compute_corpus_mean` / `IdfTable::compute` across queries
- **File:** `crates/reliary-search/src/symbol.rs` lines 161-180, 277-294, 320
- **What:** `compute_corpus_mean` scans entire `occurrence` table with `GROUP BY phrase_id, tag` on every `find_references_centered` call
- **Fix:** Cache in `OnceLock<FxHashMap<i64, (f64, i64)>>`. Invalidate on reindex (add a `pub fn clear_corpus_cache()`). Store the cache in a static with a version stamp from `file_map MAX(ingested_at)`.
- **Impact:** HIGH — removes full corpus scan from every centered query
- **Effort:** 30 min

### P7-5: Hoist `MAX(id) FROM phrases` out of `block_bag_bigram`
- **File:** `crates/reliary-search/src/symbol.rs` line 95
- **What:** `SELECT COALESCE(MAX(id), 1) FROM phrases` computed per block in `find_references_bigram`
- **Fix:** Compute once at the start of `find_references_bigram`, pass as parameter to `block_bag_bigram`.
- **Impact:** MEDIUM
- **Effort:** 10 min

### P7-6: Fix `block_bag_bigram` O(T²) self-join
- **File:** `crates/reliary-search/src/symbol.rs` lines 86-92
- **What:** `JOIN occurrence a JOIN occurrence b ON ... WHERE a.block_id = ?1` — O(T²) pairs per block
- **Fix:** Fetch ordered occurrences in one `SELECT phrase_id, line, col FROM occurrence WHERE block_id=? ORDER BY line, col`. Build bigrams in a single pass in Rust (adjacent tokens on same line). O(T) instead of O(T²).
- **Impact:** HIGH for large-block corpora
- **Effort:** 1h

### P7-7: Fix `dead_symbols` per-row COUNT(DISTINCT) + SELECT phrase
- **File:** `crates/reliary-search/src/symbol.rs` lines 967-983
- **What:** Per-def-row `COUNT(DISTINCT block_id)` subquery + per-dead-row `SELECT phrase` lookup
- **Fix:** Pre-fetch `phrase_id → (COUNT(DISTINCT block_id), phrase_text)` for all `is_def=1` phrase_ids in one `GROUP BY` query:
```sql
SELECT o.phrase_id, COUNT(DISTINCT o.block_id), p.phrase
FROM occurrence o JOIN phrases p ON p.id = o.phrase_id
WHERE o.is_def = 1 GROUP BY o.phrase_id
```
- **Impact:** MEDIUM
- **Effort:** 30 min

### P7-8: Eliminate per-row `String` allocation for `file_path` in result loops
- **File:** `crates/reliary-search/src/symbol.rs` lines 521, 333, 699, 438, 811
- **What:** `let hit_file_path: String = r.get(2)?;` — fresh String per occurrence row, same path repeated hundreds of times
- **Fix:** Select `o.file_id` only. Pre-build `FxHashMap<i64, Arc<str>>` from `file_map` (one query). Resolve `file_path` from the map (Arc clone, cheap). Store `Arc<str>` in `OccHit` instead of `String`.
- **Impact:** MEDIUM — eliminates K String allocations per query
- **Effort:** 1h

## Phase 8: Type-Flow + Callgraph Fixes (2h)

### P8-1: Unify `read_lines` + `file_meta::get` caches
- **File:** `crates/reliary-search/src/type_flow.rs` lines 1029, 1094, 1045
- **What:** `read_lines(anchor_file)` and `file_meta::get(&oi.2)` read the same files into separate caches — double disk reads
- **Fix:** Replace `read_lines` calls with `file_meta::get(path).map(|m| &m.lines)` (after P2-1, this is `Arc<FileMeta>` — cheap). Remove `lines_cache` entirely, use `meta_cache` as the single source.
- **Impact:** HIGH — halves disk reads for candidate files
- **Effort:** 1h

### P8-2: Cache `cand_line_text` per candidate iteration
- **File:** `crates/reliary-search/src/type_flow.rs` lines 1103, 1234, 1352
- **What:** `get_line_text(lines, oi.3)` called 2-3× per candidate with the same args — each call does `lines[idx].clone()`
- **Fix:** Compute `let cand_line_text = get_line_text(lines, oi.3);` once at the start of each candidate iteration. Reuse the `&str` or `String` at all 3 sites.
- **Impact:** MEDIUM — eliminates 2× redundant String clones per candidate
- **Effort:** 15 min

### P8-3: Cache `extract_call_receiver` for anchor
- **File:** `crates/reliary-search/src/type_flow.rs` lines 1257, 1356
- **What:** `extract_call_receiver(&anchor_line_text, raw_name)` computed twice — once at line 1257 and again at line 1356
- **Fix:** Compute `let anchor_receiver = extract_call_receiver(&anchor_line_text, raw_name);` once before the candidate loop. Reuse at line 1356.
- **Impact:** MEDIUM
- **Effort:** 10 min

### P8-4: Batch per-candidate `COUNT(DISTINCT phrase_id)` queries
- **File:** `crates/reliary-search/src/type_flow.rs` lines 870-873
- **What:** `SELECT COUNT(DISTINCT phrase_id) FROM occurrence WHERE block_id = ?1` per candidate (up to 10)
- **Fix:** Collect all `block_id`s, issue one `SELECT block_id, COUNT(DISTINCT phrase_id) FROM occurrence WHERE block_id IN (...) GROUP BY block_id`. Build a `FxHashMap<i64, i64>` from results.
- **Impact:** MEDIUM
- **Effort:** 20 min

### P8-5: Use `file_meta::impl_targets` for import_type lookup instead of line scanning
- **File:** `crates/reliary-search/src/type_flow.rs` lines 1270-1284
- **What:** For each candidate with `self.X` receiver, walks up 50 lines to find type def, then forward 100 lines to find field — O(50×100) per candidate
- **Fix:** Use `meta_cache[file].impl_targets` (already computed by file_meta) to find the impl block. Use brace_graph to find the field within the impl block's children. O(log N) instead of O(N).
- **Impact:** MEDIUM
- **Effort:** 1h

### P8-6: Use indices instead of cloning `occs` in cap path
- **File:** `crates/reliary-search/src/type_flow.rs` line 1020
- **What:** `scored.iter().map(|(i, _)| occs[*i].clone()).collect()` — clones 20 tuples with owned Strings
- **Fix:** Store `Vec<usize>` indices: `let capped_indices: Vec<usize> = scored.iter().map(|(i, _)| *i).collect()`. Index into `occs` directly when building results.
- **Impact:** MEDIUM
- **Effort:** 15 min

### P8-7: Eliminate `line.clone()` in `extract_call_patterns`
- **File:** `crates/reliary-search/src/callgraph_v2.rs` line 135
- **What:** `let line_text = line.clone();` for every line in the function body, even lines with no call patterns
- **Fix:** Move the clone inside the `if j < bytes.len() && bytes[j] == b'('` block — only clone when a call pattern is found. Use `line.as_str()` for scanning.
- **Impact:** MEDIUM
- **Effort:** 10 min

### P8-8: Batch `find_definition()` calls for callees
- **File:** `crates/reliary-search/src/callgraph_v2.rs` line 508
- **What:** `find_definition(db, &ident)` called per unique callee in a loop — N separate DB queries + file reads
- **Fix:** Collect all unique callee names. Resolve phrase_ids in one batched query (`WHERE phrase IN (...)`). Fetch all definitions in batched queries. Build a `HashMap<String, Option<(String, i32)>>` from results.
- **Impact:** HIGH — N queries → 2 batched queries
- **Effort:** 1h

### P8-9: Fix `fs::read_to_string` fallback in `build_callers`
- **File:** `crates/reliary-search/src/callgraph_v2.rs` line 561
- **What:** If `file_meta::get` misses, reads entire file from disk just to get one line. No caching.
- **Fix:** After reading, insert into file_meta cache via `file_meta::compute(path, content)`. Or use `read_all_lines` which already caches via file_meta.
- **Impact:** MEDIUM
- **Effort:** 15 min

### P8-10: Use `FxHashSet` for STOPWORDS
- **File:** `crates/reliary-search/src/callgraph_v2.rs` line 503
- **What:** `STOPWORDS.contains(&ident.as_str())` — O(N) linear scan over ~100 entries
- **Fix:** `static STOPWORD_SET: OnceLock<FxHashSet<&str>> = OnceLock::new();` initialized from the `STOPWORDS` slice. O(1) lookup.
- **Impact:** LOW
- **Effort:** 10 min

### P8-11: Remove dead `buf` variable in `extract_call_patterns`
- **File:** `crates/reliary-search/src/callgraph_v2.rs` lines 96, 136
- **What:** `buf` written to but never read. Dead code.
- **Fix:** Delete `let mut buf = String::new();` and `buf.push_str(ident);`.
- **Impact:** LOW
- **Effort:** 5 min

### P8-12: Use references in `find_definition_via_meta` instead of cloning file paths
- **File:** `crates/reliary-search/src/callgraph_v2.rs` lines 290-313
- **What:** Allocates 3 Vecs with `(i64, String)` — cloning file path strings
- **Fix:** Use `(i64, &str)` references into the `file_ids` Vec (which outlives the function). Avoid owned String clones.
- **Impact:** LOW
- **Effort:** 15 min

## Phase 9: Ingest + Reindex Fixes (2h)

### P9-1: Eliminate full file content clone into `FileResult`
- **File:** `crates/reliary-search/src/ingest.rs` line 341
- **What:** `content: content.to_string()` clones entire file content String into `FileResult`
- **Fix:** Move `content` into the struct: `content: std::mem::take(&mut content)`. Run `is_source_like` before the move (it needs content). Restructure so content is moved last.
- **Impact:** HIGH — eliminates one full-file memcpy per indexed file
- **Effort:** 30 min

### P9-2: Add `file_meta::compute_from_content` to avoid redundant disk read
- **File:** `crates/reliary-search/src/file_meta.rs`, `crates/reliary-search/src/ingest.rs` line 523
- **What:** `file_meta::get()` during trust re-reads the file from disk (the content was already read at ingest.rs:226)
- **Fix:** Add `pub fn compute_from_content(path: &str, content: &str) -> Arc<FileMeta>` that skips the `fs::read_to_string` and uses the provided content directly. Call it from ingest.rs:523 with `res.content` before it's moved.
- **Impact:** HIGH — halves disk I/O during indexing
- **Effort:** 30 min

### P9-3: Use `INSERT ... ON CONFLICT DO NOTHING RETURNING id` for file_map
- **File:** `crates/reliary-search/src/ingest.rs` lines 430-433
- **What:** `INSERT OR IGNORE INTO file_map` + `SELECT id FROM file_map WHERE file_path = ?1` — 2 round-trips per file
- **Fix:** Use `INSERT INTO file_map (file_path) VALUES (?1) ON CONFLICT(file_path) DO NOTHING RETURNING id` (SQLite 3.35+). One statement returns the id.
- **Impact:** MEDIUM
- **Effort:** 15 min

### P9-4: Use `&str` comparison for `defined_names` in extraction loop
- **File:** `crates/reliary-search/src/ingest.rs` line 300
- **What:** `let line_def_name = defined_names.get(li).and_then(|n| n.clone());` — clones `Option<String>` per line
- **Fix:** Use `defined_names.get(li).and_then(|n| n.as_deref())` — returns `Option<&str>`, no allocation. Change comparison sites (lines 319, 321) to use `&str`.
- **Impact:** MEDIUM — eliminates one String alloc per def line
- **Effort:** 15 min

### P9-5: Add `porter_stem_lower` variant to skip redundant lowercase
- **File:** `crates/reliary-search/src/lib.rs` line 116
- **What:** `scan_identifiers` already lowercases tokens, then `porter_stem` lowercases again
- **Fix:** Add `pub fn porter_stem_lower(word: &str) -> String` that assumes already-lowercased input (skips the `to_ascii_lowercase()` at line 116). Call from `scan_identifiers` pipeline. Keep `porter_stem` for external callers that may pass mixed-case.
- **Impact:** MEDIUM — one redundant alloc + scan per token
- **Effort:** 15 min

### P9-6: Add `reindex_batch` for multi-file reindex
- **File:** `crates/reliary-agent/src/reindex.rs`
- **What:** `reindex_single_file` opens a new DB connection + transaction per file. On batch operations (git checkout), this is N separate connection opens + N fsyncs.
- **Fix:** Add `pub fn reindex_batch(db_path: &str, files: &[(String, String)]) -> Result<usize, String>` that opens one connection, wraps all files in a single transaction, commits once.
- **Impact:** MEDIUM — for batch reindex scenarios
- **Effort:** 30 min

### P9-7: Use `INSERT ... ON CONFLICT DO NOTHING RETURNING id` for phrases in reindex
- **File:** `crates/reliary-agent/src/reindex.rs` lines 120-134
- **What:** `INSERT OR IGNORE INTO phrases` + `SELECT id FROM phrases WHERE phrase = ?1` — 2 round-trips per phrase
- **Fix:** Same as P9-3: use `RETURNING id` to combine INSERT + SELECT.
- **Impact:** MEDIUM
- **Effort:** 15 min

### P9-8: Eliminate dummy String allocation for table files
- **File:** `crates/reliary-search/src/ingest.rs` line 282
- **What:** `content: String::new()` allocated for table-file path even when unused
- **Fix:** Use `String::new()` (which doesn't allocate until pushed to) — it's already zero-allocation. The issue is the `__table_file__` String key. Use a `&'static str` constant instead.
- **Impact:** LOW
- **Effort:** 5 min

### P9-9: Eliminate `phrase_id_cache.insert(phrase.clone(), id)` per unique phrase
- **File:** `crates/reliary-search/src/ingest.rs` line 475
- **What:** Clones stemmed phrase String for cache key
- **Fix:** Use `FxHashMap<&str, i64>` with borrows from `res.phrase_locations` (which outlives the loop).
- **Impact:** LOW
- **Effort:** 15 min

### P9-10: Cache `module_path()` per unique file
- **File:** `crates/reliary-search/src/type_flow.rs` line 1130
- **What:** `module_path(&oi.2)` called per candidate — String allocation per call
- **Fix:** Pre-compute `module_path` per unique file (like `meta_cache`). Store in the cache alongside FileMeta.
- **Impact:** LOW
- **Effort:** 10 min

### P9-11: Cache `context_key_at` per unique file
- **File:** `crates/reliary-search/src/type_flow.rs` line 1129
- **What:** `context_key_at(&oi.2, oi.3, raw_name, oi.4 as usize)` — per-candidate string work
- **Fix:** Batch or cache by `(file_path, line)` if the function is expensive. Profile first to confirm it's worth caching.
- **Impact:** LOW
- **Effort:** 15 min

### P9-12: Eliminate `fallback path clones all occs`
- **File:** `crates/reliary-search/src/type_flow.rs` lines 1378-1386
- **What:** If `hits` is empty after scoring, pushes ALL occurrences into hits with similarity 0.001, cloning `oi.2` per row
- **Fix:** This path is rare (only when no hits meet threshold). Acceptable. If optimization needed, use `oi.2.clone()` is unavoidable for `OccHit` ownership. Could use `Arc<str>` for `file_path` in OccHit (requires P7-8).
- **Impact:** LOW
- **Effort:** N/A (defer to P7-8)

## Phase 10: Schema + Pack Fixes (1h)

### P10-1: Add `idx_block_range` index (duplicate of P1-5, listed here for completeness)
- **File:** `crates/reliary-search/src/schema.rs`
- **What:** Missing covering index on `block(file_id, start_line, end_line)`
- **Fix:** See P1-5
- **Effort:** already counted

### P10-2: Single-pass pack file parsing for `reliary_pack_query`
- **File:** `crates/reliary-agent/src/mcp.rs` lines 707-741
- **What:** Two O(N) passes over pack file — first pass collects caller names into HashSet, second pass collects entries
- **Fix:** Single pass: build `HashMap<String, Vec<String>>` of all entries keyed by name in one pass. Look up target + callers from the map.
- **Impact:** MEDIUM
- **Effort:** 30 min

### P10-3: Compute file count once in `find_references_with_source`
- **File:** `crates/reliary-agent/src/mcp.rs` lines 1230, 1325
- **What:** File count computed twice (once for summary mode, once for grep mode) — duplicate HashSet
- **Fix:** Compute `n_files` once before the `if is_summary_mode` / `if format == "grep"` branch.
- **Impact:** LOW-MEDIUM
- **Effort:** 10 min

### P10-4: Use `HashSet<&str>` for `callers_to_include` in pack_query
- **File:** `crates/reliary-agent/src/mcp.rs` lines 706, 713-718
- **What:** Builds `HashSet<String>` by splitting heading lines and calling `t.to_string()` per token
- **Fix:** Use `HashSet<&str>` with borrowed slices from `pack_content` (which lives for the function scope).
- **Impact:** LOW
- **Effort:** 10 min

## Summary

| Phase | Items | Effort | Impact |
|-------|-------|--------|--------|
| Phase 1: Quick Wins | 6 | 2h | HIGH |
| Phase 2: Arc<FileMeta> | 3 | 2h | HIGH |
| Phase 3: Structural | 4 | 3h | HIGH |
| Phase 4: JIT Build | 6 | 3h | HIGH |
| Phase 5: MCP Dispatch | 7 | 2h | MEDIUM |
| Phase 6: Search | 4 | 2h | HIGH |
| Phase 7: Symbol Queries | 8 | 3h | HIGH |
| Phase 8: Type-Flow + Callgraph | 12 | 2h | MEDIUM |
| Phase 9: Ingest + Reindex | 12 | 2h | MEDIUM |
| Phase 10: Schema + Pack | 4 | 1h | LOW-MEDIUM |
| **Total** | **66** | **~22h** | |

### Dependency ordering

1. **P2-1 (Arc<FileMeta>)** must come before P7-2, P8-1, P8-7, P8-9, P9-2 — they all depend on `Arc<FileMeta>` return type
2. **P2-2 (true LRU)** should come before P7-2 — it removes the local file_cache
3. **P1-5 (idx_block_range)** must come before P4-2, P7-1 — they assume the index exists
4. **P1-6 (cached connection)** must come before P5-1 — safe_path uses the connection indirectly
5. **P3-1 (single-pass scan)** should come before P3-3 — comment stripping integrates into the scan

### Verification plan

After each phase:
1. `cargo test --lib` — all tests pass
2. `cargo build --release` — clean compile
3. Run `reliary trust .` on reliary8 itself — verify indexing time
4. Run long bench (2 seeds, A + C conditions) — verify no regression
5. Run determinism tests — verify cache safety

After all phases:
1. Full 4-seed bench (A, C conditions)
2. Compare WC, score, wall time vs V15 baseline
3. Verify: WC should decrease 10-20%, wall time should decrease 20-40%