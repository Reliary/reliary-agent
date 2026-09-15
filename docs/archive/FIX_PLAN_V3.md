# FIX PLAN V3 — Second-Pass Bug Audit (45 items)

Found by four explore agents auditing regressions + pre-existing + skipped items.

## Phase 1: Critical Regressions from Edits (R1-R4)

### R1: mcp.rs:110 — tool_definitions_filtered() PANIC in production
- **Bug:** `ALL_TOOL_DEFS.get().unwrap()` — OnceLock only initialized by `tool_definitions()`, which is never called in serve_stdio production path. First `tools/list` panics.
- **Fix:** Change `ALL_TOOL_DEFS.get().unwrap()` → `tool_definitions()` (calls `get_or_init`).

### R2: mcp.rs:100-108 — Two tools missing from PRIMARY_TOOLS
- **Bug:** `reliary_callgraph_v2` and `reliary_methods_on` defined in tool_definitions() but absent from PRIMARY_TOOLS HashSet. Dispatchable but invisible to LLM.
- **Fix:** Add both names to the PRIMARY_TOOLS array at lines 101-108.

### R3: regen.ts:68-77,98-108 — spawn errors unhandled; try/catch dead code
- **Bug:** `spawn()` doesn't throw synchronously on ENOENT. Emits async `'error'` event. No `.on('error')` listener → unhandled exception crashes host.
- **Fix:** Add `child.on('error', () => {});` before `child.unref()` in both triggerReindex and triggerPackRegen.

### R4: claude-pretooluse.sh:13 — @tsv escaping corrupts commands
- **Bug:** `jq @tsv` escapes `\`→`\\`, tab→`\t`, newline→`\n`, but `read -r` doesn't unescape. Commands with backslashes corrupted.
- **Fix:** Revert to separate `jq -r` calls for tool_name and command.

## Phase 2: Critical Pre-existing (P1-P4)

### P1: trace_path.rs:59 — anchor symbol NOT stemmed before phrase lookup
- **Bug:** `SELECT id FROM phrases WHERE phrase = ?1` with raw `[anchor_symbol]`. Phrases stored porter-stemmed. Query always returns 0 → trace_path always empty.
- **Fix:** `let stem = crate::porter_stem(anchor_symbol); ... WHERE phrase = ?` with `[&stem]`.

### P2: config.rs:180 — feature flags NEVER load from config file
- **Bug:** `set_config` writes flat keys (`"features.compress"→"true"`), but `resolve_features_with_source` reads `project_cfg.get("features")` expecting nested object. Key never matches.
- **Fix:** Change `resolve_features_with_source` to read flat keys with `features.` prefix instead of nested object.

### P3: content_cache.rs:20 — DefaultHasher not SHA256 as documented
- **Bug:** Docstring says SHA256, code uses SipHash 64-bit. Birthday collision at ~65K entries.
- **Fix:** Fix docstring to say "64-bit SipHash" (no need for crypto hash in a local cache).

### P4: incremental.rs — entire module is dead code
- **Bug:** `files_needing_reindex`, `load_stored_mtimes`, `record_mtime` have zero callers. Module has multiple bugs (wrong return type, no symlink safety). Incremental reindexing is non-functional.
- **Fix:** Delete the module and its `mod incremental;` declaration. Simpler than fixing dead code with bugs.

## Phase 3: High Severity (H1-H8)

### H1: symbol.rs:96 — bigram keying collides with itself & unigrams
- **Bug:** `bigram_key = (pid_a * 7271 + pid_b) + 10_000_000`. Self-collision between pairs.
- **Fix:** Use u64 pair packing: `(pid_a as u64) << 32 | pid_b as u64`. Adjust the map key type.

### H2: symbol.rs:754 — goto_def passes anchor_line not hit.line to block_id_at
- **Bug:** `block_id_at(db, hit.file_id, anchor_line)` looks up block at anchor's line in hit's file — meaningless for cross-file hits.
- **Fix:** `block_id_at(db, hit.file_id, hit.line)`.

### H3: type_flow.rs:989 — oi.6 (block_id) used where oi.5 (is_def) intended
- **Bug:** Tuple field index wrong. `oi.6` is block_id, always nonzero, making is_def bonus fire for every candidate.
- **Fix:** Change `oi.6` → check the correct is_def field.

### H4: type_flow.rs:504 — trim_matches strips individual chars m,u,t from type names
- **Bug:** `trim_matches(|c| c=='&'||c=='m'||c=='u'||c=='t'||...)` strips these chars individually. `"mutTask"→"ask"`.
- **Fix:** Replace with explicit prefix stripping: strip `&mut ` then strip generics in `<>`.

### H5: log.rs:33 — file logger is dead code; handle leaked
- **Bug:** `FileLogger::write` has zero production callers. RELIARY_LOG_FILE opens a file but nothing writes to it.
- **Fix:** Wire `write` into the log macro, or delete FileLogger + RELIARY_LOG_FILE code.

### H6: read_summary.rs:94 — phrases_fts table no longer exists
- **Bug:** `SELECT phrase FROM phrases_fts LIMIT 200` — FTS5 table removed. Query always fails.
- **Fix:** Change `phrases_fts` → `phrases`.

### H7: claude-code-gate.sh:24-29 — removed cleanup leaves orphaned gate markers
- **Bug:** Gate markers never removed. PID recycling can bypass gate for new sessions.
- **Fix:** Restore `find /tmp -name 'reliary-gate-*' -mtime +1 -delete 2>/dev/null` line.

### H8: lib.rs:128-132 — trigrams() byte-slicing panics on multibyte UTF-8
- **Bug:** `t[i..i+3]` indexes bytes after `to_lowercase()`. Non-ASCII chars change byte length → panic. Currently zero callers (dead API).
- **Fix:** Use `char_indices()` to iterate, or collect `Vec<char>` and window by 3.

## Phase 4: Medium Severity (M1-M9)

### M1: pack/lib.rs:936,962 — dead file_cache doubles memory
- **Bug:** `file_cache: HashMap<String, String>` stores full file contents but `content` never read. Only `lines_cache` consumed.
- **Fix:** Remove `file_cache`, keep only `lines_cache`.

### M2: content_cache.rs:66 — DB errors swallowed as cache-miss
- **Bug:** `.ok()` converts SqliteFailure into None, indistinguishable from genuine miss.
- **Fix:** Match on `Err(QueryReturnedNoRows)` explicitly; log/propagate other errors.

### M3: symbol.rs:926 — dead_symbols flags public API / main
- **Bug:** `distinct <= 1` flags symbols referenced only within own block. main, pub fns flagged dead.
- **Fix:** Exclude `main` and pub-visible symbols from the dead threshold. At minimum, check for `pub` in the definition line.

### M4: trace_path.rs:54 — LIKE %file ambiguous file resolution
- **Bug:** Common basenames match many files. `LIMIT 1` picks arbitrarily. `%`/`_` are SQL wildcards.
- **Fix:** Prefer exact match first; fall back to suffix match with `ORDER BY LENGTH(file_path) ASC LIMIT 1`.

### M5: type_flow.rs:244 — has_top_level_eq false-negative on any `/`
- **Bug:** `if b == b'/' { return false; }` treats first `/` as comment, missing real `=`. Public twin has opposite defect.
- **Fix:** Only treat `//` (double-slash) as comment. Share one implementation.

### M6: log.rs:37 — rotation desyncs size tracker when rename fails
- **Bug:** `let _ = fs::rename(...)` ignores failure. On failure, `self.size` reset to 0 but file appended to. Rotation never triggers again.
- **Fix:** Only reset size/swap file when rename succeeded. On failure, keep appending with real size.

### M7: config.rs:166 — bare feature names in env silently dropped
- **Bug:** `RELIARY_FEATURES=compress` (no +/-) silently dropped.
- **Fix:** Treat bare names as `+name`.

### M8: reliary-dead/lib.rs:58 — substring is_def matches comments
- **Bug:** `line.contains(&format!("fn {}", token))` matches in comments and larger identifiers.
- **Fix:** Drop `contains` fallback. Use only trimmed-line prefix checks.

### M9: reliary-dead/lib.rs:74 — main/tests/public reported dead
- **Bug:** No special-casing of entry points.
- **Fix:** Maintain allowlist: `main`, `Main`, `__init__`, `setUp`, `tearDown`. Lower confidence for test files.

## Phase 5: Low Severity (L1-L17 + Comment Fix)

### L0: opencode-code-gate.js:6 — gate default mismatch comment
- **Fix:** Change header comment to `Toggle: RELIARY_GATE=0 to disable (default ON)`.

### L1: mcp.rs:26-28 — relpath_with prefix matching ignores path boundaries
- **Fix:** Add trailing `/` to workdir before stripping, or check next char is `/`.

### L2: claude-pretooluse.sh:30-39 — cache files never cleaned up
- **Fix:** Add `find /tmp -name 'reliary-bin-path-*' -mtime +1 -delete` alongside gate cleanup.

### L3: incremental.rs:46 — load_stored_mtimes returns wrong type
- **Note:** Module being deleted in P4. No separate fix needed.

### L4: symbol.rs:211 — cosine u64 overflow on pathological counts
- **Fix:** Use `u128` for intermediate products, or saturating ops.

### L5: trace_path.rs:117 — block_id 100-line window arbitrary
- **Fix:** Increase to 500 or make configurable. Low priority.

### L6: trace_path.rs:187 — depth≥2 always produces hop=2 (depth fiction)
- **Fix:** Document limitation in comment, or implement real multi-hop traversal.

### L7: trace_path.rs:74 — relative-path trimming mis-strips without boundary
- **Fix:** Use `trim_start_matches(&format!("{}/", project_path))`.

### L8: type_flow.rs:1095 — fn_names indexing snaps to wrong entry when OOB
- **Fix:** Return None when out of bounds instead of snapping.

### L9: type_flow.rs:36 — `#` treated as comment in all languages
- **Fix:** Add language parameter; only treat `#` as comment for Python/Shell/Ruby.

### L10: config.rs:69 — HOME fallback to "." on Windows/unset
- **Fix:** Use `dirs::config_dir()` or add USERPROFILE fallback.

### L11: config.rs:218 — set_config validation gaps for apiMode/privacyMode/apiBaseUrl
- **Fix:** Add validation match arms for these keys.

### L12: log.rs:108 — current_level substring matching order-sensitive
- **Fix:** Parse the RUST_LOG directive properly, or at minimum check `reliary_agent=` prefix first.

### L13: content_cache.rs:27 — `n & 0xFFFFFFFFFFFFFFFF` mask is no-op
- **Fix:** Remove the mask.

### L14: content_cache.rs:63 — TOCTOU between SELECT and UPDATE
- **Fix:** Combine via `UPDATE ... RETURNING` (SQLite ≥ 3.35).

### L15: reliary-dead/lib.rs:117 — dedup by name drops distinct dead symbols
- **Fix:** Dedup by `(name, file)` or report all.

### L16: reliary-dead/lib.rs:46 — byte-length vs char-length inconsistency
- **Fix:** Use `.chars().count()` for length check, or document as byte-length intentional.

### L17: incremental.rs:73 — symlink loop safety absent
- **Note:** Module being deleted in P4. No separate fix needed.

## Phase 6: Verification
- `cargo check` — full workspace, 0 errors
- `cargo test -p reliary-agent -p reliary-search` — all pass
- `cargo test -p reliary-pack` — only pre-existing `check_test_fn_appears` failure
- `bash -n hooks/*.sh` — clean
- `node --check hooks/*.js` — clean
