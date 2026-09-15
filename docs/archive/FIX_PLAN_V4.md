# FIX PLAN V4 — Deeper Audit Fixes + Dead Code Wiring

From 5 explore agents (error handling, SQL injection, dead code, MCP correctness,
path traversal). Deduplicated and revised after reading implementations.

---

## Phase 1: Bug Fixes — Path Traversal (6 items, CRITICAL)

Tools accept user paths but bypass `safe_path()`. All handler files: `mcp.rs`.

### PT-1: reliary_pack — missing safe_path
- **File:** mcp.rs:454
- **Bug:** `args.get("path").and_then(|v| v.as_str()).unwrap_or(".")` — raw, unscoped.
- **Fix:** `let sp = safe_path(path, ".")?;` then use `sp` for `generate_pack` call.

### PT-2: reliary_pack_query — missing safe_path
- **File:** mcp.rs:473
- **Bug:** `path` unscoped → `std::fs::read_to_string(pack_md_path)` and
  `std::fs::write(pack_md_path, &pack)` and `generate_pack(path)` all outside workdir.
- **Fix:** `let sp = safe_path(path, ".")?;` then `pack_md_path = sp.join(".reliary/pack_l2l3.md")`.

### PT-3: reliary_brace_graph / reliary_call_graph / reliary_brace_debug — missing safe_path
- **File:** mcp.rs:1055, 1088, 1110
- **Bug:** `file_path` unscoped → `get_brace_graph(file_path)` reads arbitrary files.
- **Fix:** In `handle_symbol_tool`, validate `file_path` with `safe_path(file_path, &dir)`.
  `dir` is available from `open_symbol_index()` → returns `(db, workdir)`.

### PT-4: reliary_find_references_with_source — anchor_file bypasses safe_path
- **File:** mcp.rs:864-869
- **Bug:** `resolve_af()` preserves absolute paths → `File::open(&abs)` on any file.
- **Fix:** In the grep format branch (lines 929-972), pass the resolved `af` through
  `safe_path()` against `workdir` from `open_symbol_index()`.

### PT-5: reliary_query_ast — missing safe_path
- **File:** mcp.rs:588-601
- **Bug:** `file` arg passed raw to `query_file()` → `std::fs::read_to_string` on any file.
  Bypasses `handle_symbol_tool` entirely (dispatched at line 386).
- **Fix:** `let fp = safe_path(file, ".")?;` at line 589, use `fp.to_string_lossy()`.

### PT-6: CLI commands — missing safe_path
- **File:** main.rs:1755 (Dead), main.rs:517 (ReindexFile)
- **Bug:** CLI subcommands accept user path args without `safe_path()`.
- **Fix:** Add `safe_path()` normalization. Lower priority (CLI runs as user).

---

## Phase 2: Bug Fixes — Error Handling / Crash Prevention (4 items)

### ER-1: watcher.rs:137 — poisoned mutex crashes watcher
- **File:** watcher.rs:137
- **Bug:** `debounce.lock().unwrap()` panics if any `handle_event()` poisons the mutex.
  All subsequent file-change events crash the thread → silent disabling of reindex.
- **Fix:** `debounce.lock().unwrap_or_else(|poisoned| poisoned.into_inner())`

### ER-2: mcp.rs respond/respond_error — silent write failures
- **File:** mcp.rs:45-46, 57-58
- **Bug:** `let _ = serde_json::to_writer(...)` and `let _ = writeln!(out)` absorb failures.
  Client receives truncated/garbage with no diagnostic.
- **Fix:** Check results; `eprintln!` on failure. If stdout is unrecoverable, the
  caller (`handle_tool_call_stdio`) should break the serve loop.

### ER-3: reindex.rs:43 — BEGIN failure silently skipped
- **File:** reindex.rs:43-45
- **Bug:** `if let Err(e) = db.execute_batch("BEGIN;") { eprintln!(...); }` — continues
  without transaction. Subsequent DELETEs auto-commit individually, risking partial index.
- **Fix:** Return `false` immediately on BEGIN failure so reindex is retried on next event.

### ER-4: reindex.rs — DELETE failures masked (6 sites)
- **File:** reindex.rs:47-51, 83, 86
- **Bug:** `let _ = db.execute(DELETE ...)` suppresses errors. Failed DELETE leaves stale
  rows → phantom data for the reindexed file.
- **Fix:** Check results; return `false` on any DELETE failure.

---

## Phase 3: Bug Fixes — MCP Tool Correctness (6 items)

### TB-1: reliary_dead — double +1 → 2-based line numbers
- **File:** mcp.rs:312
- **Bug:** `reliary-dead/src/lib.rs:99` already converts 0→1 (`line: dl + 1`).
  `mcp.rs:312` adds another `+1` (`"line": c.line + 1`) → 2-based output.
- **Fix:** `"line": c.line` (remove the `+ 1`).

### TB-2: reliary_find_references_with_source — limit default not propagated
- **File:** mcp.rs:834
- **Bug:** Schema default `limit=50`. Line 834: `None` stored when omitted.
  Output truncation (line 916-918) falls back to `min(30)`.
- **Fix:** `let limit = args.get("limit").and_then(|v| v.as_i64()).map(|v| v as usize).or(Some(50));`

### TB-3: reliary_trace_path — direction default mismatch
- **File:** mcp.rs:434
- **Bug:** Schema says `"default": "inbound"` (line 90). Handler says `unwrap_or("both")`.
- **Fix:** `unwrap_or("inbound")`.

### TB-4: handle_tool_call_stdio — silent fallback on invalid arguments
- **File:** mcp.rs:1137
- **Bug:** If `arguments` is string/number/array, `as_object()` returns None →
  silently uses empty map. Client protocol bugs hidden.
- **Fix:** Log warning or return JSON-RPC error when `arguments` exists but isn't an object.

### TB-5: reliary_find_references_with_source — confidence cap overrides user limit
- **File:** mcp.rs:901-915
- **Bug:** User `limit=200` silently capped to 10/20/30 based on avg similarity.
  Undocumented behavior.
- **Fix:** Use `max(user_limit, cap)` so user intent isn't overridden.

### TB-6: LIKE wildcard leakage in ORDER BY (4 sites)
- **File:** type_flow.rs:730, 825; callgraph_v2.rs:203, 280
- **Bug:** `%`/`_` in bound param values act as LIKE wildcards in ORDER BY
  tiebreakers. Can skew ranking. Not exploitable for injection.
- **Fix:** Escape `%`→`\%`, `_`→`\_` in param values before binding, matching
  existing pattern in `trace_path.rs:59`.

---

## Phase 4: Pure Waste Deletions (4 items)

Confirmed zero-read callers. No value to preserve.

### DL-1: `lines_owned: Vec<String>` field — ingest.rs:78
- **Evidence:** 2 writes (lines 257, 308), 0 reads. O(N) heap allocations per file.
- **Fix:** Remove field + 2 initialization sites. Check struct construction for
  remaining field references.

### DL-2: `OccRow` struct — ingest.rs:43
- **Evidence:** Defined at line 43, mentioned in comment at line 67. Never instantiated.
- **Fix:** Delete struct definition.

### DL-3: `#[allow(dead_code)]` on `mod color` — main.rs:29
- **Evidence:** All 7 color functions actively used in production.
- **Fix:** Remove annotation. Reveals if any function truly becomes dead later.

### DL-4: Stale refactoring artifacts
- **Files:** `type_flow.rs.arc24-failed`, `type_flow.rs.bad`
- **Evidence:** Not in module tree, not compiled.
- **Fix:** `rm` both files.

---

## Phase 5: Dead Code — Wire Up (6 items)

These functions have genuine, well-thought-out value. Built, tested, never wired.

---

### WR-1: generate_pack_auto — auto-format pack generation

**What it does:**
`should_inject_pack(path)` computes a composite complexity score (symbol count,
block span, cross-ref density, identifier specificity — calibrated on real data:
reliary8=6.4→Full, quale=5.4→Minimal, tokio=5.4→Minimal). `generate_pack_auto`
then maps score → Skip/Minimal(15)/Full(50) and calls `generate_pack_hotspot`.

**Value:** Removes format-selection from the LLM (which it often gets wrong).
Data-driven, benchmark-backed, no API calls, no token cost.

**Wiring:**
1. `mcp.rs:453-467` — Add `"auto"` to the `format` enum in schema (line 459).
2. In the handler: `if format_str == "auto" { generate_pack_auto(path) } else { ... }`.
3. Change schema default `"format"` from `"l2l3"` to `"auto"`.

**Effort:** ~10 lines in mcp.rs + 1 schema change. No new imports (same crate).

---

### WR-2: generate_hierarchical_pack — module-grouped packs

**What it does:**
Groups symbols by module (`detect_modules`), produces `HierarchicalPack {
top_level: String, module_packs: Vec<(String, Vec<String>)> }`. Top-level map
shows key symbols per module. Per-module packs show detail limited to
`max_symbols_per_module`.

**Value:** Structural solution to context window overflow for 500+ symbol repos.
LLM can navigate hierarchically — read overview first, zoom into specific modules.

**Wiring:**
1. `mcp.rs:453-467` — Add `"hierarchical"` to the `format` enum.
2. In the handler: `if format_str == "hierarchical" { generate_hierarchical_pack(path, PackFormat::L2L3, 50) }`.
3. Format the `HierarchicalPack` output for LLM consumption:
   - Return `top_level` as primary content.
   - Include `module_packs` as a `modules` field in the JSON response
     (LLM can request a specific module in a follow-up call if needed).

**Effort:** ~15 lines in mcp.rs + schema change.

---

### WR-3: should_slice_for_query + adaptive_slice — conditional pack slicing

**What it does:**
`should_slice_for_query(query)` classifies queries: SLICE for bugs, cross-references,
detail questions, specific symbol names. SKIP for architecture, impact, review,
discriminate/edge questions. Based on 63-probe benchmark with measured deltas
(bug=+1.8, crossref=+1.0, arch=+0.0). `adaptive_slice` wraps this to conditionally
call `slice_pack_for_query`. The full classification logic is at lines 589-636.

**Value:** Saves tokens on queries where the model already knows from training.
Data-driven, 11 tests validate classification. No API calls, <1ms runtime.

**Wiring:**
1. `mcp.rs:468-537` (reliary_pack_query handler) — modify to use `adaptive_slice`
   instead of `slice_pack_for_query` directly.
2. Add `adaptive: bool` parameter to schema (default `true`).
3. When `adaptive=true`: `let (sliced, decision) = adaptive_slice(&pack_content, &context, 5);`
   Include `"decision": format!("{:?}", decision)` in output.
4. When `adaptive=false`: use existing `slice_pack_for_query` path.
5. Also wire into the `generate_pack → query` fallback path (lines 489-496)
   where it auto-generates pack then queries.

**Effort:** ~20 lines in mcp.rs + schema change. Same crate — `reliary_pack::adaptive_slice`.

---

### WR-4: compute_blast_radius — actionable risk data

**What it does:**
Scans file content for `pub fn`, `pub struct`, `pub enum`, `pub trait`,
`pub type`, `pub const`, `pub static`, `export` → extracts identifier names.
Small, fast function (<30 lines). Already has a test at line 158.

**Value:** Current `reliary_risk` output is `{risk: "Medium", reason: "25 pub exports"}`.
User doesn't know WHICH exports. This adds `blast_radius: ["process", "Config"]`.

**Wiring:**
1. `mcp.rs:211-233` (reliary_risk handler) — after `compute_file_risk`, also call
   `compute_blast_radius(&content)`.
2. Add `"blast_radius"` field to the JSON output.

**Effort:** ~3 lines in mcp.rs. Same crate — `reliary_risk::compute_blast_radius`.

---

### WR-5: content_aware_match — natural-language fix extraction

**What it does:**
Parses text for `'old_str' → 'new_str'` or `old_str -> new_str` patterns using
regex. Checks if `file_content.contains(&old_str)`. Returns `Vec<(String, String)>`
feedable to `apply_fixes`. Has a test at line 218.

**Value:** Bridges gap between "LLM described a change in conversation" and
"code needs a literal replacement." Current `reliary_fix` requires structured
`old`/`new` pairs.

**Wiring:**
1. Add optional `context` parameter to `reliary_fix` schema (the conversation text
   describing the change, default `""`).
2. In the handler (mcp.rs:234-267): if `old.is_empty() && new.is_empty() && !context.is_empty()`,
   call `content_aware_match(context, &content)` to extract fix pairs.
3. Feed extracted pairs to `apply_fixes` as normal.

**Effort:** ~10 lines in mcp.rs + schema change.

---

### WR-6: reliary-memory — HDC vector memory (DEFER wiring, KEEP code)

**What it does:**
Full hyperdimensional computing memory: 10K-bit hypervectors, Hebbian co-occurrence
updates, cosine-similarity recall, co-occurrence prediction, memory tiering
(episodic→semantic→consolidated), SQLite persistence. 292 lines, 5 tests.

**Value:** Replaces or augments `reliary_prior` with vector-space memory.
Currently `reliary_prior` reads a flat text file (line 344: `read_to_string(prior_block)`).
This crate would enable semantic recall instead of exact text match.

**Decision: KEEP but don't wire in this pass.** Wiring requires:
- New MCP tools (`reliary_remember`, `reliary_recall`)
- Session-level MemoryStore state (static/global, or per-session file)
- Integration with `reliary_prior` or a new backend
- Schema migration for the `cortex_memories` table

This is a significant feature, not a bug fix. It should be planned separately.

**Action:** Remove `reliary-memory` from `reliary-agent/Cargo.toml` dependencies
(to stop CI from compiling dead code). Keep the crate in the workspace and git
(if someone wants it, restore the dependency). OR: leave everything as-is for now
and defer to a separate memory feature PR.

---

## Phase 6: Verification

- `cargo check --workspace` — 0 errors
- `cargo test -p reliary-agent -p reliary-search -p reliary-core -p reliary-dead`
  — all pass (except pre-existing `check_test_fn_appears`)
- `bash -n hooks/*.sh` — clean
- `node --check hooks/*.js` — clean

---

## Summary

| Phase | Items | Type |
|-------|-------|------|
| 1: Path traversal | 6 | CRITICAL bug fixes |
| 2: Error handling | 4 | CRITICAL-HIGH bug fixes |
| 3: MCP tool bugs | 6 | MEDIUM-LOW bug fixes |
| 4: Pure waste | 4 | Deletions |
| 5: Wire up | 5 (+1 deferred) | Feature completion |
| 6: Verify | — | `cargo check` + `cargo test` |

**Files modified:** `mcp.rs` (heavy), `main.rs`, `watcher.rs`, `reindex.rs`,
`ingest.rs`, `type_flow.rs`, `callgraph_v2.rs`, `Cargo.toml` (de-depend reliary-memory).

**27 fixes + 5 wiring changes + 4 deletions = 36 items.**

**Items NOT wired in this pass:**
- `reliary-memory` — requires new MCP tools, session state, separate PR.
- `generate_hierarchical_pack` — auto and hierarchical wiring is sufficient for now;
  hierarchical requires UI design for module navigation.
