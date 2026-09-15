# FIX PLAN V5 — Deep Audit (5 agents: concurrency, algorithms, data integrity, edge cases, hooks)

Deduplicated against V3/V4. Items already fixed are excluded.

---

## Phase 1: CRITICAL — Concurrency & Data Integrity (7 items)

### C1: WAL helpers exist but are never used — root cause of all concurrency bugs
- **Files:** `schema.rs:47` (`open_existing_db_safe` — 0 callers), `fs_safe.rs:118` (`safe_open_db_query` — 0 callers), 12 call sites use `open_existing_db` (MEMORY+OFF) instead
- **Bug:** Every MCP query + watcher reindex downgrades the shared DB to `journal_mode=MEMORY; synchronous=OFF`. No concurrent reads during writes. Watcher writes block MCP queries for up to 5s.
- **Fix:** Replace all 12 `open_existing_db` calls with `open_existing_db_safe` (WAL + synchronous=NORMAL). Keep MEMORY+OFF only inside `create_new_db` for bulk `index` CLI.

### C2: `reindex_file` uses `apply_speed_pragma` (MEMORY+OFF) on shared DB
- **File:** `reindex.rs:10`
- **Bug:** Watcher writer forces MEMORY journal on shared index, racing MCP server readers. Combined with C1, both sides agree on MEMORY = zero concurrency.
- **Fix:** Replace `apply_speed_pragma(&d)` with `db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;")`.

### C3: `encode_varint` infinite loop on negative `i64`
- **File:** `schema.rs:284-294`
- **Bug:** `i64 >>= 7` is arithmetic shift — sign-extends for negative values, never reaches 0. Any negative input hangs the process.
- **Fix:** Cast to `u64` before shifting: `let mut n = n as u64;`.

### C4: `find_references_prototype` panics on DB error — `bag_cache.get().unwrap()`
- **File:** `symbol.rs:404,407,422,455,456`
- **Bug:** If `block_bag(db, bid)` returns Err, entry is not inserted into cache. Subsequent `.unwrap()` panics, crashing MCP server.
- **Fix:** Use `if let Some(bag) = bag_cache.get(&bid) { ... } else { continue; }`.

### C5: `reindex_file` returns `true` after COMMIT failure
- **File:** `reindex.rs:132-145`
- **Bug:** If `COMMIT` fails (disk full), changes are rolled back but function returns `true`. Watcher reports success. Lazy rebuild then writes to a DB where DELETEs were rolled back.
- **Fix:** `return false` on COMMIT failure, skip `ensure_all_for_file_with_content`.

### C6: Lazy-table rebuild runs OUTSIDE the transaction
- **File:** `reindex.rs:132-143`
- **Bug:** `BEGIN` wraps 5 DELETEs + phrase_occ updates. `COMMIT` closes it. Then `ensure_all_for_file_with_content` runs post-COMMIT. If process crashes between COMMIT and rebuild completion, occurrence/block/scope tables are deleted but never rebuilt. Irrecoverable index inconsistency.
- **Fix:** Move `ensure_all_for_file_with_content` call before COMMIT. Remove the internal `BEGIN IMMEDIATE`/`COMMIT` from `ensure_*` functions (or use savepoints for nested transactions).

### C7: Stale `phrase_occ` entries after identifier removal
- **File:** `reindex.rs:71-112`
- **Bug:** `affected_ids` only contains phrases from NEW content. If an identifier was in OLD content but absent from new, its `phrase_occ` blob retains a stale entry for this `file_id`. File-level search returns this file for identifiers that no longer appear in it.
- **Fix:** Before the strip loop, scan ALL `phrase_occ` rows, unpack blobs, and strip `file_id` from any that contain it. Or add a sidecar `phrase_file(phrase_id, file_id)` table for O(1) cleanup.

---

## Phase 2: CRITICAL — Hooks & Plugin (4 items)

### H1: `RELIARY_BIN` discovered but never used; command hardcodes `reliary`
- **Files:** `claude-pretooluse.sh:52`, `opencode-reliary-sift.js:36`
- **Bug:** Hook discovers binary path (which may be `reliary-agent`), then uses bare string `"reliary"` in the command. On systems with only `reliary-agent` installed, the wrapped command fails at runtime.
- **Fix:** Use `$RELIARY_BIN wrap ...` (shell) / `${RELIARY_BIN} wrap ...` (JS).

### H2: Compiled `dist/index.js` is stale — uses blocking `execFileSync`
- **File:** `opencode-plugin/dist/index.js:32` vs `src/regen.ts:69`
- **Bug:** Source uses non-blocking `spawn`+`unref()`, but dist still uses `execFileSync` with 10s timeout. `package.json` main points to dist. Every file edit freezes opencode for up to 40s.
- **Fix:** Rebuild dist: `cd opencode-plugin && npm run build`.

### H3: Mutex `.unwrap()` cascade-panic on poison
- **Files:** `full_file.rs:47,57`, `brace_graph.rs:201,211`, `func_profile.rs:37,48`
- **Bug:** If any thread panics while holding these locks, mutex poisons. Every subsequent `.lock().unwrap()` panics, permanently breaking the MCP server.
- **Fix:** Replace all `.lock().unwrap()` with `.lock().unwrap_or_else(|e| e.into_inner())`.

### H4: No debouncing of reindex processes; rapid edits spawn dozens of children
- **File:** `regen.ts:64-79`
- **Bug:** No debounce/batch/concurrency limit. 50-file bulk edit spawns 50 simultaneous `reliary reindex-file` processes + 50 `reliary pack` processes. System resource exhaustion.
- **Fix:** Implement a debounce queue with 500ms delay and single in-flight process.

---

## Phase 3: HIGH — Algorithmic Correctness (6 items)

### A1: BM25 multi-term queries use max-aggregation, not sum
- **File:** `search.rs:128-133`
- **Bug:** `results[idx].score = score` replaces instead of accumulating. A file matching 3 query terms gets `max(s1,s2,s3)` instead of `s1+s2+s3`.
- **Fix:** `results[idx].score += score;`.

### A2: BM25 per-file TF hardcoded to 1.0
- **File:** `search.rs:122`
- **Bug:** `let tf = 1.0;` — TF saturation is constant across all docs. A doc with the term 50 times ranks identically to one with it once.
- **Fix:** Store per-file term frequency in `phrase_occ` or occurrence table and pass real `tf`.

### A3: Type-flow scoring is stubbed — `infer_receiver_type` always returns None
- **File:** `compat.rs:6-18`, used at `type_flow.rs:1140-1145`
- **Bug:** `infer_receiver_type` → `None`, `type_jaccard` → `0.0`. The "type-flow" feature doesn't use types at all.
- **Fix:** Wire `resolve_receiver_type_with_path` (type_flow.rs:339) into `infer_receiver_type`, or document that type inference is disabled.

### A4: `build_callers` off-by-one — source text from line BEFORE the call
- **File:** `callgraph_v2.rs:461-468`
- **Bug:** `h.line` is 0-based (from DB). Code does `h.line - 1` treating it as 1-based, fetching the wrong line. Displayed `line` is `h.line + 1` (correct 1-based). Source text doesn't match line number.
- **Fix:** `content.lines().nth(h.line as usize)` (h.line is already 0-based).

### A5: `reliary-dead` is single-file scope; cross-file usage invisible
- **File:** `reliary-dead/src/lib.rs:110-118`
- **Bug:** Each file analyzed independently. A function defined in `a.rs` and called from `b.rs` is reported DEAD. Floods results with false positives for any multi-file project.
- **Fix:** Build a global identifier→usage map across all files before classifying, or deprecate in favor of `symbol::dead_symbols`.

### A6: `dead_symbols` checks capital "Test"/"Main" but phrases are always lowercase
- **File:** `symbol.rs:888-896`
- **Bug:** Phrases are stored porter-stemmed (lowercase). `name.starts_with("Test")` never matches. Go-style `TestFoo` functions are not filtered → false-positive dead reports.
- **Fix:** Use lowercase patterns: `name.starts_with("test")`, `name.ends_with("test")`. Drop the `"Main"` arm.

---

## Phase 4: HIGH — Data Integrity & Schema (5 items)

### D1: 5 queries in `main.rs` reference non-existent `phrase_occ.file_id` column
- **File:** `main.rs:1102,1124,1186,1199,1231`
- **Bug:** v4 schema packed `file_id` into blob; there's no column. All 5 queries fail silently. `diagnose_failure` and caller-count enrichment are completely non-functional.
- **Fix:** Rewrite queries to unpack `file_blob` in Rust (as `search.rs:110` already does correctly).

### D2: `reindex_file` has no schema-version check before writing
- **File:** `reindex.rs:8-17`
- **Bug:** Opens DB and writes without checking `user_version`. If watcher triggers reindex against an older schema DB, v4 INSERTs fail.
- **Fix:** Call `schema::open_existing_db(&db)` after opening, bail on version mismatch.

### D3: `phrase_occ` UPSERT errors swallowed — success reported on failure
- **File:** `reindex.rs:123-130`
- **Bug:** UPSERT error is `eprintln!`'d but not returned. Function returns `true`. Watcher reports success even when every phrase write failed.
- **Fix:** Return `false` on UPSERT error.

### D4: `BEGIN IMMEDIATE` errors swallowed → silent autocommit fallback
- **Files:** `lazy_tables.rs:94,214`, `lazy_occurrence.rs:390,478`
- **Bug:** `.ok()` swallows `SQLITE_BUSY`. Subsequent INSERTs run in autocommit mode — non-atomic, 10-100× slower.
- **Fix:** Propagate error with `?`, or use `db.transaction()` for automatic rollback.

### D5: `run_index` doesn't restore `.bak` on `create_new_db` failure
- **File:** `main.rs:391-402`
- **Bug:** Previous good index renamed to `.bak` before new schema creation. If `create_new_db` fails, code returns without restoring. User left with broken DB and stranded `.bak`.
- **Fix:** In the `is_err()` branch: remove broken partial DB, rename `.bak` back.

---

## Phase 5: HIGH — Schema/Handler Mismatches (5 items)

### S1: `reliary_find_references` threshold default mismatch (schema 0.1, handler 0.3)
- **File:** `mcp.rs:77` vs `mcp.rs:681`
- **Fix:** Change handler to `unwrap_or(0.1)`.

### S2: `reliary_pack` format default mismatch + missing "auto" enum
- **File:** `mcp.rs:92` vs `mcp.rs:465`
- **Bug:** Handler defaults to `"auto"` (not in enum). Schema says `"l2l3"`.
- **Fix:** Add `"auto"` to enum, change default to `"auto"`.

### S3: `reliary_find_references` required fields should be optional
- **File:** `mcp.rs:77`
- **Bug:** Schema says `anchor_file` and `anchor_line` are required, but handler supports auto-anchor discovery.
- **Fix:** Change `"required": ["name"]`.

### S4: Undeclared parameters in schema — `window`, `with_source`, `summary`, `context`
- **File:** `mcp.rs` — multiple tools
- **Fix:** Add these to each tool's `inputSchema.properties`.

### S5: `reliary_compress` has no input size limit
- **File:** `mcp.rs:156-167`
- **Bug:** No size guard. 10MB input → 100MB+ peak memory from regex allocations.
- **Fix:** `if text.len() > 1_000_000 { return Error(-1, "text too large"); }`.

---

## Phase 6: MEDIUM — Algorithmic & Edge Cases (8 items)

### M1: `scan_identifiers` uses Unicode `is_alphanumeric` — CJK chars fuse into identifiers
- **File:** `lib.rs:98-107`
- **Fix:** Use `c.is_ascii_alphanumeric()` in the split predicate.

### M2: BM25 `avg_tokens` can be 0.0 → division by zero → NaN → corrupt sort
- **File:** `search.rs:35`
- **Fix:** Guard: `if avg_tokens == 0.0 { 1.0 } else { avg_tokens }`.

### M3: BM25 early `break` at `top_n` skips later query terms
- **File:** `search.rs:154,157`
- **Fix:** Remove the early break; collect all, sort+truncate at end.

### M4: LIKE wildcard in `search_fts5` — `_` treated as wildcard
- **File:** `search.rs:20-24`
- **Bug:** `_` in search query matches any single char in LIKE.
- **Fix:** Escape `_` in the sanitized term, or use `INSTR()` instead of LIKE.

### M5: LIKE escape without ESCAPE clause in `trace_path.rs`
- **File:** `trace_path.rs:57-59`
- **Bug:** `%`/`_` escaped with `\` but no `ESCAPE '\'` clause. SQLite treats `\` as literal.
- **Fix:** Add `ESCAPE '\'` to the LIKE clause.

### M6: `reliary_fix` with empty `old`/`new` and no `context` → empty-string replacement
- **File:** `mcp.rs:253-258`
- **Fix:** Validate `old` is non-empty when `context` is empty.

### M7: `build_callers` misses tab whitespace and generic call syntax
- **File:** `callgraph_v2.rs:114-115`
- **Fix:** Accept `b' ' || b'\t'` for whitespace; optionally skip `::<...>` before `(`.

### M8: Non-deterministic cross-ref selection (HashSet order)
- **File:** `pack/lib.rs:470-491, 2134-2176`
- **Fix:** Collect into Vec, sort by stable key before truncating. Unify the cap (3 vs 6).

---

## Phase 7: MEDIUM — Hooks Robness (6 items)

### HK1: `jq` missing = silent failure, no warning
- **Files:** `claude-pretooluse.sh:16`, `claude-code-gate.sh:15`
- **Fix:** Check `command -v jq` at top, warn on stderr if missing.

### HK2: Symlink attack on `/tmp` cache/marker files
- **Files:** `claude-pretooluse.sh:42`, `claude-code-gate.sh:32`
- **Bug:** `>` redirection follows symlinks. Attacker pre-creates symlink at `/tmp/reliary-bin-path-<ppid>` → truncates arbitrary file.
- **Fix:** Use `mktemp -d` or `set -C` (noclobber).

### HK3: `discoverReliary` in regen.ts doesn't check `reliary-agent` fallback
- **File:** `regen.ts:35`
- **Fix:** Add `which reliary-agent` as fallback.

### HK4: `triggerReindex` doesn't check for `.reliary/index.sqlite`
- **File:** `regen.ts:64-79`
- **Fix:** Add `findProjectRoot` guard before spawning.

### HK5: `spawn` error handler swallows errors; returns `true` on failure
- **File:** `regen.ts:73-75`
- **Fix:** Log the error: `child.on('error', (err) => console.error(...))`.

### HK6: Env-var toggle comments say "default OFF" but code defaults ON (4 files)
- **Files:** `claude-pretooluse.sh:7`, `opencode-reliary-sift.js:6`, `opencode-code-gate.js:6`, `claude-session-reminder.sh:4`
- **Fix:** Fix comments to say "default ON, set to 0 to disable".

---

## Phase 8: LOW — Cleanup & Hardening (8 items)

### L1: Watcher thread detached, no join/flush on exit
- **File:** `watcher.rs:102`
- **Fix:** Store `JoinHandle` + shutdown `Sender` in `WatcherHandle`. `Drop` impl signals stop and joins.

### L2: `reindex_file` early returns leave transaction open (no ROLLBACK)
- **File:** `reindex.rs:43-67`
- **Fix:** Add explicit `ROLLBACK` on error paths, or use `db.transaction()`.

### L3: `file_meta` LRU evicts random 50 keys (not truly LRU)
- **File:** `file_meta.rs:82-85`
- **Fix:** Use `LruCache` or track access time.

### L4: `content_cache` uses `DefaultHasher` (not stable across Rust releases)
- **File:** `content_cache.rs:21-27`
- **Fix:** Use `blake3` or `sha2::Sha256` for persistence.

### L5: Bigram key `pid_a * stride` overflow on large phrase counts
- **File:** `symbol.rs:95-101`
- **Fix:** Use `i128` or `u64` pair packing.

### L6: `reliary_dead` confidence param missing enum/default in schema
- **File:** `mcp.rs:73`
- **Fix:** Add `"enum": ["high", "medium", "all"], "default": "all"`.

### L7: 0-symbol pack causes infinite regeneration
- **File:** `mcp.rs:501-504`
- **Fix:** Lower threshold or check for `"0 symbols"` content.

### L8: `relpath_with` breaks when workdir is root `/`
- **File:** `mcp.rs:22-30`
- **Fix:** Guard against empty workdir after trimming.

---

## Summary

| Phase | Items | Type |
|-------|-------|------|
| 1: Concurrency & integrity | 7 | CRITICAL |
| 2: Hooks & plugin | 4 | CRITICAL |
| 3: Algorithmic | 6 | HIGH |
| 4: Data integrity | 5 | HIGH |
| 5: Schema mismatches | 5 | HIGH |
| 6: Algorithmic & edge cases | 8 | MEDIUM |
| 7: Hooks robustness | 6 | MEDIUM |
| 8: Cleanup & hardening | 8 | LOW |
| **Total** | **49** | |

**Highest-leverage single fix:** C1 (replace `open_existing_db` with `open_existing_db_safe` at 12 call sites) — alone resolves the concurrency root cause behind C1, C2, and most of D4.

**Files modified:** `schema.rs`, `fs_safe.rs`, `reindex.rs`, `symbol.rs`, `callgraph_v2.rs`, `search.rs`, `lib.rs`, `mcp.rs`, `main.rs`, `watcher.rs`, `full_file.rs`, `brace_graph.rs`, `func_profile.rs`, `lazy_tables.rs`, `lazy_occurrence.rs`, `compat.rs`, `file_meta.rs`, `content_cache.rs`, `trace_path.rs`, `pack/lib.rs`, `reliary-dead/lib.rs`, hooks/*, regen.ts, index.ts, dist/index.js
