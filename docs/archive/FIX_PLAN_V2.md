# Performance Bug Fix Plan V2 — Deep Audit

50 new performance bugs found across 4 deep-dive agents. Deduplicated against V1 fixes (WAL pragma, search.rs dedup/4KB read, mcp.rs BufReader, double find_installs, find_open_index, file content cache in extract_symbols).

All line numbers approximate — verify with grep before editing.

---

## Tier 0 — CRITICAL: Reindex Hot Path (runs after EVERY file edit)

### P0-1: Double tokenization of edited file
- **File:** `crates/reliary-agent/src/reindex.rs:137` + `:55`
- **Problem:** `reindex_single_file` calls `tokenize(content)` at line 137 to get the count, then passes raw `content` to `reindex_file` which calls `tokenize(content)` AGAIN at line 55. Full file tokenized twice per edit.
- **Fix:** Change `reindex_file` signature to accept `&[String] phrases` (or pass count + phrases). Remove the second `tokenize` call.
- **Impact:** Eliminates 50% of tokenization work on the hot path.

### P0-2: Edited file re-read from disk 4× by ensure_all_for_file
- **File:** `crates/reliary-agent/src/reindex.rs:126` → `lazy_tables.rs:54,136,167` + `lazy_occurrence.rs:352`
- **Problem:** `reindex_file` has `content` in memory. Calls `ensure_all_for_file(&db, file_id)` which calls 4 functions, each independently does `fs::read_to_string(file_path)`. That's 4 redundant disk reads of the same file whose content is already held by the caller.
- **Fix:** Add `ensure_all_for_file_with_content(db, file_id, content: &str)` that threads the in-memory content down. Each sub-function gets a `_with_content` variant.
- **Impact:** Eliminates 4 disk reads per edit.

### P0-3: 5 DELETEs execute OUTSIDE the transaction
- **File:** `crates/reliary-agent/src/reindex.rs:43-51`
- **Problem:** Lines 43-47 execute 5 `DELETE` statements BEFORE `BEGIN` at line 49. Each DELETE is its own auto-commit transaction → up to 5 separate WAL-frame flushes (fsyncs).
- **Fix:** Move `BEGIN` to before line 43 so all 5 deletes + the rebuild share one transaction.
- **Impact:** Reduces 5 fsyncs to 1 per edit.

### P0-4: Reindex connection missing 5 of 6 speed PRAGMAs
- **File:** `crates/reliary-agent/src/reindex.rs:10`
- **Problem:** Only sets `PRAGMA synchronous = NORMAL;`. Missing: `cache_size`, `mmap_size`, `temp_store`, `lock_timeout`. Compare `fs_safe.rs:122-131` which sets all 6.
- **Fix:** Call `reliary_core::apply_speed_pragma(&db)` after opening.
- **Impact:** Warms cache, enables memory-mapping, reduces I/O.

### P0-5: Redundant dedup of already-unique set
- **File:** `crates/reliary-agent/src/reindex.rs:97-99`
- **Problem:** `affected_ids` is already a `HashSet` (line 56), so iteration yields unique values. The `seen` HashSet + `insert` check is dead code.
- **Fix:** Delete the `seen` set and the `if !seen.insert() { continue; }` guard.
- **Impact:** Removes an allocation + redundant check per edit.

---

## Tier 1 — CRITICAL: MCP Server (runs on every LLM tool call)

### P1-6: DB connection opened per tool call — no pooling
- **File:** `crates/reliary-agent/src/mcp.rs:136,393,417,616` (`open_symbol_index`)
- **Problem:** Every MCP tool call that touches the index opens a fresh `rusqlite::Connection`: file open, header read, page cache init, schema parse, PRAGMA. On 50+ tool calls per session, that's 50 connection setups. The `serve_stdio` server is single-threaded (line 1132), so connection reuse is safe.
- **Fix:** Cache connection in `OnceLock<Connection>` or `thread_local!`. Open once during `initialize`, store statically, reuse for all subsequent tool calls.
- **Impact:** Eliminates per-call connection overhead.

### P1-7: tool_definitions() rebuilds 26 JSON Values every tools/list
- **File:** `crates/reliary-agent/src/mcp.rs:56-116`
- **Problem:** Allocates 26 `serde_json::Value` objects (each with nested Map + long description strings), then filters with `PRIMARY.contains()` — linear scan of 22-element slice × 26 tools = 572 comparisons. Called on every `tools/list` request.
- **Fix:** `static TOOLS: OnceLock<Vec<serde_json::Value>>`. Build once, return `&'static`. Replace `PRIMARY.contains()` with `HashSet<&str>`.
- **Impact:** Eliminates all allocation on tools/list.

### P1-8: args deep-cloned on every tools/call
- **File:** `crates/reliary-agent/src/mcp.rs:1122`
- **Problem:** `params.get("arguments").and_then(|v| v.as_object()).cloned()` deep-clones the entire arguments JSON Map. `dispatch_tool_call` only takes `&serde_json::Map`. For tools with large `text`/`old`/`new` args, this is a full deep copy.
- **Fix:**
  ```rust
  let empty = serde_json::Map::new();
  let args = params.get("arguments").and_then(|v| v.as_object()).unwrap_or(&empty);
  dispatch_tool_call(name, args);
  ```
- **Impact:** Eliminates deep copy of potentially KB-sized JSON per tool call.

### P1-9: relpath() calls current_dir() syscall per hit
- **File:** `crates/reliary-agent/src/mcp.rs:22-31`, called at `:670,711,800,1002`
- **Problem:** `relpath()` does `std::env::current_dir()` (getcwd syscall) + `to_string_lossy().to_string()` (allocation) on EVERY call. Inside `.iter().map()` loops over hits. 50 hits = 50 syscalls + 50 allocations for a value that never changes.
- **Fix:** Compute `workdir` once before each loop. Create `fn relpath_with(file_path: &str, workdir: &str) -> String`.
- **Impact:** Eliminates N syscalls per search/references call.

### P1-10: respond/respond_error allocate intermediate String
- **File:** `crates/reliary-agent/src/mcp.rs:33-42,44-53`
- **Problem:** `serde_json::to_string(&response)` allocates a String, then `writeln!` writes it through Display formatting.
- **Fix:** `serde_json::to_writer(&mut out, &response).ok(); writeln!(out).ok();`
- **Impact:** Minor per-request overhead, zero-cost fix.

---

## Tier 2 — CRITICAL: Hooks/Plugin (runs on every tool invocation)

### P2-11: execFileSync BLOCKS event loop — not actually fire-and-forget
- **File:** `opencode-plugin/src/index.ts:31-33`
- **Problem:** Comment says "Fire-and-forget; do not await" but `onFileEdit` → `triggerReindex` → `execFileSync` is synchronous and blocks the Node.js event loop until child exits (up to 10s timeout). Every write/edit stalls the entire opencode process.
- **Fix:** Use `spawn()` with `stdio: 'ignore', detached: true` + `child.unref()`.
  ```typescript
  import { spawn } from 'child_process';
  const child = spawn(bin, ['reindex-file', filePath], { stdio: 'ignore', detached: true });
  child.unref();
  ```
- **Impact:** Unblocks event loop during reindex — biggest UX win.

### P2-12: discoverReliary() called 2× per file edit
- **File:** `opencode-plugin/src/regen.ts:65,88,121-122`
- **Problem:** `onFileEdit` calls `triggerReindex` and `triggerPackRegen`, each independently calls `discoverReliary()` which spawns `which reliary` (up to 2s timeout). Two subprocess spawns per edit when one would do. `opts.bin` exists but caller never sets it.
- **Fix:** In `onFileEdit`, resolve once:
  ```typescript
  const bin = opts.bin ?? discoverReliary();
  const resolvedOpts = { ...opts, bin };
  triggerReindex(filePath, resolvedOpts);
  triggerPackRegen(filePath, resolvedOpts);
  ```
- **Impact:** Halves subprocess spawns per edit.

### P2-13: find /tmp scan on every grep/glob/read
- **File:** `hooks/claude-code-gate.sh:25`
- **Problem:** `find /tmp -name 'reliary-gate-*' -mtime +1 -delete` traverses entire `/tmp` on every matching tool call. Multi-millisecond disk walk per invocation for stale marker cleanup.
- **Fix:** Remove entirely (markers are PID-keyed, self-cleaning), or gate behind `[ $((RANDOM % 100)) -eq 0 ]` (~1% of calls).
- **Impact:** Eliminates /tmp walk on every tool call.

### P2-14: which subprocess on every bash tool call
- **File:** `hooks/claude-pretooluse.sh:30`
- **Problem:** `which reliary 2>/dev/null || which reliary-agent 2>/dev/null` — up to 2 subprocess spawns per bash command. Result never changes within a session.
- **Fix:** Cache to marker file on first call, or document `RELIARY_BIN_PATH` as the contract.
- **Impact:** Eliminates 1-2 subprocess spawns per bash call.

### P2-15: Three jq subprocess spawns per bash tool call
- **File:** `hooks/claude-pretooluse.sh:11,24,37`
- **Problem:** Three separate `jq` invocations: extract tool_name, extract command, encode output. ~5-6 total subprocesses per bash call with the `which` calls.
- **Fix:** Single `jq` call: `jq -r '[.tool_name, .tool_input.command] | @tsv'`. For output encoding, use `jq -n --arg c "$new_cmd" '{tool_input:{command:$c}}'`.
- **Impact:** Reduces 3 jq spawns to 1-2.

### P2-16: existsSync stat per tool call after gate already triggered
- **File:** `hooks/opencode-code-gate.js:14,35`
- **Problem:** After first gate trigger, every subsequent grep/read/glob does `existsSync(GATE_MARKER)` — stat syscall for a file that will exist all session.
- **Fix:** Cache in-memory:
  ```javascript
  let gateTriggered = false;
  function gateMarkerExists() {
    if (gateTriggered) return true;
    gateTriggered = existsSync(GATE_MARKER);
    return gateTriggered;
  }
  ```
- **Impact:** Eliminates stat syscall on every tool call after gate fires.

### P2-17: Regex on every bash command in sift hook
- **File:** `hooks/opencode-reliary-sift.js:35`
- **Problem:** `cmd.replace(/'/g, "'\\''")` scans full command string on every qualifying bash call, even when no single quotes present.
- **Fix:** `const escaped = cmd.includes("'") ? cmd.replace(/'/g, "'\\''") : cmd;`
- **Impact:** Minor, but compounds across sessions.

---

## Tier 3 — CRITICAL: Pack Generation (reliary-pack/lib.rs)

### P3-18: O(S×F×P) cross-ref building in build_cross_refs_from_index
- **File:** `crates/reliary-pack/src/lib.rs:2131-2175`
- **Problem:** For each symbol (S), scans ALL files (F) × phrases per file (P). 1000 symbols × 700 files × 50 phrases = 35M iterations. Inner scans check every phrase twice (any() then for loop).
- **Fix:** Build two inverted indexes once:
  - `non_def_to_files: HashMap<&str, Vec<i64>>` — phrase → files where used non-def
  - `file_to_defs: HashMap<i64, Vec<&str>>` — file → symbols defined
  - Per symbol: O(files_using_symbol × defs_per_file), typically 100-1000× faster.
- **Impact:** Largest single pack-gen speedup. O(n²) → O(n) effectively.

### P3-19: Per-symbol file re-splitting (file cache incomplete)
- **File:** `crates/reliary-pack/src/lib.rs:944-980,1252,1262`
- **Problem:** File cache caches raw `String` content (V1 fix). But `read_signature_line` does `.lines().nth(n)` walking from byte 0 every call, and `read_doc_comment` does `.lines().collect()` re-splitting entire file — each called per symbol. 100 symbols in a file = 100 walks/collects.
- **Fix:** In `extract_symbols_from_index`, after caching content, split once: `let lines: Vec<&str> = content.lines().collect();`. Pass `&[&str]` to both functions. Change signatures to accept slices.
- **Impact:** Eliminates per-symbol re-splitting. 100× reduction for 100-symbol files.

### P3-20: O(n²) cross-ref expansion in slice_pack_for_query
- **File:** `crates/reliary-pack/src/lib.rs:747-788`
- **Problem:** Three nested loops: selected entries × refs × all entries. Plus `expanded.iter().any()` linear scan that grows. Plus `format!("{}/", ref_name)` allocation inside innermost loop. Total O(selected × refs × entries × expanded).
- **Fix:**
  - Build `name_to_entry: HashMap<&str, &PackEntry>` once after parsing
  - Maintain `expanded_names: HashSet<String>` alongside `expanded: Vec<&PackEntry>`
  - Precompute `ref_name_slash = format!("{}/", ref_name)` outside entry loop
- **Impact:** O(n²) → O(n) for pack slicing. Runs per query.

### P3-21: Pointless join→re-split cycle
- **File:** `crates/reliary-pack/src/lib.rs:2266-2269` (and `:2217-2219`)
- **Problem:** `lines[start..end].join("\n")` creates a new String (copying all bytes), then `.lines()` re-parses it looking for newlines. Happens per symbol. 1000 symbols × ~2.5KB each = 2.5MB of wasted copies.
- **Fix:** Iterate the slice directly:
  ```rust
  for line in &lines[start..end] {
      let trimmed = line.trim();
      // ... skeleton/frequency work
  }
  ```
  Keep `body` only for `sources` insert and `render_entry`.
- **Impact:** Eliminates 1000+ allocations during pack gen.

### P3-22: to_lowercase() allocation per candidate in is_definition_like
- **File:** `crates/reliary-pack/src/lib.rs:1189`
- **Problem:** Inside `is_definition_like`, called per candidate symbol from index: `let lower = check.to_lowercase();` allocates new String + Unicode case mapping. Signatures are short ASCII.
- **Fix:** Use `to_ascii_lowercase()` (no Unicode overhead) or `contains_ignore_ascii_case`-style checks.
- **Impact:** Eliminates per-candidate allocation in definition detection.

### P3-23: Duplicate file reads across extract_symbols and read_symbols_and_frequency
- **File:** `crates/reliary-pack/src/lib.rs:932` vs `:2245` (and `:2202`)
- **Problem:** Three independent file caches exist: `extract_symbols_from_index` (HashMap<String,String>), `read_symbols_and_frequency` (HashMap<String,Vec<String>>), `read_symbols_for_subset` (HashMap<String,Vec<String>>). In `generate_pack`, the first reads all files, then the second re-reads the same files. 700 files = 1400 disk reads.
- **Fix:** Share a single cache. Either have `extract_symbols_from_index` return its cache, or create a `FileContentStore` struct that caches both raw content and split lines.
- **Impact:** Halves file reads during pack generation.

---

## Tier 4 — HIGH: Search Path

### P4-24: COUNT(*) + AVG() full table scans on every search
- **File:** `crates/reliary-search/src/search.rs:34-35`
- **Problem:** `SELECT COUNT(*) FROM file_map` and `SELECT AVG(token_len) FROM file_stats` scan entire tables on every query. These values only change at reindex time.
- **Fix:** Store both in `meta` table (schema.rs:94) at index/reindex time. Read scalars here.
- **Impact:** Eliminates 2 full table scans per search.

### P4-25: N+1 query — one file_map query per result phrase
- **File:** `crates/reliary-search/src/search.rs:103-124` (duplicated at `:202-215` in `who_calls`)
- **Problem:** Each phrase hit prepares+executes a separate dynamic `IN(...)` query. K matching phrases = K prepare/exec round trips.
- **Fix:** Pre-load `(id, file_path, token_len)` from `file_map` into `HashMap<i64, (String, f64)>` once at function entry. Resolve all file_ids by lookup.
- **Impact:** Eliminates per-phrase SQL round trips.

### P4-26: O(M) linear scan for file_id → path in search inner loop
- **File:** `crates/reliary-search/src/search.rs:129`
- **Problem:** `file_paths.iter().find(|(id, _, _)| id == fid)` — linear scan per result entry.
- **Fix:** Same HashMap as P4-25.
- **Impact:** O(M) → O(1) per lookup.

### P4-27: Synchronous file I/O in search inner loop
- **File:** `crates/reliary-search/src/search.rs:150-159`
- **Problem:** For every new candidate result, opens+reads file from disk for `is_source_like()` check. Stalls query on disk latency.
- **Fix:** Cache `is_source_like` per path (stable between edits), or apply as post-filter after collecting candidates, or defer.
- **Impact:** Reduces disk I/O during search.

### P4-28: COUNT(*)...LIMIT 1 does not short-circuit in lazy-table guards
- **Files:** `crates/reliary-search/src/lazy_tables.rs:24,35,46`; `crates/reliary-search/src/lazy_occurrence.rs:335-339`
- **Problem:** `SELECT COUNT(*) FROM block WHERE file_id = ?1 LIMIT 1` — LIMIT doesn't stop COUNT(*); SQLite scans all matching rows. Four functions do this. All run on every reindex via `ensure_all_for_file`. `has_occurrence` (lazy_occurrence.rs:111) already does it correctly.
- **Fix:** `SELECT EXISTS(SELECT 1 FROM block WHERE file_id = ?1)` or `SELECT 1 ... LIMIT 1` + check row presence.
- **Impact:** Eliminates 4 full scans per reindex.

---

## Tier 5 — HIGH: Tokenization & String Processing

### P5-29: scan_identifiers triple-allocates and re-validates
- **File:** `crates/reliary-search/src/lib.rs:99-109`
- **Problem:**
  1. `split(|c| ...)` allocates `Vec<&str>`
  2. `.chars().all(|c| ...)` re-validates what split already guaranteed (redundant filter)
  3. `.to_lowercase()` allocates a new `String` per token even for already-lowercase ASCII
- **Fix:** Byte-level scan (like schema.rs:199-209 already does). Use `to_ascii_lowercase` which can stay in place for ASCII.
- **Impact:** Significant — hottest function in tokenizer, invoked per line per file.

### P5-30: porter_stem allocates even when returning unchanged
- **File:** `crates/reliary-search/src/lib.rs:112-126`
- **Problem:** `let w = word.trim().to_lowercase();` always allocates. For tokens < 4 chars (very common: `int`, `let`, `i`, `x`), returns the owned `w` unchanged. Called once per token.
- **Fix:** Take `&str`, operate on bytes. Return `Cow<str>` or write into reusable buffer. At minimum `to_ascii_lowercase`.
- **Impact:** Reduces per-token allocations.

### P5-31: classify_line allocates Vec per line during ingest
- **File:** `crates/reliary-search/src/schema.rs:216`
- **Problem:** `let words: Vec<&str> = s.split_whitespace().collect();` — Vec exists only to compute average word length.
- **Fix:** Compute sum + count in one pass over `s.split_whitespace()` without collecting.
- **Impact:** Eliminates per-line Vec allocation during ingest.

### P5-32: trigrams slices by byte index — UTF-8 panic
- **File:** `crates/reliary-search/src/lib.rs:129-133`
- **Problem:** `t[i..i+3]` indexes bytes; non-ASCII char boundary → panic. Latent crash in tokenizer. (Correctness, not perf.)
- **Fix:** Use `char_indices` or operate on bytes consistently.
- **Impact:** Prevents crash on non-ASCII input.

---

## Tier 6 — HIGH: File Walking & I/O

### P6-33: apply_speed_pragma fires 6 separate execute_batch calls
- **File:** `crates/reliary-core/src/fs_safe.rs:122-131`
- **Problem:** 6 individual `db.execute_batch()` calls for 6 PRAGMAs = 6 SQLite VM round trips.
- **Fix:** Combine into single `execute_batch` with multi-line string (as `create_new_db` does at schema.rs:13-19).
- **Impact:** 6× reduction in PRAGMA round trips per connection.

### P6-34: safe_read issues two stat syscalls
- **File:** `crates/reliary-core/src/fs_safe.rs:46-59`
- **Problem:** `if !p.exists()` (stat #1) then `if let Ok(meta) = p.metadata()` (stat #2). First is redundant.
- **Fix:** Call `metadata()` once, match on `ErrorKind::NotFound`.
- **Impact:** Halves stat calls on every safe_read.

### P6-35: atomic_write never fsyncs despite doc comment
- **File:** `crates/reliary-core/src/fs_safe.rs:27-42`
- **Problem:** Doc says "write to temp file, fsync, then rename" but no `sync_all()`/`sync_data()` before rename. (Correctness/durability.)
- **Fix:** Add `file.sync_all().ok();` before `rename()`.
- **Impact:** Data durability guarantee.

### P6-36: Per-file hidden-directory check re-converts every path component
- **File:** `crates/reliary-search/src/ingest.rs:165`
- **Problem:** `path.components().any(|c| c.as_os_str().to_str()...)` allocates per component. Only skips the file, not the subtree — walkdir still descends into `.git`/`node_modules`.
- **Fix:** Use `WalkDir::filter_entry(|e| !is_hidden(e))` to prune whole hidden subtrees.
- **Impact:** Avoids descending into .git/node_modules during ingest.

### P6-37: files_needing_reindex does DB round trip per file during walk
- **File:** `crates/reliary-search/src/incremental.rs:69-101`
- **Problem:** Per-file `read_stored_mtime(db, &p_str)` SELECT interleaved with directory I/O. Module already has `load_stored_mtimes` (line 46) that bulk-loads.
- **Fix:** Call `load_stored_mtimes` once before walk, compare against in-memory set.
- **Impact:** Eliminates N SELECTs during incremental reindex.

---

## Tier 7 — MEDIUM: MCP & Main.rs

### P7-38: no_color() does 2× env::var lookups on every color call
- **File:** `crates/reliary-agent/src/main.rs:31-33`
- **Problem:** Every `color::green()` etc. calls `no_color()` which does 2 environment variable lookups (hash table scans). Functions calling color helpers 3-5× trigger duplicate env scans.
- **Fix:** `static NO_COLOR: OnceLock<bool>`.
- **Impact:** Eliminates repeated env scans.

### P7-39: db.prepare() inside loop in diagnose_failure
- **File:** `crates/reliary-agent/src/main.rs:1200-1222`
- **Problem:** `for file in locations.iter().take(2) { if let Ok(mut stmt) = db.prepare(...)` — compiles SQL every iteration. Text is identical, only params change.
- **Fix:** Hoist `db.prepare(...)` above loop, or use `prepare_cached`.
- **Impact:** Eliminates redundant SQL compilation.

### P7-40: current_dir() syscall inside walkdir loop
- **File:** `crates/reliary-agent/src/main.rs:1769`
- **Problem:** Inside `walkdir` loop over potentially thousands of files: `std::env::current_dir()` called per file. 5000 files = 5000 getcwd syscalls.
- **Fix:** `let cwd = std::env::current_dir().unwrap_or_default();` above loop.
- **Impact:** Eliminates thousands of syscalls.

### P7-41: Eager index blocks initialize response
- **File:** `crates/reliary-agent/src/mcp.rs:1163-1235`
- **Problem:** When `RELIARY_EAGER_INDEX` is set, occurrence table build + lazy table build runs synchronously inside `initialize` handler, before `respond()`. Client blocked during entire build. File_meta warming (line 1219) already correctly uses `thread::spawn`.
- **Fix:** Move eager-index work to `thread::spawn`, send `respond` immediately.
- **Impact:** Unblocks MCP client during initialization.

### P7-42: build_read_footer sorts entire vec just to find longest element
- **File:** `crates/reliary-agent/src/main.rs:1123-1124`
- **Problem:** `identifiers.sort_by_key(|s| -(s.len() as i64));` then takes `[0]`. O(n log n) sort for O(n) max.
- **Fix:** `identifiers.iter().max_by_key(|s| s.len()).unwrap()`
- **Impact:** Minor, but free fix.

### P7-43: reliary_dead makes three separate filter passes over filtered
- **File:** `crates/reliary-agent/src/mcp.rs:295-297`
- **Problem:** Three `.iter().filter().count()` calls — three full iterations.
- **Fix:** Single pass with match:
  ```rust
  let (mut high, mut medium, mut low) = (0, 0, 0);
  for c in &filtered { match c.confidence { ... } }
  ```
- **Impact:** 3× → 1× iteration.

### P7-44: Triple-duplicated walk-up-to-find-index logic
- **File:** `crates/reliary-agent/src/main.rs:519-548` (`run_reindex_file`), `:564-593` (`run_who_calls`), `:1065-1083` (`find_open_index`)
- **Problem:** `find_open_index` was extracted (V1 fix) but `run_reindex_file` and `run_who_calls` still have inline copies of the same directory-walk-up logic.
- **Fix:** Replace inline walks with calls to `find_open_index`.
- **Impact:** Code quality + ensures future optimizations apply everywhere.

### P7-45: Vec<String> one allocation per file line in MCP grep/json branches
- **File:** `crates/reliary-agent/src/mcp.rs:940-943,996-999`
- **Problem:** `s.lines().map(String::from).collect()` allocates N Strings for N-line file. Then `.cloned()` to read a single line. For 10K-line file with 5 hits, 10K allocations to access 5 lines.
- **Fix:** Store raw `String` in cache, use `s.lines().nth(n)` for small hit counts. Replace `.cloned()` with `.map(|s| s.as_str())`.
- **Impact:** Reduces allocations proportional to file size.

---

## Tier 8 — MEDIUM: Read Summary & Paths

### P8-46: Dead WAL PRAGMA instantly overwritten in read_summary
- **File:** `crates/reliary-agent/src/read_summary.rs:61-62` (and `:106`)
- **Problem:** Sets `PRAGMA journal_mode=WAL;` then `open_existing_db` immediately overrides with `journal_mode=MEMORY; synchronous=OFF`. Double mode-transition (MEMORY→WAL→MEMORY) on every call.
- **Fix:** Remove line 61 entirely. `open_existing_db` already sets correct PRAGMAs.
- **Impact:** Eliminates mode-transition I/O per call.

### P8-47: Fresh SQLite connection per build() call
- **File:** `crates/reliary-agent/src/read_summary.rs:60`
- **Problem:** Every `build()` opens a new connection (file open, header read, page cache cold-start, PRAGMA setup) and drops it at scope end.
- **Fix:** Cache in `OnceLock<Connection>` or thread-local keyed by db_path.
- **Impact:** Eliminates connection setup per read summary.

### P8-48: Duplicate find_workdir implementation
- **File:** `crates/reliary-agent/src/read_summary.rs:21-31`
- **Problem:** Near-duplicate of `paths::find_workdir` (paths.rs:29). Walks ancestors, allocates Strings, checks `.exists()` per ancestor.
- **Fix:** Delete this function, use `crate::paths::find_workdir(file)`.
- **Impact:** Code dedup + consistent behavior.

### P8-49: format!() allocation on every relativize() call
- **File:** `crates/reliary-agent/src/paths.rs:35`
- **Problem:** `file.strip_prefix(&format!("{}/", root))` allocates String just to borrow as prefix.
- **Fix:** `file.strip_prefix(root).unwrap_or(file).strip_prefix('/').map(|s| s.to_string()).unwrap_or_else(|| file.to_string())`
- **Impact:** Minor allocation elimination.

### P8-50: Loop-invariant string allocations in log filter
- **File:** `crates/reliary-agent/src/ux.rs:391-393`
- **Problem:** `let upper = format!(" [{}] ", lvl.to_uppercase());` and `let lower = format!("[{}]", lower_lvl);` allocated INSIDE `for line in content.lines()` — identical every iteration. 100K-line log = 200K allocations.
- **Fix:** Hoist before loop.
- **Impact:** Eliminates per-line allocations in log filtering.

---

## Tier 9 — MEDIUM: Pack Generation (additional)

### P9-51: generate_pack_hotspot clones Symbol structs instead of references
- **File:** `crates/reliary-pack/src/lib.rs:339-343`
- **Problem:** `.map(|(_, s)| s.clone())` clones up to 50 Symbols (4 String fields each = 200 heap allocations). Used only as `&selected` afterward.
- **Fix:** `Vec<&Symbol>` — keep references into original `symbols` vec.
- **Impact:** Eliminates 200 allocations per hotspot pack.

### P9-52: parse_pack_entries clones then immediately clears
- **File:** `crates/reliary-pack/src/lib.rs:531-538`
- **Problem:** `current_header.clone()` + `current_body.clone()` then `current_header.clear()` + `current_body.clear()`. Full copy then immediate emptying.
- **Fix:** `std::mem::take(&mut current_header)` / `std::mem::take(&mut current_body)` — zero-copy swap.
- **Impact:** Eliminates copy per pack entry.

### P9-53: detect_modules clones every symbol
- **File:** `crates/reliary-pack/src/lib.rs:2374-2376`
- **Problem:** Every symbol deep-cloned (4 String fields each) to group by module. Cloned symbols only read afterward.
- **Fix:** Store indices: `module_map.entry(module).or_default().push(idx)`. Iterate `&symbols[idx]` when building packs.
- **Impact:** Eliminates ~4000 heap allocations for 1000 symbols.

### P9-54: derive_module_name and derive_crate_name re-split paths per symbol
- **File:** `crates/reliary-pack/src/lib.rs:2358,2384`
- **Problem:** Called once per symbol. Same file paths re-split repeatedly. `derive_module_name` also does `.to_string_lossy().to_string()` per component.
- **Fix:** Cache module/crate name per file path: `HashMap<String, String>`.
- **Impact:** Eliminates redundant path splitting.

### P9-55: Three-pass line iteration in extract_surprise_from_body
- **File:** `crates/reliary-pack/src/lib.rs:1528,1667,1755`
- **Problem:** Passes 2 and 3 both iterate full line vector. Pass 3 re-runs `is_noise_line` + `aggressive_skeleton` on lines already classified. Called once per symbol during rendering.
- **Fix:** Merge passes 2+3 into single loop. Pre-filter noise lines once.
- **Impact:** 3× → 2× iteration per symbol render.

### P9-56: Dead timing variable t_xref
- **File:** `crates/reliary-pack/src/lib.rs:134`
- **Problem:** `let t_xref = std::time::Instant::now();` created but `.elapsed()` never called. Dead code.
- **Fix:** Remove the line, or add missing timing output.
- **Impact:** Code cleanliness.

---

## Tier 10 — LOW: Minor Inefficiencies

### P10-57: label_number allocates to_lowercase() per magic number
- **File:** `crates/reliary-pack/src/lib.rs:2014`
- **Problem:** `let lower = code.to_lowercase();` inside `for num in find_magic_numbers(code)` — same code lowercased once per number.
- **Fix:** Compute `lower` once per line in the caller (line 1667), pass in.

### P10-58: format!() + tokenize per entry in BM25
- **File:** `crates/reliary-pack/src/lib.rs:666-667`
- **Problem:** `format!("{} {}", entry.name, entry.body)` concatenates then tokenizes — large String allocation + tokenize. Per entry, per query.
- **Fix:** Tokenize name and body separately, concatenate token vectors.

### P10-59: is_common_word does to_lowercase() + linear scan of ~150 words
- **File:** `crates/reliary-pack/src/lib.rs:1248`
- **Problem:** `let lower = word.to_lowercase();` + `COMMON.contains(&lower.as_str())` linear scan. Called per candidate symbol name and per cross-ref phrase.
- **Fix:** `HashSet<&str>` initialized once (or phf::Set). Use `eq_ignore_ascii_case`.

### P10-60: should_slice_for_query allocates to_lowercase()
- **File:** `crates/reliary-pack/src/lib.rs:593`
- **Problem:** `let text = query.to_lowercase();` per query. Could use case-insensitive `contains`.
- **Fix:** Use `eq_ignore_ascii_case` or similar.

### P10-61: Global config file read & JSON-parsed multiple times
- **File:** `crates/reliary-agent/src/config.rs:113,186`
- **Problem:** `resolve_mode_with_source` reads+parses global config; `resolve_features_with_source` reads+parses it again. Within features resolver, parsed as `HashMap<String,String>` then re-parsed as `HashMap<String,bool>`.
- **Fix:** Single `load_config()` returning parsed struct.

### P10-62: Dead FTS5 DDL executed on every new-DB creation
- **File:** `crates/reliary-search/src/schema.rs:165`
- **Problem:** `DROP TABLE IF EXISTS phrases_fts;` — FTS5 was removed but this DDL still runs on every `create_new_db`.
- **Fix:** Delete the line + misleading FTS5 comments.

### P10-63: content_cache::retrieve is two round trips
- **File:** `crates/reliary-core/src/content_cache.rs:61-77`
- **Problem:** SELECT then UPDATE — two SQL round trips for one logical operation.
- **Fix:** `UPDATE ... SET accessed_at=? WHERE hash=? RETURNING original` (SQLite ≥ 3.35).

### P10-64: pi --version subprocess to detect Pi (avoidable)
- **File:** `crates/reliary-agent/src/init.rs:109`
- **Problem:** If `~/.local/bin/pi` doesn't exist, spawns subprocess for detection. `uninstall()` (line 410) already uses PATH-split + existence check.
- **Fix:** Reuse `pi_in_path` logic from `uninstall()`.

### P10-65: Gate default mismatch (correctness)
- **File:** `hooks/opencode-code-gate.js:29` (and `opencode-reliary-sift.js:26`)
- **Problem:** Headers say `Toggle: RELIARY_GATE=1 (default OFF)` but code defaults to ON (`!== "0"`).
- **Fix:** Either fix the code to match docs (`=== "1"`) or fix the docs to match code.

---

## Summary by Impact

| Tier | Bugs | Area | Frequency |
|------|------|------|-----------|
| 0 | 5 | Reindex hot path | Per file edit |
| 1 | 5 | MCP server | Per LLM tool call |
| 2 | 7 | Hooks/plugin | Per tool invocation |
| 3 | 6 | Pack generation | Per pack gen/query |
| 4 | 5 | Search path | Per search query |
| 5 | 4 | Tokenization | Per token per file |
| 6 | 5 | File I/O | Per file walk |
| 7 | 8 | MCP & main | Per call |
| 8 | 5 | Read summary/paths | Per read summary |
| 9 | 6 | Pack gen (additional) | Per pack gen |
| 10 | 9 | Minor | Various |
| **Total** | **65** | | |

## Top 10 Highest-ROI Fixes

1. **P0-1 + P0-2**: Double tokenization + 4× disk re-reads (per edit)
2. **P2-11**: execFileSync blocking event loop (per edit)
3. **P3-18**: O(S×F×P) cross-ref building (per pack gen)
4. **P1-6**: DB connection per MCP call (per tool call)
5. **P0-3**: 5 auto-commit DELETEs (per edit)
6. **P3-19**: Per-symbol file re-splitting (per pack gen)
7. **P4-25/26**: N+1 + linear scan in search (per query)
8. **P4-28**: COUNT(*)...LIMIT 1 guards (per reindex)
9. **P2-13**: find /tmp scan (per tool call)
10. **P5-29/30**: Tokenizer allocation reduction (per token)

## Execution Order

1. **Tier 0** (P0-1 to P0-5): Reindex hot path — most impactful, smallest blast radius
2. **Tier 2** (P2-11 to P2-17): Hooks/plugin — highest UX impact
3. **Tier 1** (P1-6 to P1-10): MCP server — most frequent calls
4. **Tier 3** (P3-18 to P3-23): Pack generation — largest algorithmic wins
5. **Tier 4** (P4-24 to P4-28): Search path
6. **Tiers 5-10**: Remaining fixes in priority order

After each tier: `cargo check` + `cargo test` for affected crate(s).
After all tiers: full workspace `cargo build && cargo test`.
