# FIX PLAN V9 — Deep Dive (5 parallel audits)

Branch: `fix/v5-deep-audit` (extend) → `fix/v9-deep-audit`

All 5 deep-audit agents completed. Findings deduplicated and ranked by impact.

---

## CRITICAL Bugs (Must Fix)

### C1 — `compat::extract_bindings_for_file` and `extract_methods_for_file` are stubs returning 0
**File:** `crates/reliary-search/src/compat.rs:80-97`
**Impact:** The schema (schema.rs:133-157) defines `scope_binding` and `method_occurrence` tables, plus 5 indexes. The lazy_tables wrappers call these stubs and get 0 rows back. **Both tables are permanently empty.** Every query against them returns zero rows.
**Fix:** Either implement these functions or delete the tables/indexes entirely.

### C2 — Empty file panic in `brace_graph.rs:141`
**File:** `crates/reliary-search/src/brace_graph.rs:141`
**Code:** `all_nodes.into_iter().next().unwrap()`
**Impact:** Empty file, single-line script, single identifier, or comment-only file → panic on first MCP query. Crashes the entire MCP server.
**Fix:** Return `Option<BraceNode>` and have callers handle `None`.

### C3 — `is_definition` misclassifies strings and comments as definitions
**File:** `crates/reliary-search/src/lib.rs:143-181`
**Impact:** `let s = "fn foo(";` → `is_definition("foo", line, idx)` returns true. Comments containing `fn foo(` also misclassified. **This poisons goto-def accuracy across the codebase.**
**Fix:** Add string-literal and comment awareness before `(`, `<`, `[`, `=`, `:`, `{`.

### C4 — RCE via `RELIARY_BIN_PATH` env var in hooks
**File:** `hooks/claude-pretooluse.sh:61`, `hooks/opencode-reliary-sift.js:36`
**Impact:** `RELIARY_BIN_PATH='/path/reliary; rm -rf ~'` would execute the attacker's payload when the hook rewrites a bash command.
**Fix:** Validate `RELIARY_BIN` matches `/^[A-Za-z0-9_./-]+$/` before using.

### C5 — TOCTOU + cross-session collision on gate marker files
**File:** `hooks/claude-code-gate.sh:27-32`, `hooks/opencode-code-gate.js:20-21`
**Impact:** Multiple Claude Code sessions with the same `PPID` collide. PPID can be unset in some hook runner contexts. Symlink attacks on `/tmp` possible.
**Fix:** Use `mkdir` with `O_EXCL` for atomic create. Use unique per-session key (`$$` + nanoseconds).

### C6 — Nested BEGIN transaction silently fails
**File:** `lazy_occurrence.rs:399` calls `ensure_blocks_for_file` which tries `BEGIN IMMEDIATE` while the outer tx is open
**Impact:** The inner `BEGIN` returns SQLITE_ERROR ("cannot start a transaction within a transaction"). `let _ = ...` swallows it. Blocks never inserted. `block_id_at_line` returns 0 for every occurrence in that file.
**Fix:** Either use SAVEPOINTs or detect and skip the inner BEGIN when already in a tx.

### C7 — Begin..begin chain in ingest.rs leaks transaction on early return
**File:** `crates/reliary-search/src/ingest.rs:327` (BEGIN IMMEDIATE), 496 (COMMIT)
**Impact:** Between BEGIN and COMMIT there are ~12 `?` early-return sites. If any fires, the function returns with transaction open. Connection drop auto-rolls back, but the 5GB in-memory journal (synchronous=OFF) sits there until close. Crash → corruption.
**Fix:** Wrap body in `db.transaction(|tx| { ... })` so rusqlite handles commit/rollback on every exit path.

### C8 — INSERT OR REPLACE wipes file_stats computed columns
**File:** `crates/reliary-search/src/ingest.rs:522-525`
**Impact:** `INSERT OR REPLACE INTO file_stats (file_id, token_len, content_len) VALUES ...` resets `unique_def_count`, `total_def_count`, `comment_ratio` to defaults. Any tool that computed and stored those values has data silently wiped on every reindex.
**Fix:** Use `ON CONFLICT(file_id) DO UPDATE SET token_len=excluded.token_len, content_len=excluded.content_len`.

### C9 — trait method declarations NOT marked as def
**File:** `crates/reliary-search/src/structural.rs:65-66`
**Impact:** `fn bar(&self);` inside a `trait Foo { }` block ends with `;`. `is_function_signature` requires `!ends_with_semi`. Trait methods are NOT marked as definitions. goto_def fails for trait methods.
**Fix:** Allow `;` as terminator when inside a trait block.

### C10 — BM25 uses `log(1+tf)` instead of raw `tf`
**File:** `crates/reliary-search/src/search.rs` (BM25 implementation)
**Impact:** TF signal completely killed. A file with 50 mentions ranks identically to one with 1 mention.
**Fix:** Use raw `tf` value (not `log(1+tf)`).

### C11 — Stem collisions merge distinct symbols
**File:** `crates/reliary-search/src/lib.rs:112-126`
**Impact:** `running`, `runner` → both → `runn`. `color`, `colors`, `colorful` → `color`. Same `phrase_id` for different symbols. find_references returns hits for wrong words.
**Fix:** Document prominently OR add a `phrase_orig` table preserving original spellings.

### C12 — All MCP errors use code `-1`
**File:** `crates/reliary-agent/src/mcp.rs` (~40 sites)
**Impact:** The LLM cannot programmatically distinguish "file not found" from "DB corrupt" from "permission denied". Treats them all as fatal.
**Fix:** Use JSON-RPC standard codes: `-32602` invalid params, `-32001..-32006` semantic errors (NotFound, InvalidPath, MissingParam, SchemaMismatch, DbError, IoError).

### C13 — Stem/identifier collision causes wrong cross-references (test-vs-prod)
**File:** `find_references` ranking + porter_stem collisions
**Impact:** Same stem → same `phrase_id` → all occurrences of any word with that stem appear together. Cannot distinguish e.g. `consume` (BufWriter trait) from `consume` (other unrelated fns).

### C14 — `best_context_key` ignores DB count, always picks first column
**File:** `crates/reliary-search/src/pattern.rs:132-149`
**Impact:** Function returns wrong context even when DB count would indicate correct column.

---

## HIGH Bugs

### H1 — `find_references` returns every `is_def` hit regardless of threshold
**File:** `crates/reliary-search/src/symbol.rs:807-833`
**Impact:** Threshold ignored for defs; test code (low signal) ranks equal to production code (high signal). Test-vs-prod problem the user explicitly asked about.
**Fix:** Boost or filter defs by signal strength.

### H2 — Phase Z type-flow system is a stub
**File:** `crates/reliary-search/src/compat.rs:55-62` (function_profile_wasserstein), compat.rs:6-13 (infer_receiver_type)
**Impact:** `ws_boost` always 0. `ti_str` always empty. Type-flow scoring is just view-based scoring.

### H3 — Brace-graph counts `{`/`}` inside strings
**File:** `crates/reliary-search/src/brace_graph.rs:113-138`
**Impact:** `let s = "{ not a brace }"` inside braces corrupts depth tracking.

### H4 — Classifier treats `if let Some(x) = ...` as definition of `Some`
**File:** `crates/reliary-search/src/structural.rs`
**Impact:** Control flow misclassified as definitions.

### H5 — Classifier drops generic impl blocks entirely
**File:** `crates/reliary-search/src/structural.rs`
**Impact:** `impl<T: Bound> Foo for Bar<T> { ... }` not classified correctly.

### H6 — Stale lazy-table content reads from disk
**File:** `crates/reliary-search/src/lazy_tables.rs:53-56, 75-79`
**Impact:** Lazy rebuild reads CURRENT disk content, not the snapshot used at ingest. Determinism broken.

### H7 — Stale rows after file deletion
**File:** `crates/reliary-search/src/lazy_tables.rs:62`
**Impact:** Orphan rows in `block`, `occurrence`, etc. after file is removed from index. No cascade delete.

### H8 — `lines_cache.get().unwrap()` after inserting `None`
**File:** `crates/reliary-search/src/type_flow.rs:1079, 1320`
**Impact:** Panics on cache miss when file is unreadable.

### H9 — `ensure_blocks_for_file` swallowed then `?` after
**File:** `crates/reliary-search/src/lazy_occurrence.rs:399, 485`
**Impact:** Build fails silently. All occurrences get block_id=0.

### H10 — Hook `find /tmp ... -delete` on every tool call
**File:** `hooks/claude-pretooluse.sh:20`, `hooks/claude-code-gate.sh:24`
**Impact:** 5-10ms × every LLM tool call. Noticeable UI freeze on slow filesystems.

### H11 — Hook `RELIARY_BIN` unquoted in `bash -c` rewrite
**File:** `hooks/claude-pretooluse.sh:61`
**Impact:** Breaks installs in non-standard paths. Word-splits binary path.

### H12 — Hook newlines in commands break shell escape
**File:** `hooks/claude-pretooluse.sh:60`, `hooks/opencode-reliary-sift.js:35`
**Impact:** Any multi-line bash command breaks the rewrite.

### H13 — Hook `claude-code-gate.sh` `which` blocks opencode startup
**File:** `hooks/opencode-reliary-sift.js:13`
**Impact:** If `which` hangs (NFS), opencode fails to boot.

### H14 — Hook `regen.ts` has no dispose hook
**File:** `opencode-plugin/src/regen.ts:64-76`
**Impact:** On hot-reload, new instance shares state. On session end, pending timer fires against stale binary path.

### H15 — Hook `index.ts` silent no-op on unrecognized arg key
**File:** `opencode-plugin/src/index.ts:28-30`
**Impact:** If opencode passes `{filepath: "..."}` instead of `{file: "..."}`, plugin no-ops silently. Index goes stale.

### H16 — `phrases` UNIQUE index defeated by `LIKE '%x%'` search
**File:** `crates/reliary-search/src/search.rs:35`
**Impact:** Every search becomes full table scan of phrases.

### H17 — `INSERT OR REPLACE INTO file_stats` resets columns
(See C8 — same root cause)

### H18 — `phrase_occ` blob grows unbounded on repeated reindex
**File:** `crates/reliary-agent/src/reindex.rs:152` (ON CONFLICT DO UPDATE appends)
**Impact:** Relies on C7 strip; if that fails, blob grows 100x.

### H19 — `multi-line signatures` misclassified
**File:** `crates/reliary-search/src/structural.rs:65-66`
**Impact:** `fn foo(\n    arg: T,\n)` — first line gets `defined_name=foo` (OK) but `primary_identifier_col` returns col of `fn` not `foo`.

### H20 — `INSERT OR IGNORE INTO phrases` race with stem collisions
**File:** `crates/reliary-search/src/ingest.rs:408-411`
**Impact:** Multiple distinct words with same stem overwrite each other's phrase rows.

---

## MEDIUM Bugs

### M1 — `LET _ = db.execute_batch("BEGIN IMMEDIATE")?;` swallowed in lazy_*
**File:** `lazy_tables.rs:94-115`, `lazy_occurrence.rs:212-272`
**Impact:** Inner BEGIN fails silently when called from within an outer transaction. Same root cause as C6.

### M2 — `LET _ = ... ROLLBACK` silently drops rollback failure
**File:** `reindex.rs:156, 164`
**Impact:** Half-applied state committed on busy/locked DB.

### M3 — `LET _ = db.execute("DELETE ...")` during reindex hides 99.7% bloat bug regression
**File:** `ingest.rs:391-394`
**Impact:** Original Arc 31 bug returns if DELETE fails.

### M4 — `phrases` UNIQUE defeats LIKE search (see H16)

### M5 — Stale rows after file deletion (see H7)

### M6 — Lazy re-read sees different source than ingest (see H6)

### M7 — Non-UTF8 paths → lossy conversion → DB path can't reopen file
**File:** `crates/reliary-search/src/ingest.rs:170, 183`
**Impact:** U+FFFD replacement chars in stored path.

### M8 — Empty `name` accepted silently in all tools
**File:** `mcp.rs:130, 173, 685, 749, 776, 824, 833, 865, 898`
**Impact:** LLM gets 0 results for empty-name queries. Should return `-32602`.

### M9 — Inconsistent "file too large" error handling
**File:** `mcp.rs:223-227, 254-258`
**Impact:** Different error path for permission-denied vs >10MB.

### M10 — Hook case-sensitive tool name matching
**File:** `opencode-code-gate.js:40`
**Impact:** Mixed-case tool names miss gate.

### M11 — Hook unverified SDK contract for `event.result`
**File:** `opencode-code-gate.js:44-45`
**Impact:** May silently do nothing if SDK contract differs.

### M12 — Hook `findProjectRoot` walks per-file
**File:** `regen.ts:88`
**Impact:** 20+ stat calls per file in batch.

### M13 — Hook `pendingReindex` state shared across instances
**File:** `regen.ts:64-76`
**Impact:** Cross-instance state leak.

### M14 — Hook silent jq-missing fail-open
**File:** `claude-code-gate.sh:15`
**Impact:** Gate doesn't gate if jq absent.

### M15 — Hook marker files world-readable + predictable names
**File:** `claude-code-gate.sh:27`, `claude-pretooluse.sh:41`

### M16 — Hook `regen.ts` error-after-unref logging loss
**File:** `regen.ts:96-99`

### M17 — Hook `which` cross-platform portability
**File:** `opencode-reliary-sift.js:13`

### M18 — Hook module-load throws on slow `which`
**File:** `opencode-reliary-sift.js:27`

### M19 — Hook tool-name brittleness across SDK versions
**File:** `index.ts:26`

### M20 — Hook per-file spawn instead of batch
**File:** `regen.ts:73-76`

---

## LOW Bugs

### L1 — Comment detection misses `"""`, raw strings
**File:** `structural.rs:38-44`

### L2 — Trait default methods not detected (related to C9)

### L3 — Macro-expanded fn defs mishandled

### L4 — Windows paths not handled (compat.rs:22)

### L5 — `tokens` array without dedup

### L6 — Single-line panic paths

### L7 — Empty file content edge cases

### L8 — Comment-only files silently registered

### L9 — `0-byte file path` accepted

### L10 — Test-only `panic!`/`unwrap` in tests (acceptable)

### L11 — `find` cleanup not bounded by depth

### L12 — `eval`/`Function` constructor (none found, clean)

---

## Summary by category

| Category | Count | Top severity |
|----------|-------|--------------|
| Concurrency/race | 4 | HIGH |
| Algorithmic correctness | 8 | CRITICAL |
| Data integrity | 5 | CRITICAL |
| Silent failures | 7 | HIGH |
| Boundary/edge | 8 | MEDIUM |
| Hooks/plugin | 12 | HIGH |
| Schema drift | 3 | CRITICAL |
| Numerical | 3 | MEDIUM |

## Top 14 to fix first

| # | File:Line | Issue | Est |
|---|-----------|-------|-----|
| 1 | compat.rs:80-97 | extract_bindings/methods stubs | 1-2 days |
| 2 | brace_graph.rs:141 | empty-file panic | 1h |
| 3 | lib.rs:143-181 | is_definition misclassifies strings | 4h |
| 4 | hooks/* | RCE via RELIARY_BIN_PATH | 30m |
| 5 | hooks/* | TOCTOU + PPID collision | 1h |
| 6 | lazy_occurrence.rs:399 | nested BEGIN swallowed | 2h |
| 7 | ingest.rs:327 | leaked BEGIN on early return | 4h |
| 8 | ingest.rs:522 | INSERT OR REPLACE wipes columns | 30m |
| 9 | structural.rs:65-66 | trait methods not def | 2h |
| 10 | search.rs | BM25 uses log(1+tf) | 1h |
| 11 | lib.rs:112 | porter_stem collisions | 4h (or doc) |
| 12 | mcp.rs (~40) | all errors code -1 | 4h |
| 13 | symbol.rs:807 | find_references ignores threshold for defs | 2h |
| 14 | compat.rs:55-62 | type-flow stubs return 0/None | 2-3 days |

Total estimate: ~15-20 days for all CRITICAL + HIGH. Could be broken into 3-4 sub-plans.

## Recommendation

**Phase 1 (this PR, 1-2 days):** Fix the trivial critical bugs that have minimal risk:
- C2 (brace_graph panic)
- C4 (RCE in hooks) — security blocker
- C5 (TOCTOU in hooks)
- C8 (INSERT OR REPLACE)
- C12 (error codes)
- H10 (find /tmp in hooks)
- M8 (empty name validation)

**Phase 2 (next PR, 3-4 days):** Algorithmic correctness:
- C3 (is_definition)
- C9 (trait methods)
- C10 (BM25)
- C11 (stem collisions — at least document)
- H1 (find_references threshold)
- H4 (if let Some misclassification)
- H9 (lazy_occurrence nested BEGIN)
- C7 (BEGIN leak in ingest.rs)

**Phase 3 (3-4 days):** Concurrency + data integrity:
- C1 (compat stubs)
- C6 (nested BEGIN)
- C8 (file_stats replace)
- C14 (best_context_key)
- H2 (type-flow stubs)
- H3-H9 (brace-graph, lazy tables)
- C13 (test-vs-prod)

**Phase 4 (cleanup, 1-2 days):** Hooks/plugin robustness
- BUG-11 (which blocks startup)
- BUG-13 (no dispose)
- BUG-14 (silent no-op on arg key)
- BUG-23 (findProjectRoot per-file)
- All other hook bugs

Want me to plan Phase 1 (the trivial critical fixes) in detail?