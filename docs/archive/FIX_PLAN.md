# Fix Plan: 34 Bugs (20 Correctness + 14 Performance)

All line numbers are approximate — verify with `grep` before editing.

---

## Phase 1: Correctness Fixes (20 items)

### P1-A: Regressions from proxy removal (do these first)

#### Fix 1 — Orphaned `#[test]` on `test_inject_mcp_server_stdio`
- **File:** `crates/reliary-agent/src/init.rs:~576`
- **Problem:** `test_inject_mcp_server_stdio` lost its `#[test]` attribute. A stray `#[test]` at ~line 594 double-attributes `test_remove_mcp_server`.
- **Fix:** Add `#[test]` before `fn test_inject_mcp_server_stdio`. Remove the stray `#[test]` before `test_remove_mcp_server` if it already has one (check for duplicate attribute).
- **Verify:** `cargo test -p reliary-agent test_inject_mcp_server_stdio`

#### Fix 2 — Dead code: `main_inline_search`, `main_inline_risk`, `main_inline_read_summary`
- **File:** `crates/reliary-agent/src/main.rs:~1971-2018`
- **Problem:** 48 lines of unreachable code. Only caller was deleted `routes.rs`.
- **Fix:** Delete the three functions and any associated helper imports they uniquely use.
- **Verify:** `cargo check -p reliary-agent` (no new errors)

#### Fix 3 — Vestigial proxy file read in `reliary_stats`
- **File:** `crates/reliary-agent/src/mcp.rs:~367`
- **Problem:** Reads `/tmp/reliary_proxy.jsonl` which nothing writes anymore.
- **Fix:** Remove the file-read block and any accumulation of proxy savings stats.
- **Verify:** `cargo check -p reliary-agent`

#### Fix 4 — `"serverUrl"` in `VALID_CONFIG_KEYS`
- **File:** `crates/reliary-agent/src/config.rs:~156`
- **Problem:** Vestigial proxy config key in allow-list.
- **Fix:** Remove `"serverUrl"` entry from the list.
- **Verify:** `cargo check -p reliary-agent`

#### Fix 5 — Stale env var `RELIARY_PROXY_FT_WEIGHT`
- **File:** `crates/reliary-search/tests/ft_weight_gate.rs`
- **Problem:** Test references old proxy-prefixed env var name.
- **Fix:** Rename to current env var name (check `reliary-search/src/` for the actual name used in code — likely `RELIARY_FT_WEIGHT`).
- **Verify:** `cargo test -p reliary-search ft_weight_gate`

### P1-B: Pre-existing functional bugs

#### Fix 6 — MCP key mismatch (`"mcp"` vs `"mcpServers"`)
- **File:** `crates/reliary-agent/src/init.rs:~317` (write), `crates/reliary-agent/src/ux.rs:~459` (read)
- **Problem:** Init writes MCP config under JSON key `"mcp"`, but `has_mcp_server` checks for `"mcpServers"`. Doctor always reports "Not wired" after init.
- **Fix:** Change init.rs to write under `"mcpServers"` (standard opencode key). Verify remove_mcp_server and any other readers use the same key.
- **Verify:** `cargo test -p reliary-agent test_inject_mcp_server` then manually run `reliary init` + `reliary doctor`

#### Fix 7 — `std::env::var("")` in `pipe_to_pager`
- **File:** `crates/reliary-agent/src/main.rs:~362`
- **Problem:** Empty env var name — always errors. `$PAGER` is ignored.
- **Fix:** Change to `std::env::var("PAGER")`.
- **Verify:** `cargo test -p reliary-agent`

### P1-C: Stale doc comments (12 items)

#### Fix 8-19 — Stale proxy/daemon references in comments and help text

| # | File:Line | Current | Fix to |
|---|-----------|---------|--------|
| 8 | `main.rs:~760` | "Tail daemon logs" | "Tail reliary logs" |
| 9 | `main.rs:~801` | "...Cline) and daemon" | "...Cline)" |
| 10 | `main.rs:~803` | "and background daemon" | remove phrase |
| 11 | `main.rs:~834` | "stdio fallback" | "stdio MCP server" |
| 12 | `main.rs:~79` | "Hidden commands like daemon, mcp, veto" | "Hidden commands like mcp, veto" |
| 13 | `main.rs:~145` | `assert!(output.contains("serve"))` | `assert!(output.contains("server"))` |
| 14 | `main.rs:~1490` | "index or proxy" | "index" |
| 15 | `main.rs:~1068` | "daemon down" | "index not found" |
| 16 | `mcp.rs:~55` | "shared by stdio and SSE" | "stdio MCP handler" |
| 17 | `mcp.rs:~118` | "shared use by stdio and SSE" | "stdio MCP handler" |
| 18 | `read_summary.rs:~121` | "Used by the proxy" | "Used by read-summary tool" |
| 19 | `paths.rs:~2` | "Used by proxy MCP endpoints" | "Shared path utilities" |

- **Verify:** `cargo check -p reliary-agent && cargo test -p reliary-agent cli_structure_valid`

### P1-D: Unused import

#### Fix 20 — Remove `use std::time::Duration;`
- **File:** `crates/reliary-agent/src/main.rs:~21`
- **Problem:** Never used (line ~1383 uses fully-qualified `std::time::Duration`).
- **Fix:** Delete the import line.
- **Verify:** `cargo check -p reliary-agent`

---

## Phase 2: Performance Fixes (14 items)

### P2-A: Hot-path P0 fixes (highest impact)

#### Fix 21 — Cache file contents in `extract_symbols_from_index`
- **Files:** `crates/reliary-pack/src/lib.rs:~930-987` (caller), `:~1244-1319` (`read_signature_line`, `read_doc_comment`)
- **Problem:** Each symbol triggers a full `read_to_string` of the entire file. 50-symbol file = 100 full reads. No cache.
- **Fix:** Introduce a `HashMap<PathBuf, Arc<String>>` (or `Rc<String>`) file cache at the `extract_symbols_from_index` level. Pass it (or individual cached contents) into `read_signature_line` and `read_doc_comment` so they accept `&str` content + line number instead of a path. The existing `file_cache` in `read_symbols_and_frequency` (line ~2240) proves the pattern — share it.
- **Implementation detail:**
  ```rust
  // Before extract loop:
  let mut file_cache: HashMap<PathBuf, Arc<String>> = HashMap::new();
  // In the loop, per symbol:
  let content = file_cache.entry(path.clone())
      .or_insert_with(|| Arc::new(std::fs::read_to_string(&path).unwrap_or_default()));
  // Pass content.as_ref() + line_num to read functions
  ```
- **Expected impact:** Eliminates ~99% of file reads during pack generation. Single largest win.
- **Verify:** `cargo test -p reliary-pack`

#### Fix 22 — Batch phrase INSERT+SELECT in `reindex_file`
- **File:** `crates/reliary-agent/src/reindex.rs:~57-89`
- **Problem:** Per-token `INSERT OR IGNORE` + `SELECT id` = 2N queries for N tokens. 500 tokens = 1000+ queries.
- **Fix:** Use a transaction with prepared statements:
  1. Prepare `INSERT OR IGNORE INTO phrases (text) VALUES (?1)` and `SELECT id FROM phrases WHERE text = ?1` once outside the loop.
  2. Execute them in a single transaction.
  3. Even better: use `INSERT ... RETURNING id` (SQLite 3.35+) to combine into 1 query per token.
- **Implementation:**
  ```rust
  let mut insert_stmt = tx.prepare("INSERT OR IGNORE INTO phrases (text) VALUES (?1)")?;
  let mut select_stmt = tx.prepare("SELECT id FROM phrases WHERE text = ?1")?;
  for phrase in &unique_phrases {
      insert_stmt.execute(params![phrase])?;
      let id: i64 = select_stmt.query_row(params![phrase], |row| row.get(0))?;
      phrase_ids.push(id);
  }
  ```
- **Expected impact:** Cuts query count from 2N to 2 prepared-statement executions (amortized).
- **Verify:** `cargo test -p reliary-agent reindex`

#### Fix 23 — Remove redundant `PRAGMA journal_mode = WAL`
- **Files:**
  - `crates/reliary-agent/src/reindex.rs:~10`
  - `crates/reliary-agent/src/main.rs:~597, ~1095, ~1202`
  - `crates/reliary-agent/src/main.rs:~433` (run_vacuum)
- **Problem:** WAL is a persistent DB setting (set once at `schema.rs:49` and `fs_safe.rs:113`). Re-setting forces checkpoint.
- **Fix:** Remove `PRAGMA journal_mode = WAL;` lines. Keep `PRAGMA synchronous = NORMAL;` (per-connection, defensible).
- **Verify:** `cargo test -p reliary-agent`

#### Fix 24 — Reuse existing connection in `reindex_file`
- **File:** `crates/reliary-agent/src/reindex.rs:~122`
- **Problem:** Opens a second SQLite connection for `ensure_all_for_file` while first (line 8) is still in scope.
- **Fix:** Call `ensure_all_for_file` with the existing connection. If the transaction must be committed first, commit then reuse `conn`. Avoid opening connection #2.
- **Verify:** `cargo test -p reliary-agent reindex`

### P2-B: Query/algorithmic P1 fixes

#### Fix 25 — Batch `file_map` lookups in `search_fts5`
- **File:** `crates/reliary-search/src/search.rs:~103-124`
- **Problem:** Per-phrase `SELECT ... FROM file_map WHERE id IN (...)` query.
- **Fix:** Collect all `file_map` IDs across all phrases first, then issue a single `SELECT id, file_path FROM file_map WHERE id IN (...)` with the full set.
- **Verify:** `cargo test -p reliary-search`

#### Fix 26 — Replace O(n²) dedup with HashMap index
- **File:** `crates/reliary-search/src/search.rs:~140`
- **Problem:** `results.iter_mut().find(|r| r.file == file_path)` — linear scan per duplicate.
- **Fix:** Maintain a `HashMap<String, usize>` mapping file_path → index in results vec. O(1) lookup.
  ```rust
  let mut file_index: HashMap<String, usize> = HashMap::new();
  // when adding/updating:
  if let Some(&idx) = file_index.get(&file_path) {
      results[idx].score += score;
  } else {
      file_index.insert(file_path.clone(), results.len());
      results.push(result);
  }
  ```
- **Verify:** `cargo test -p reliary-search`

#### Fix 27 — Read 4KB instead of full file for `is_source_like`
- **File:** `crates/reliary-search/src/search.rs:~152`
- **Problem:** Reads entire file just to check if it looks like source code.
- **Fix:** Use `std::fs::File` + `read` with a 4096-byte buffer. Pass the slice to `is_source_like`.
  ```rust
  use std::io::Read;
  let mut buf = [0u8; 4096];
  let n = std::fs::File::open(&file_path)?.read(&mut buf)?;
  let snippet = std::str::from_utf8(&buf[..n]).unwrap_or("");
  if !is_source_like(snippet) { continue; }
  ```
- **Verify:** `cargo test -p reliary-search`

#### Fix 28 — Read only target line in `find_references_with_source`
- **File:** `crates/reliary-agent/src/mcp.rs:~868`
- **Problem:** Reads entire anchor file to inspect one line.
- **Fix:** Use `BufReader` + `.lines().nth(al-1)` or seek-based read of just the needed line. If the file is small (<64KB) keep `read_to_string` but at least gate on a size check.
- **Verify:** `cargo test -p reliary-agent find_references`

### P2-C: Redundant work P2 fixes

#### Fix 29 — Eliminate double `find_installs()` in doctor
- **File:** `crates/reliary-agent/src/ux.rs:~163, ~205`
- **Problem:** `doctor_checks()` calls `find_installs()`, then `doctor()` calls it again. ~14 subprocesses per doctor run.
- **Fix:** Have `doctor_checks()` return the `installs` vec alongside the check results. Pass it into `doctor()`. Or refactor `doctor()` to call `doctor_checks()` and reuse the returned installs.
- **Verify:** `cargo test -p reliary-agent doctor`

#### Fix 30 — Factor out index discovery in `sift` flow
- **File:** `crates/reliary-agent/src/main.rs:~1069, ~1181`
- **Problem:** `build_read_footer` and `diagnose_failure` each independently walk the directory tree + open a fresh SQLite connection.
- **Fix:** Create a helper `fn find_and_open_index(start: &Path) -> Option<(PathBuf, Connection)>` and call it once. Pass the connection/path into both functions. This naturally combines with Fix 23 (removing redundant WAL pragma).
- **Verify:** `cargo test -p reliary-agent`

#### Fix 31 — Fix `env::var("")` (already Fix 7)
- **Note:** This is the same fix as correctness Fix 7. Apply once.

### P2-D: Allocation waste P3 fixes

#### Fix 32 — Remove unnecessary `.clone()` on owned `body`
- **File:** `crates/reliary-pack/src/lib.rs:~2269`
- **Problem:** `sources.insert(sym.name.clone(), body.clone())` then `body` is iterated. Clone is unnecessary.
- **Fix:** Iterate `body.lines()` first, then move `body` into the insert. Or insert `sym.name.clone()` with `body.clone()` only if body is still needed (it's not).
  ```rust
  for line in body.lines() { /* ... */ }
  sources.insert(sym.name.clone(), body);
  ```
- **Verify:** `cargo test -p reliary-pack`

#### Fix 33 — Avoid double allocation for filtered blob set
- **File:** `crates/reliary-agent/src/reindex.rs:~77-78`
- **Problem:** Allocates a filtered `Vec` then uses it once.
- **Fix:** Use an iterator chain or `retain` on the original collection to avoid the second allocation.
- **Verify:** `cargo test -p reliary-agent reindex`

#### Fix 34 — Single-pass in `reliary_pack_query`
- **File:** `crates/reliary-agent/src/mcp.rs:~516-550`
- **Problem:** Two full passes over `pack_content.lines()`.
- **Fix:** Merge into a single pass. Track state in one loop.
- **Verify:** `cargo test -p reliary-agent pack_query`

---

## Execution Order

1. **Phase 1A** (Fixes 1-5): Clean up proxy removal regressions first — these are our mess.
2. **Phase 1B** (Fixes 6-7): Fix functional bugs that affect users.
3. **Phase 1C** (Fixes 8-19): Stale comments — batch these in one pass per file.
4. **Phase 1D** (Fix 20): Trivial import removal.
5. **Phase 2A** (Fixes 21-24): Hot-path performance — highest impact.
6. **Phase 2B** (Fixes 25-28): Query/algo improvements.
7. **Phase 2C** (Fixes 29-31): Redundant work elimination.
8. **Phase 2D** (Fixes 32-34): Allocation cleanup.

After each phase: `cargo check` + `cargo test` for affected crate.
After all phases: full workspace `cargo build && cargo test`.

---

## Risk Assessment

| Fix | Risk | Notes |
|-----|------|-------|
| 1 | Low | Adding back a test attribute |
| 2 | Low | Deleting dead code |
| 6 | Medium | Changing JSON key — check all readers/writers of opencode.json MCP config |
| 21 | Medium | Changing function signatures in pack — update all callers |
| 22 | Low | Same queries, just prepared |
| 24 | Low | Connection reuse — ensure no borrow conflicts |
| 29 | Low | Refactoring internal function |
| 30 | Medium | Changing sift flow — test with real files |
| All others | Low | |

