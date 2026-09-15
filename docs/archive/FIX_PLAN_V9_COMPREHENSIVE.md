# FIX PLAN V9 — Comprehensive (every bug, every severity)

Branch: `fix/v5-deep-audit` (extend) → `fix/v9-comprehensive`

Source: 5 parallel deep-audit agents (concurrency, SQL/DB, algorithmic, error handling, hooks/plugin).
Total: 66 distinct bugs (14 CRITICAL + 20 HIGH + 20 MEDIUM + 12 LOW).

Each item has: ID, file:line, why wrong, fix sketch, severity, est cost.

---

## PHASE 1 — CRITICAL Bugs (must fix) [14 items, ~6 days]

### C1 — `compat::extract_bindings_for_file` and `extract_methods_for_file` are stubs
**File:** `crates/reliary-search/src/compat.rs:80-97`
**Impact:** Schema defines `scope_binding` and `method_occurrence` tables + 5 indexes. Wrappers in `lazy_tables.rs` call these stubs → 0 rows. Tables permanently empty.
**Fix:** Implement both functions to walk source lines via brace_graph + structural classifier, populating tables. Or: delete tables, indexes, and all references if scope/method features won't be used.
**Est:** 1-2 days

### C2 — Empty file panic in `brace_graph.rs:141`
**File:** `crates/reliary-search/src/brace_graph.rs:141`
**Code:** `all_nodes.into_iter().next().unwrap()`
**Impact:** Empty file, single-line script, single identifier, comment-only file → panic.
**Fix:** Change signature to return `Option<BraceNode>`. Callers check `None` and fall back to a default node.
**Est:** 1 hour

### C3 — `is_definition` misclassifies strings and comments
**File:** `crates/reliary-search/src/lib.rs:143-181`
**Impact:** `let s = "fn foo("` → "foo" is a def. Comments containing `fn foo(` misclassified.
**Fix:** Add string-literal tracking. Reuse `find_byte_outside_string` pattern from structural.rs to skip chars inside `"..."`, `'...'`, `// ...`. Also skip if `//` precedes the match.
**Est:** 4 hours

### C4 — RCE via `RELIARY_BIN_PATH` env var in hooks
**File:** `hooks/claude-pretooluse.sh:61`, `hooks/opencode-reliary-sift.js:36`
**Impact:** `RELIARY_BIN_PATH='/path/reliary; rm -rf ~'` executes attacker payload when bash command rewritten.
**Fix:** Validate `RELIARY_BIN` matches `/^[A-Za-z0-9_./-]+$/`. Reject anything else with stderr warning + skip rewrite.
**Est:** 30 min

### C5 — TOCTOU + cross-session collision on gate markers
**File:** `hooks/claude-code-gate.sh:27-32`, `hooks/opencode-code-gate.js:20-21`
**Impact:** Multiple sessions with same PPID collide. PPID can be unset. Symlink attacks on /tmp.
**Fix:** Use `mkdir` (shell) or `fs.mkdtempSync` (JS) for atomic create. Use unique per-session key (PID + nanoseconds).
**Est:** 1 hour

### C6 — Nested BEGIN transaction silently fails
**File:** `lazy_occurrence.rs:399` calls `ensure_blocks_for_file` which tries `BEGIN IMMEDIATE` while outer tx open
**Impact:** Inner BEGIN returns SQLITE_ERROR. `let _ = ...` swallows it. Blocks never inserted. `block_id_at_line` returns 0.
**Fix:** Use SAVEPOINTs (`db.transaction(|tx| ...)` creates them). Or: detect active tx and skip the inner BEGIN.
**Est:** 2 hours

### C7 — `ingest.rs:327` leaks BEGIN on early return
**File:** `crates/reliary-search/src/ingest.rs:327` (BEGIN IMMEDIATE), 496 (COMMIT)
**Impact:** ~12 `?` early-return sites between BEGIN and COMMIT. Connection drop auto-rolls back, but 5GB in-memory journal sits there until close. Crash → corruption.
**Fix:** Wrap body in `db.transaction(|tx| { ... })` so rusqlite handles commit/rollback on every exit.
**Est:** 4 hours

### C8 — `INSERT OR REPLACE` wipes `file_stats` columns
**File:** `crates/reliary-search/src/ingest.rs:522-525`
**Impact:** `INSERT OR REPLACE INTO file_stats (file_id, token_len, content_len)` resets `unique_def_count`, `total_def_count`, `comment_ratio` to defaults.
**Fix:** Use `INSERT INTO file_stats (...) VALUES (...) ON CONFLICT(file_id) DO UPDATE SET token_len=excluded.token_len, content_len=excluded.content_len`.
**Est:** 30 min

### C9 — Trait method declarations NOT marked as def
**File:** `crates/reliary-search/src/structural.rs:65-66`
**Impact:** `fn bar(&self);` inside trait block ends with `;`. `is_function_signature` requires `!ends_with_semi`. Trait methods NOT marked as def. goto_def fails.
**Fix:** Track `in_trait` flag (separate depth counter for trait blocks). Allow `;` terminator when inside trait block.
**Est:** 2 hours

### C10 — BM25 uses `log(1+tf)` instead of raw `tf`
**File:** `crates/reliary-search/src/search.rs` (BM25 impl)
**Impact:** TF signal killed. 50 mentions ranks = 1 mention.
**Fix:** Use raw `tf` value (BM25 standard formula: `idf * (tf * (k1+1)) / (tf + k1 * (1 - b + b * dl/avgdl))`).
**Est:** 1 hour

### C11 — Stem collisions merge distinct symbols
**File:** `crates/reliary-search/src/lib.rs:112-126`
**Impact:** `running`/`runner` → both `runn`. Same `phrase_id` for different symbols.
**Fix:** Add a `phrase_orig TEXT` column to `phrases` (preserving original spelling). Update queries to use exact orig matching. OR: document prominently in CHANGELOG as known limitation.
**Est:** 4 hours (or 5 min if doc-only)

### C12 — All MCP errors use code `-1`
**File:** `crates/reliary-agent/src/mcp.rs` (~40 sites)
**Impact:** LLM cannot distinguish "file not found" from "DB corrupt".
**Fix:** Introduce `enum ErrKind { NotFound=-32001, InvalidPath, MissingParam, SchemaMismatch, DbError, IoError, Internal }`. Helper `fn err(kind: ErrKind, msg: String) -> DispatchResult`.
**Est:** 4 hours

### C13 — Stem/identifier collision causes wrong cross-references (test-vs-prod)
**File:** `symbol.rs:807-833` (find_references ranking) + lib.rs:112 (stem collisions)
**Impact:** Same stem → same phrase_id → all occurrences of any word with that stem appear together.
**Fix:** Filter results: only show hits where the original token (pre-stem) matches `query` exactly OR is in the same family. Add a `phrase_orig` check.
**Est:** 6 hours (depends on C11)

### C14 — `best_context_key` ignores DB count, always picks first column
**File:** `crates/reliary-search/src/pattern.rs:132-149`
**Impact:** Function returns wrong context even when DB count would indicate correct column. All 5 queries per column are identical → wasted I/O.
**Fix:** Run the COUNT query ONCE before the loop. Use the count to pick the best column heuristically (e.g., column with most distinct phrase_ids).
**Est:** 2 hours

---

## PHASE 2 — HIGH Bugs [20 items, ~10 days]

### H1 — `find_references` returns every `is_def` hit regardless of threshold
**File:** `crates/reliary-search/src/symbol.rs:807-833`
**Fix:** For `is_def=1` hits, boost similarity by `*1.5` or filter at threshold*0.5. Otherwise test code dominates.
**Est:** 2 hours

### H2 — Phase Z type-flow system is a stub
**File:** `crates/reliary-search/src/compat.rs:55-62` (function_profile_wasserstein), compat.rs:6-13 (infer_receiver_type)
**Fix:** Implement basic receiver-type inference from function signature (scan for `self: &T` / `&mut T` / `Pin<&mut T>`). Implement Wasserstein or use simpler cosine distance on profile vectors.
**Est:** 2-3 days

### H3 — Brace-graph counts `{`/`}` inside strings
**File:** `crates/reliary-search/src/brace_graph.rs:113-138`
**Fix:** Add string-literal tracking via `find_byte_outside_string` for `{` and `}` counting.
**Est:** 2 hours

### H4 — Classifier treats `if let Some(x) = ...` as definition of `Some`
**File:** `crates/reliary-search/src/structural.rs`
**Fix:** Reject definitions where the keyword before the name is `if`, `while`, `for`, `match`, `where`, `let`, `return`. Use a small keyword set check.
**Est:** 2 hours

### H5 — Classifier drops generic impl blocks
**File:** `crates/reliary-search/src/structural.rs`
**Fix:** When classifying `impl<T: Bound> Foo for Bar<T> {`, detect `impl ... for ...` pattern and set `tag=3` (method_def target). Mark Foo + Bar as defined.
**Est:** 3 hours

### H6 — Stale lazy-table content reads from disk
**File:** `crates/reliary-search/src/lazy_tables.rs:53-56, 75-79`
**Fix:** Capture file content at ingest time and store in `meta` table keyed by file_id. Lazy builders read from `meta` instead of disk.
**Est:** 4 hours

### H7 — Stale rows after file deletion
**File:** `crates/reliary-search/src/lazy_tables.rs:62`
**Fix:** When `file_map` DELETE happens, also DELETE from `block`, `occurrence`, `scope_binding`, `method_occurrence`, `phrase_occ`, `file_stats` where file_id matches. Use cascade or explicit DELETE statements.
**Est:** 2 hours

### H8 — `lines_cache.get().unwrap()` after inserting `None`
**File:** `crates/reliary-search/src/type_flow.rs:1079, 1320`
**Fix:** Use `let l = lines_cache.entry(oi.2.clone()).or_insert_with(|| read_lines(&oi.2));` and check `if l.is_none() { continue; }`.
**Est:** 30 min

### H9 — `ensure_blocks_for_file` swallowed then `?` after
**File:** `crates/reliary-search/src/lazy_occurrence.rs:399, 485`
**Fix:** Capture result of `ensure_blocks_for_file`. On error, log warning. Continue with `block_id_at_line` (will return 0/None but won't crash).
**Est:** 1 hour

### H10 — Hook `find /tmp ... -delete` on every tool call
**File:** `hooks/claude-pretooluse.sh:20`, `hooks/claude-code-gate.sh:24`
**Fix:** Remove these `find` calls. Add cleanup to `claude-session-reminder.sh` (runs once per session).
**Est:** 30 min

### H11 — Hook `RELIARY_BIN` unquoted in `bash -c` rewrite
**File:** `hooks/claude-pretooluse.sh:61`
**Fix:** Quote: `new_cmd="\"$RELIARY_BIN\" wrap bash -c '$escaped_cmd'"`. Better: use array-style JSON output if host supports.
**Est:** 15 min

### H12 — Hook newlines in commands break shell escape
**File:** `hooks/claude-pretooluse.sh:60`, `hooks/opencode-reliary-sift.js:35`
**Fix:** Reject commands containing newlines (return original command untouched for multi-line). Or: use `jq -Rs` to JSON-encode and pass as array.
**Est:** 2 hours

### H13 — Hook `which` blocks opencode startup
**File:** `hooks/opencode-reliary-sift.js:13`, `opencode-plugin/src/regen.ts:35`
**Fix:** Run `which` in async (`execFile` with `setImmediate` callback). Cache result. Add 1500ms timeout. Fall back to empty if timeout.
**Est:** 2 hours

### H14 — Hook `regen.ts` has no dispose hook
**File:** `opencode-plugin/src/regen.ts:64-76`
**Fix:** Export `dispose()` function. In opencode-plugin/index.ts, register cleanup via `process.on('exit', dispose)` + `process.on('SIGINT', dispose)`. Flush pending reindex synchronously on dispose.
**Est:** 2 hours

### H15 — Hook `index.ts` silent no-op on unrecognized arg key
**File:** `opencode-plugin/src/index.ts:28-30`
**Fix:** When `candidate` is missing for write/edit, log warning with `args` keys. Use SDK types if available.
**Est:** 1 hour

### H16 — `phrases` UNIQUE index defeated by `LIKE '%x%'`
**File:** `crates/reliary-search/src/search.rs:35`
**Fix:** Switch to trigram index on `phrase` column. `LIKE 'foo%'` (prefix-only) can use the index. For mid-word matches, trigram search.
**Est:** 4 hours

### H17 — `phrase_occ` blob grows unbounded on repeated reindex
**File:** `crates/reliary-agent/src/reindex.rs:152`
**Fix:** Replace `ON CONFLICT DO UPDATE SET file_blob = file_blob || excluded.file_blob` with stripping logic that reconstructs blob from scratch (C7 fix in V7 already does this — verify).
**Est:** 30 min (verify V7 fix is correct)

### H18 — Multi-line signatures misclassified
**File:** `crates/reliary-search/src/structural.rs:65-66`
**Fix:** Track block depth. A `fn foo(` at depth 0 inside a `impl {` is a def. At depth > 1 inside another fn body, it's a nested fn (still a def but tag=1 method_def). Update `primary_identifier_col` to find the name correctly.
**Est:** 3 hours

### H19 — `INSERT OR IGNORE` race with stem collisions
**File:** `crates/reliary-search/src/ingest.rs:408-411`
**Fix:** Use UPSERT (ON CONFLICT DO UPDATE) for `phrases` table to preserve first-seen spelling as the canonical one.
**Est:** 1 hour

### H20 — All `find /tmp` runs in hooks (cleanup of H10)
See H10.

---

## PHASE 3 — MEDIUM Bugs [20 items, ~7 days]

### M1 — `let _ = BEGIN IMMEDIATE` swallowed in lazy_*
**File:** `lazy_tables.rs:94-115`, `lazy_occurrence.rs:212-272`
**Fix:** Convert to `db.transaction(|tx| { ... })` which handles errors and COMMIT/ROLLBACK automatically. Avoid `.ok()` swallowing.
**Est:** 4 hours

### M2 — `let _ = ROLLBACK` silently drops rollback failure
**File:** `reindex.rs:156, 164`
**Fix:** Use `if let Err(e) = db.execute_batch("ROLLBACK;") { eprintln!("..."); }` at minimum. Better: use `db.transaction()` and never explicitly rollback.
**Est:** 30 min

### M3 — `let _ = db.execute("DELETE ...")` during reindex hides 99.7% bloat regression
**File:** `ingest.rs:391-394`
**Fix:** Propagate with `?` after `fresh_build` (only-fresh-build mode). Continue swallowing only on incremental update.
**Est:** 30 min

### M4 — Lazy-table re-read sees different source than ingest (same as H6)

### M5 — Stale rows after file deletion (same as H7)

### M6 — Non-UTF8 paths → lossy conversion
**File:** `ingest.rs:170, 183`
**Fix:** Use `OsString` round-trip via raw bytes (store as BLOB) or reject non-UTF8 paths with error.
**Est:** 3 hours

### M7 — Empty `name` accepted silently
**File:** `mcp.rs:130, 173, 685, 749, 776, 824, 833, 865, 898`
**Fix:** Add a helper `fn require_name(args: &Map) -> Result<String, DispatchResult>` that returns `Err(DispatchResult::Error(-32602, "missing required parameter 'name'"))` for empty names. Use everywhere.
**Est:** 2 hours

### M8 — Inconsistent "file too large" error handling
**File:** `mcp.rs:223-227, 254-258`
**Fix:** Use `reliary_core::safe_read(file, MAX)` uniformly. Returns `Result<String, String>` with descriptive error. Use in all file-reading handlers.
**Est:** 1 hour

### M9 — Hook case-sensitive tool name matching
**File:** `opencode-code-gate.js:40`
**Fix:** Match `Grep|grep|Glob|glob|Read|read` (case-insensitive). Use `tool.toLowerCase()`.
**Est:** 15 min

### M10 — Hook unverified SDK contract for `event.result`
**File:** `opencode-code-gate.js:44-45`
**Fix:** Check `@opencode-ai/plugin` typings. Use documented API (`{ block: true, message }` or whatever). Add a test.
**Est:** 1 hour

### M11 — Hook `findProjectRoot` walks per-file
**File:** `regen.ts:88`
**Fix:** Cache discovered `projectRoot` in a `Map<filePath, string>` with TTL or invalidate on file-mtime change.
**Est:** 1 hour

### M12 — Hook `pendingReindex` state shared across instances
**File:** `regen.ts:64-76`
**Fix:** Make `pendingReindex`, `pendingFiles` instance-scoped via factory function. Each plugin instance has its own queue.
**Est:** 2 hours

### M13 — Hook silent jq-missing fail-open
**File:** `claude-code-gate.sh:15`
**Fix:** Add `command -v jq` check at top. If missing, emit warning to stderr and exit 0 (gate no-ops).
**Est:** 15 min

### M14 — Hook marker files world-readable + predictable names
**File:** `claude-code-gate.sh:27`, `claude-pretooluse.sh:41`
**Fix:** `umask 077` before writing. Or: use `~/.cache/reliary/` instead of `/tmp`. Use unique suffix to prevent PID collision.
**Est:** 1 hour

### M15 — Hook `regen.ts` error-after-unref logging loss
**File:** `regen.ts:96-99`
**Fix:** Use `setImmediate` to log errors before unref. Or: keep ref for 100ms after spawn to ensure error flushes.
**Est:** 1 hour

### M16 — Hook `which` cross-platform portability
**File:** `opencode-reliary-sift.js:13`
**Fix:** Walk `$PATH` directly via Node `fs.existsSync`. Avoid `which` entirely. Fallback list `['reliary', 'reliary-agent']`.
**Est:** 1 hour

### M17 — Hook module-load throws on slow `which`
**File:** `opencode-reliary-sift.js:27`
**Fix:** Wrap entire `findReliary()` in try/catch. Return null on error. Hook no-ops gracefully.
**Est:** 15 min

### M18 — Hook tool-name brittleness across SDK versions
**File:** `index.ts:26`
**Fix:** Check `input.tool?.toLowerCase()` against `'write'` / `'edit'`. Allow `['patch', 'replace']` as aliases for future-proofing.
**Est:** 30 min

### M19 — Hook per-file spawn instead of batch
**File:** `regen.ts:73-76`
**Fix:** Modify `reliary-agent` to accept multiple files: `reliary reindex-file a.rs b.rs c.rs`. Use single spawn.
**Est:** 4 hours (touches CLI)

### M20 — Eager-index paths all silently swallowed
**File:** `mcp.rs:1308, 1327`
**Fix:** Capture result of `query_row` for COUNT. If query fails, log + treat as empty (correct existing behavior, but at least log).
**Est:** 30 min

---

## PHASE 4 — LOW Bugs [12 items, ~3 days]

### L1 — Comment detection misses `"""`, raw strings
**File:** `structural.rs:38-44`
**Fix:** Add triple-quote (`"""..."""`) and raw string (`r"..."`, `r#"..."#`) detection. Skip `//` after code (only at line start).
**Est:** 2 hours

### L2 — Trait default methods not detected (related to C9)
See C9.

### L3 — Macro-expanded fn defs mishandled
**File:** `structural.rs`
**Fix:** Detect `name!(args)` macro invocations at top level. Mark them as `tag=0` (occurrences, not defs). Add `MacroInvocation` role.
**Est:** 4 hours

### L4 — Windows paths not handled
**File:** `compat.rs:22`
**Fix:** Use `Path::components()` instead of `split('/')`. `Path` handles platform separators automatically.
**Est:** 30 min

### L5 — `tokens` array without dedup
**File:** `lib.rs:scan_identifiers` returns `Vec<String>` with duplicates
**Fix:** Use `FxHashSet<String>` internally, then convert to sorted `Vec<String>`. Avoid wasted work downstream.
**Est:** 30 min

### L6 — Single-line panic paths
**File:** various (test-only mostly)
**Fix:** None needed — tests only.
**Est:** 0

### L7 — Empty file content edge cases
**File:** `ingest.rs:192`
**Fix:** If `content.is_empty()`, return early with no phrase_locations. Don't register an empty file.
**Est:** 30 min

### L8 — Comment-only files silently registered
**File:** `ingest.rs` ingest path
**Fix:** After tokenize, if zero phrases extracted, register a `__comment_only__` placeholder or skip the file entirely.
**Est:** 1 hour

### L9 — `0-byte file path` accepted
**File:** `mcp.rs:615-617`
**Fix:** Reject empty file arg with explicit error before `safe_path`.
**Est:** 15 min

### L10 — Test-only `panic!`/`unwrap` (acceptable, no fix)

### L11 — `find` cleanup not bounded by depth
**File:** `hooks/claude-pretooluse.sh:20`
**Fix:** Replace with direct `unlink` of expected file path (no recursion). Use `rm -f /tmp/reliary-bin-path-*` (non-recursive).
**Est:** 30 min

### L12 — `eval`/`Function` constructor (none found, clean)
No fix needed.

---

## Cross-Cutting Refactors (apply during execution)

### X1 — Replace `let _ = ...` with proper error propagation in reindex.rs and lazy_*.rs
**Est:** 2 hours

### X2 — Replace `unwrap_or_default()` on DB errors with logging
**Files:** `mcp.rs:699, 703, 708, 876, 880, 911, 917, 923`
**Est:** 2 hours

### X3 — Add `serde::Serialize` on `BraceNode` so it can be returned via MCP
**Est:** 1 hour

---

## Execution Plan (4 PRs)

### PR1 (1 week) — CRITICAL only
- C1: compat stubs → implement or remove
- C2: brace_graph panic
- C3: is_definition string/comment awareness
- C4: hook RCE
- C5: hook TOCTOU
- C6: nested BEGIN
- C7: ingest.rs transaction leak
- C8: INSERT OR REPLACE
- C9: trait method defs
- C10: BM25 formula
- C11: stem collisions (doc-only for now)
- C12: error codes
- C13: stem collisions filter (depends on C11)
- C14: best_context_key

### PR2 (1.5 weeks) — HIGH (H1-H20)
Phase 2 items, prioritized by impact.

### PR3 (1 week) — MEDIUM (M1-M20)
Phase 3 items.

### PR4 (3 days) — LOW (L1-L12)
Phase 4 items + cross-cutting refactors.

**Total: ~4 weeks for all 66 bugs.**

---

## Acceptance criteria for each PR

- `cargo test --workspace`: 100% pass
- `cargo clippy --workspace -- -D warnings`: 0 warnings
- New tests added for each fixed bug (regression prevention)
- Long bench: scores maintained or improved (≥24/30 on cond=A)
- Hook tests (if any): bash -n and node --check pass
- CHANGELOG.md updated with each PR

## Risk mitigation

- **Stem collisions (C11/C13)** is invasive — could break search. Start with documentation, then evaluate impact before filtering.
- **Error codes (C12)** is invasive (40 sites). Use sed/automated refactor with safety checks.
- **BM25 formula (C10)** may change search rankings. Re-run long bench to verify no regression.
- **Trait methods (C9)** may create many false-positive defs. Add confidence threshold (e.g., only treat trait methods as defs if `is_def` heuristic score > 0.7).

## Open questions for user

1. **C1 (compat stubs)**: implement or remove? If implement, this is a real feature. If remove, several test fixtures need updating.
2. **C11 (stem collisions)**: fix the data model or just document?
3. **C13 (test-vs-prod)**: how much do you care about separating tests from production code? This is your explicit goal per the audit finding.
4. **H29 (per-file spawn → batch)**: should we modify the CLI to accept multiple files?