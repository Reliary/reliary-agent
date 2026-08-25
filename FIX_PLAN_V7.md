# FIX PLAN V7 — Deep Perf Audit (3 agents: main.rs, type_flow, schema/ingest/search)

Deduplicated against V4/V5/V6. 20 new items, 3 phases.

---

## Phase 1: CRITICAL/HIGH (3 items)

### P7-1: reindex C7 full table scan — query only affected_ids
- **File:** `reindex.rs:80-106`
- **Bug:** C7 fix from V5 scans ALL phrase_occ rows to strip one file_id. O(total_blobs) for every single-file edit. On large repos (50K files), this is catastrophic.
- **Fix:** Query only `WHERE phrase_id IN (affected_ids)`: `SELECT phrase_id, file_blob FROM phrase_occ WHERE phrase_id IN (...)`. Affected_ids is already built at line 110-127 before this scan. Move the C7 scan AFTER affected_ids is populated, or build affected_ids first, then query only those rows.

### P7-2: Dead command walks into .git/.reliary/node_modules
- **File:** `main.rs:1759`
- **Bug:** `WalkDir::new(&path_buf)` enters every directory including `.git/` (thousands of pack objects), `.reliary/` (the index itself), `node_modules/`, `target/`. All opened and 8KB-read for binary detection.
- **Fix:** Add `.filter_entry(|e| !is_hidden_dir(e))` matching pattern in `ingest.rs:148-150`.

### P7-3: Redundant phrase_id_for() DB query per type_flow candidate
- **File:** `type_flow.rs:1207`
- **Bug:** `phrase_id_for(db, raw_name)` re-runs SQLite query for every candidate. `phrase_id` already computed at line 954 and in scope.
- **Fix:** `let anchor_pid = Some(phrase_id);`

---

## Phase 2: MEDIUM (8 items)

### P7-4: Shadowed scope_bonus — resolve_local_binding result discarded
- **File:** `type_flow.rs:1169` shadowed by `type_flow.rs:1279`
- **Bug:** First scope_bonus at line 1169 calls `resolve_local_binding(db, ...)` (DB query!) then result is never read — `let scope_bonus` at line 1279 shadows it with a new binding.
- **Fix:** Rename line 1169 to `local_binding_bonus` and add to `base` at line 1306, or delete lines 1169-1179.

### P7-5: Double cand_line_text clone per type_flow candidate
- **File:** `type_flow.rs:1082` (used only for single `.contains()` at 1094, then shadowed by 1217)
- **Fix:** At line 1094, use `lines.get(idx).map_or(false, |s| s.contains(...))` instead of cloning.

### P7-6: rcache key allocates to_string() twice
- **File:** `type_flow.rs:347+359`
- **Bug:** `fp.to_string()` and `stem.to_string()` called twice — once for `get()`, once for `insert()`.
- **Fix:** Allocate key once: `let key = (fp.to_string(), line, stem.to_string());`

### P7-7: call_arity computed twice per type_flow candidate
- **File:** `type_flow.rs:1223+1325`
- **Bug:** When meta==None, computes same call_arity result twice.
- **Fix:** Store at line 1223, reuse at 1325.

### P7-8: Individual INSERTs in phrase_occ deferred flush
- **File:** `ingest.rs:472-480`
- **Bug:** Per-phrase INSERT (O(all_phrases) B-tree traversals). Should batch into multi-row INSERT with chunks of ~500.
- **Fix:** Build dynamic SQL `INSERT INTO phrase_occ VALUES (?1,?2),(?3,?4),...` in chunks.

### P7-9: Double file read in ingest (content then file_meta.get)
- **File:** `ingest.rs:179` (read content) and `ingest.rs:463` (file_meta::get re-reads)
- **Bug:** Same file read from disk twice during indexing: once for tokenization, once for file_meta cache.
- **Fix:** Pass content to file_meta via `compute_from_str(content)` or remove file_meta.get from ingest loop.

### P7-10: is_source_like to_lowercase() per sampled line
- **File:** `lazy_occurrence.rs:82` (called from `search.rs:148`)
- **Bug:** Allocates `to_lowercase()` String for every sampled line (100 per uncached file). All keywords are ASCII.
- **Fix:** Compare bytes: `trimmed.as_bytes().starts_with(b"let ")` etc.

### P7-11: Duplicated upward-walk to find .reliary/index.sqlite (5 copies)
- **Files:** `main.rs:518-547`, `main.rs:562-606`, `main.rs:1064-1082`, `watcher.rs:49-59`, `paths.rs:8-26`
- **Bug:** Five copies of identical walk-up logic, each doing `exists()` per level. No caching across calls.
- **Fix:** Extract single `find_index_path()` in `paths.rs`. Callers thread the path.

---

## Phase 3: LOW (9 items)

### P7-12: Fresh DB per who_calls
- **File:** `main.rs:593`
- **Fix:** Pool or reuse connection from `find_open_index`.

### P7-13: build_read_footer + diagnose_failure open same DB
- **File:** `main.rs:1087,1175`
- **Fix:** Open once in `exec_sift`, pass `Option<Connection>` to both.

### P7-14: Transaction leaks on early error in reindex_file
- **File:** `reindex.rs:54-72`
- **Fix:** Add ROLLBACK before every `return false` after BEGIN.

### P7-15: module_path allocates per type_flow candidate
- **File:** `compat.rs:23` called at `type_flow.rs:1109`
- **Fix:** Cache in `FxHashMap<&str, String>`.

### P7-16: Anchor file not in lines_cache
- **File:** `type_flow.rs:1013`
- **Fix:** Insert anchor file into `lines_cache` after reading.

### P7-17: Dead OCC_BATCH constant
- **File:** `ingest.rs:400`
- **Fix:** Remove.

### P7-18: is_source_like result not cached across queries
- **File:** `search.rs:143-154`
- **Fix:** Add `OnceLock<Mutex<FxHashMap<String, bool>>>` cache.

### P7-19: IN clause may exceed SQLite placeholder limit
- **File:** `search.rs:195-197`
- **Fix:** Chunk into batches of 500.

### P7-20: Dead stub calls in type_flow scoring
- **File:** `type_flow.rs:1150` (infer_receiver_type), `type_flow.rs:1199` (same_impl_target)
- **Fix:** Remove dead calls or feature-gate.
