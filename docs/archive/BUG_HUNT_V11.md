# Bug Hunt V11 — Complete Findings

4 parallel explore agents audited the entire codebase after V10.
~60 bugs found across 4 areas. All listed below — no filtering.

---

## A. reliary-search (structural, ingest, symbol, type_flow, lazy_tables, search, lib)

### CRITICAL

**A-CRIT-1: `ensure_blocks_for_file` double BEGIN IMMEDIATE**
- File: `lazy_tables.rs:67-72 + 104`
- C6 added SAVEPOINT/BEGIN at top (lines 67-72), but the original `BEGIN IMMEDIATE` at line 104 was never removed. When blocks need inserting, line 104 always fails with "cannot start a transaction within a transaction". Callers using `unwrap_or(0)` silently swallow it — **blocks are never JIT-populated via this path**.
- The `_with_content` variant (line 211) is correct — no double BEGIN.
- Fix: Check `has_blocks` BEFORE opening tx; remove duplicate BEGIN; use RAII guard for early returns.

**A-CRIT-2: `ensure_blocks_for_file` transaction leak on early return**
- File: `lazy_tables.rs:73-75, 83, 87, 94-96`
- Multiple early `return Ok(0)` paths execute AFTER the transaction was opened (lines 67-72) but BEFORE any COMMIT/RELEASE/ROLLBACK. Each leaves an open transaction.
- Fix: Move tx open to after all early-return checks (as `_with_content` does).

### HIGH

**A-HIGH-3: ingest.rs DELETE/CREATE targets removed tables**
- File: `ingest.rs:414-415 (DELETE), 529-537 (CREATE INDEX)`
- C1 removed `scope_binding` and `method_occurrence` tables, but ingest.rs still executes `DELETE FROM scope_binding`, `DELETE FROM method_occurrence`, and CREATE INDEX on both. All fail with "no such table" — errors swallowed by `let _ =`.
- Fix: Remove all references to removed tables from ingest.rs.

**A-HIGH-4: `strip_line_comment` fails after division operator**
- File: `structural.rs:349-358`
- `find_byte_outside_string(line, b'/')` returns the FIRST `/`. If it's a division operator (e.g. `let x = a / b; // comment`), `bytes[pos+1] != b'/'`, so the full line is returned unstripped. The comment text gets tokenized.
- Conversely, `https://example.com` outside a string gets stripped from `://` onward (false positive).
- Fix: Iterate ALL `/` positions outside strings, check each for `//`.

**A-HIGH-5: H1 test-path penalty false positives**
- File: `symbol.rs:825`
- `hit_file_path.contains("test")` matches substrings: "latest", "contest", "protest", "attest", "greatest". `contains("loom")` matches "bloom", "gloom". Production code in these directories gets 15% penalty.
- Fix: Split path by `/` and check if any segment equals "test"/"tests" or matches `test_*`/`*_test`.

**A-HIGH-6: `count_unmatched` doesn't skip block comments**
- File: `ingest.rs:22-39`
- Skips `//` comments (line 35) but NOT `/* */` block comments. Braces inside block comments inflate `open_count`/`close_count`, corrupting brace_depth tracking and cascading misclassification.
- Fix: Track block-comment state across scan; skip braces inside `/* */`.

### MEDIUM

**A-MED-7: `find_references_type_flow` returns empty for >2000 occurrences**
- File: `type_flow.rs:984-986`
- `if n > 2000 { return Ok(vec![]); }` — common identifiers (get, set, value) exceed this silently.
- Fix: Truncate/score top-N instead of returning empty.

**A-MED-8: `is_source_like` UTF-8 truncation excludes source files**
- File: `search.rs:143-152`
- 4KB sample ending mid-multibyte → `from_utf8` fails → `unwrap_or("")` → `is_source_like("")` = false → file silently excluded.
- Fix: Use `from_utf8_lossy`.

**A-MED-9: C9 trait-method false positive on `self_ref`, `selfish`**
- File: `structural.rs:83-89`
- `after.starts_with("self")` matches `self_ref`, `selfish`, `self_handle`. False definition rows.
- Fix: Add word-boundary: `self)`, `self,`, `self `, `self:`, `self == "self"`.

**A-MED-10: H4 control-flow filter misses else/loop/switch/try/catch**
- File: `structural.rs:48-51`
- Only `if`, `while`, `for`, `match` filtered. `else {`, `loop {`, `switch x {`, `try {`, `catch (E e) {`, `} else if cond() {` pass through → false definitions.
- Fix: Expand keyword list; handle leading `}`.

**A-MED-11: `find_references` threshold bypassed for ALL definitions**
- File: `symbol.rs:545`
- `if final_sim >= threshold || is_def != 0` — every def included regardless of similarity. Common method names flood results.
- Fix: Require threshold for defs too, or use separate def-threshold.

### LOW

**A-LOW-12: `scan_last_identifier` dead lifetime check**
- `structural.rs:280` — `if first == b'\''` unreachable (line 276 already returns for non-alpha).

**A-LOW-13: `detect_blocks` dead `else` branch**
- `ingest.rs:100` — `else { start }` unreachable.

**A-LOW-14: BM25 count saturates at 31**
- `search.rs:127` — `unpack_count` caps at 31. Files with 50 occurrences score same as 31.
- Fix: Consult `count_overflow` table on saturation.

**A-LOW-15: `find_references_type_flow` redundant `cand_line_text` recompute**
- `type_flow.rs:1085 + 1202` — computed twice in hot loop.

**A-LOW-16: L5 dedup — `col` is index in unique list, not byte column**
- `lib.rs:99-112` — after dedup, `col` from enumerate is not the real byte position. Second occurrence of same identifier on a line is not stored. Pre-existing (phrase_locations HashMap keyed by stem), not a regression.

---

## B. reliary-agent (mcp, reindex, main, init)

### CRITICAL

**B-CRIT-1: `serve_stdio` eager-index `return;` breaks MCP handshake**
- File: `mcp.rs:1391, 1395, 1415`
- When `RELIARY_EAGER_INDEX` is set, error paths execute `return;` which exits `serve_stdio()` entirely. The `respond(id, ...)` for `initialize` is never sent. MCP client hangs.
- Fix: Replace `return;` with labeled break or restructure.

### HIGH

**B-HIGH-2: Byte-slice truncation panics on non-ASCII UTF-8**
- File: `mcp.rs:833, 839, 845, 1108`
- `&cg.source_preview[..80]` panics if byte 80 is inside a multibyte codepoint. Reachable via any file with CJK/emoji/accented chars.
- Fix: Use `char_indices` / `floor_char_boundary`.

**B-HIGH-3: `reliary_dead` MCP tool non-recursive**
- File: `mcp.rs:334-345`
- Uses `std::fs::read_dir` (immediate children only). CLI version uses `walkdir::WalkDir`. MCP misses all dead code in subdirectories.
- Fix: Use `walkdir::WalkDir`.

**B-HIGH-4: `require_name` (M7) defined but never called**
- File: `mcp.rs:154`
- Dead code. Every symbol tool does `unwrap_or("")`, silently accepting empty/missing name.
- Fix: Replace all `unwrap_or("")` name extractions with `require_name(args)?`.

### MEDIUM

**B-MED-5: `reliary_fix` accepts partial old/new pair**
- File: `mcp.rs:302-306`
- `old` without `new` → deletes every occurrence of `old`. `new` without `old` → pathological.
- Fix: Require both non-empty together.

**B-MED-6: `open_symbol_index` returns `Err(Success(...))`**
- File: `mcp.rs:701-705`
- Missing index wrapped as `Err(DispatchResult::Success(...))`. Client sees `isError=false`.
- Fix: Use `DispatchResult::Error(codes::NOT_FOUND, ...)`.

**B-MED-7: Wrong error codes — path-traversal returns DB_ERROR**
- File: `mcp.rs:1229, 1269, 664-667`
- safe_path failure → `err_db` instead of `err_invalid_path`. Parse error → `err_db` instead of `err_invalid_params`.
- Fix: Use correct error code per category.

**B-MED-8: `respond`/`respond_error` swallow I/O failures**
- File: `mcp.rs:38-60`
- `let _ = serde_json::to_writer(...)` — broken pipe silently ignored. Server keeps spinning.
- Fix: Propagate write errors; exit serve loop on broken pipe.

**B-MED-9: `inject_opencode_plugin` assumes dev-build path**
- File: `init.rs:353-356`
- `ancestors().nth(3)` assumes `<repo>/target/release/reliary`. Installed binary → wrong path.
- Fix: Don't derive repo root from binary path.

### LOW

**B-LOW-10: `Man` subcommand uses `.expect()`**
- `main.rs:1835-1836, 1840` — panics on I/O failure.

**B-LOW-11: `reliary_find_references_boltzmann` listed twice**
- `mcp.rs:213 (inline) + 438 (group)` — group entry unreachable.

**B-LOW-12: `reliary_query_ast` self-contradictory comment**
- `mcp.rs:441-444`

**B-LOW-13: `reliary_dead` confidence "low" silently means "all"**
- `mcp.rs:346-352` — ambiguous design.

**B-LOW-14: `reindex_file` inconsistent ROLLBACK**
- `reindex.rs:49-73` — early DELETE errors `return false` without ROLLBACK (saved by Drop).

**B-LOW-15: `reliary_retrieve`/`reliary_stats` hardcode CWD-relative cache**
- `mcp.rs:412, 425` — `.reliary/cache.sqlite` without safe_path.

**B-LOW-16: `reliary_pack` silently maps invalid format to L2L3**
- `mcp.rs:519-523` — `format: "garbage"` → l2l3 instead of INVALID_PARAMS.

---

## C. hooks + opencode-plugin (shell, JS, TS)

### CRITICAL

**C-CRIT-1: `dist/index.js` stale — ships pre-H13 `execFileSync('which')`**
- File: `opencode-plugin/dist/index.js:2, 10-18`
- dist predates H13/H14 fixes. Still calls `execFileSync("which", ...)`. `dispose()` absent. `package.json main` points to this stale file.
- Fix: Run `npm run build` (tsup).

**C-CRIT-2: `$RELIARY_BIN` unquoted in bash -c rewrite**
- File: `claude-pretooluse.sh:71`
- `new_cmd="$RELIARY_BIN wrap bash -c '$escaped_cmd'"` — path with spaces splits.
- Fix: Quote: `"'$RELIARY_BIN' wrap bash -c '$escaped_cmd'"`.

**C-CRIT-3: No newline guard in bash sed escape**
- File: `claude-pretooluse.sh:70`
- Unlike JS hook (line 56), bash hook has no newline check. Command with `\n` breaks single-quote context.
- Fix: `case "$cmd" in *$'\n'*) exit 0;; esac` before escape.

### HIGH

**C-HIGH-4: `index.ts` tool matching case-sensitive + misses variants**
- File: `index.ts:26`
- Strict `=== 'write'/'edit'`. Misses `Write`, `EDIT`, `patch`, `replace`, `str_replace`, `multiedit`.
- Fix: `toLowerCase()` + expanded list.

**C-HIGH-5: `dispose()` never registered with lifecycle**
- File: `index.ts` / `regen.ts:175-192`
- H14 added `dispose()` but `index.ts` never imports/calls/registers it. Pending debounced reindexes dropped on exit.
- Fix: `process.on('exit', dispose)` in index.ts.

**C-HIGH-6: `opencode-reliary-sift.js` dead `execFileSync` import + inline `require("fs")`**
- File: `opencode-reliary-sift.js:8, 25, 31`
- Import unused after H13. `require("fs")` called inline twice. No try/catch in `findReliary`.
- Fix: Remove import; hoist require; add try/catch.

**C-HIGH-7: PATH walk missing executable-bit check**
- File: `opencode-reliary-sift.js:31`, `regen.ts:40`
- `existsSync` returns true for non-executable files and directories named `reliary`.
- Fix: Add `fs.accessSync(candidate, X_OK)`.

**C-HIGH-8: Paths with spaces silently rejected, no diagnostic**
- File: `opencode-reliary-sift.js:13`
- `SAFE_BIN_RE` rejects spaces. Users with spaces in home dir get silent "binary not found".
- Fix: Emit diagnostic stderr when isValidBin rejects an existing path.

### MEDIUM

**C-MED-9: `claude-code-gate.sh` gate fires every call (C5 over-correction)**
- File: `claude-code-gate.sh:25`
- Session key includes nanosecond timestamp → always unique → mkdir always succeeds → gate never blocks.
- Fix: Drop `%N` from key.

**C-MED-10: `opencode-code-gate.js` exit-handler leak on crash**
- File: `opencode-code-gate.js:23`
- `rmdirSync` only on clean exit. SIGKILL/crash leaks `/tmp/reliary-gate-*`.
- Fix: Startup sweep of stale markers.

**C-MED-11: `regen.ts` `discoverReliary` missing SAFE_BIN_RE**
- File: `regen.ts:30-44`
- JS hook validates binary path; TS version doesn't. Asymmetric defense.
- Fix: Add same regex check.

**C-MED-12: `regen.ts` `dispose()` re-discovers binary per file in loop**
- File: `regen.ts:186-188`
- 100 queued files → 100 PATH walks → 1400 stat syscalls. Silent catch swallows errors.
- Fix: Hoist `discoverReliary()` above loop; add error listener.

**C-MED-13: `index.ts` no try/catch around `onFileEdit`**
- File: `index.ts:33`
- EMFILE during bulk edit throws synchronously, propagates to hook runner.
- Fix: Wrap in try/catch.

**C-MED-14: Module-level debounce state shared across instances**
- File: `regen.ts:64-65`
- Two workspaces share same queue. Files from A reindexed with B's opts.bin.
- Fix: Factory pattern with closure-scoped state.

**C-MED-15: `claude-pretooluse.sh` cache file never cleaned up**
- File: `claude-pretooluse.sh:41`
- `/tmp/reliary-bin-path-${PPID}` leaked per session. World-readable (no umask 077).
- Fix: `trap 'rm -f "$_CACHE_FILE"' EXIT` or move to `~/.cache/reliary/`.

### LOW

**C-LOW-16: `claude-session-reminder.sh` comment contradicts default**
- Line 4: says default OFF, code is `:-1` (default ON).

**C-LOW-17: `claude-pretooluse.sh` dead unreachable empty check**
- Lines 64-66 — after exec check, empty check is unreachable.

**C-LOW-18: `opencode-reliary-sift.js` unused `execFileSync` import**
- Line 8 (same as C-HIGH-6 once other points addressed).

**C-LOW-19: `regen.ts` default export omits `dispose`**
- Line 194 — incomplete and unused default export.

**C-LOW-20: `index.ts` stale comment referencing execFileSync**
- Line 32 — references removed synchronous behavior.

**C-LOW-21: `regen.ts` vs `gate.js` inconsistent `--` arg separator**
- `regen.ts:91` uses `--`, `gate.js:72` doesn't. File paths starting with `-` misparsed.

**C-LOW-22: No `unhandledRejection` guard in any JS hook**
- Defense-in-depth gap; low risk today.

---

## D. reliary-core / reliary-pack (fs_safe, pack generation)

### HIGH

**D-HIGH-1: `safe_path` rejects non-existent paths**
- File: `mcp.rs:12` (note: safe_path is in reliary-agent, not reliary-core)
- `canonicalize()` requires path to exist. Write/create tools get "No such file or directory".
- Fix: Canonicalize parent dir + append filename for write paths.

### MEDIUM

**D-MED-2: `atomic_write` temp filename uses only PID — unsafe for same-process concurrency**
- File: `fs_safe.rs:28`
- Two threads writing same path collide on identical temp filename.
- Fix: Add per-call nonce (counter + thread id).

**D-MED-3: `atomic_write` not atomic on Windows**
- File: `fs_safe.rs:40`
- `fs::rename` fails if destination exists on Windows.
- Fix: Use `ReplaceFile` or gate as POSIX-only.

**D-MED-4: `atomic_write` missing directory fsync**
- File: `fs_safe.rs:35-44`
- File content synced but parent directory not. Rename may not survive power loss.
- Fix: Open + sync_all parent directory after rename.

**D-MED-5: `safe_read` TOCTOU — size check vs unbounded read**
- File: `fs_safe.rs:52-68`
- `metadata().len()` checked, then `read_to_string` reads unbounded. File growth between calls defeats OOM guard.
- Fix: Use bounded reader (`take(MAX_FILE_SIZE + 1)`).

**D-MED-6: `build_cross_refs_from_index` `.expect()` panics on corrupt index**
- File: `pack/lib.rs:2097, 2109`
- SQL prepare/query failure panics. Sibling function handles gracefully.
- Fix: Convert to `?` or match-and-return-empty.

**D-MED-7: Pack generator unbounded file reads bypass MAX_FILE_SIZE**
- File: `pack/lib.rs:949, 2211, 2252`
- `read_to_string` with no size cap on indexed files.
- Fix: Route through `safe_read`.

**D-MED-8: Pack specificity metric broken — `LIKE '%_%'` always true**
- File: `pack/lib.rs:162-163`
- `_` is wildcard in LIKE → query matches any non-empty phrase. Inflates specificity_ratio (30% of score).
- Fix: Escape underscore: `LIKE '%\_%' ESCAPE '\'`.

### LOW

**D-LOW-9: `safe_path` TOCTOU between canonicalize and use**
- `mcp.rs:12-17` — symlink swap between validation and use. Low for single-user workflows.

**D-LOW-10: `safe_path` no explicit symlink policy documented**
- No test covers "symlink pointing outside workdir."

**D-LOW-11: `atomic_write` predictable temp filename**
- `fs_safe.rs:28` — attacker could pre-create temp name as symlink. Low risk for project-dir writes.

**D-LOW-12: `safe_read` follows symlinks with no confinement**
- `fs_safe.rs:50-69` — caller's responsibility gap.

**D-LOW-13: `safe_read` UTF-8-only, silently**
- `fs_safe.rs:68` — non-UTF-8 files rejected with unclear error.

**D-LOW-14: `find_body_end` brace-depth underflow**
- `pack/lib.rs:2338` — `depth -= 1` has no underflow guard. `}` in string literal → premature termination.
- Fix: `if depth > 0 { depth -= 1; }`.

**D-LOW-15: SQL operator-precedence ambiguity in specificity WHERE**
- `pack/lib.rs:162-163` — `(A OR B AND C)` is fragile. Currently works by accident.

**D-LOW-16: `GateDecision::Full` always uses L2L3, never PackFormat::Full**
- `pack/lib.rs:502-510` — naming collision is maintenance trap.

**D-LOW-17: `safe_open_db` docstring contradicts code**
- `fs_safe.rs:88-114` — docs say speed PRAGMAs, code sets WAL+NORMAL.

**D-LOW-18: `safe_open_db` swallows PRAGMA errors**
- `fs_safe.rs:108-112` — WAL failure silently ignored. Caller operates in rollback mode.

**D-LOW-19: `if let Some(_)` style in pack/lib.rs:1613**
- Should be `.is_some()`.

**D-LOW-20: No proxy/daemon remnants in reliary-core or reliary-pack**
- Confirmed clean.

---

## Summary

| Severity | Count |
|----------|-------|
| CRITICAL | 7 (A:2, B:1, C:3, D:0) + 1 stale dist |
| HIGH | 15 (A:4, B:3, C:5, D:1) + 1 stale dist |
| MEDIUM | 22 (A:5, B:5, C:7, D:5) |
| LOW | 24 (A:5, B:7, C:7, D:5) |
| **Total** | **~64 distinct bugs** |

### Fix priority

1. **CRITICAL** (7): A-CRIT-1, A-CRIT-2, B-CRIT-1, C-CRIT-1, C-CRIT-2, C-CRIT-3, + rebuild dist
2. **HIGH** (15): A-HIGH-3 through A-HIGH-6, B-HIGH-2 through B-HIGH-4, C-HIGH-4 through C-HIGH-8, D-HIGH-1
3. **MEDIUM** (22): all medium items
4. **LOW** (24): cleanup and edge cases
