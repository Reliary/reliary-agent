# V51: Audit for resurgent bug classes

## Goal

Find and fix bugs in the same classes we squashed in V25-V40 that may have
resurfaced in code added since. These are silent data corruption bugs that
affect every repo but don't trigger error messages.

## Context: Bug classes we squashed before

### Class 1: Silent error swallowing (V25)
- Pattern: `.unwrap_or(0)` / `.unwrap_or_default()` / `.ok()` on Result types
- Example fixed: `flush_occurrence_batch` discarding DB INSERT errors
- Fix: propagate errors with `?` or explicit error returns

### Class 2: Off-by-one line indexing (V26, V40)
- Pattern: `meta.lines.get(X as usize)` where X is 1-indexed from MCP params
  but the array is 0-indexed
- Example fixed: `mcp.rs:435` uses `m.line.saturating_sub(1)`
- Fix: convert 1-indexed to 0-indexed at MCP boundary

### Class 3: Empty-string-to-default mappings (V40)
- Pattern: `resolve_af("")` returning CWD instead of None
- Example fixed: find_references handler
- Fix: return Option<PathBuf>, None for empty input

### Class 4: Batch INSERT parameter reuse (V25)
- Pattern: `?N` numbered params with `params_from_iter` (silent data corruption)
- Example fixed: `flush_occurrence_batch` changed to `?` anonymous params
- Fix: use `?` not `?N` with rusqlite

## Audit scope

Three classes to audit, all matching previously-squashed patterns:

### Phase 1: Silent error audit (1 hour)

**Goal:** Find Result types being silently converted to default values.

**Sites to check:**
- `db.execute(...).unwrap_or(0)` → likely silent DB error
- `db.query_row(...).unwrap_or(0)` → likely silent DB error
- `db.query_map(...).ok()` → silently drops rows
- `db.prepare_cached(...).ok()` → silently fails to prepare
- `db.execute_batch(...).ok()` → silently drops batch errors
- `fs::read_to_string(...).unwrap_or_default()` → silently drops file read errors

**Expected findings:** 10-20 sites with potential silent data corruption.

**Audit method:**
1. Grep for `db.*().unwrap_or(0)` and `db.*().ok()`
2. Check if the wrapper around it propagates Result properly
3. For each bad site: fix to propagate error with `?` or return Result

**Phase output:** List of files:lines with silent error patterns, prioritized by
how often the code path runs (hot path = higher priority).

### Phase 2: Off-by-one line indexing audit (1 hour)

**Goal:** Find places where 1-indexed line numbers are used to index 0-indexed arrays.

**Sites to check (from the grep):**
- `qualified.rs:164` - `meta.fn_names.get(line as usize)` - caller passes `line` from MCP params (1-indexed)
- `qualified.rs:169` - `meta.lines.get(line as usize).and_then(|l| fn_name_from_line(l))`
- `symbol.rs:1155` - `anchor_lines.get(anchor_line as usize)` - `anchor_line` from MCP params (1-indexed)
- `symbol.rs:1194` - `cand_lines.get(line as usize)` - `line` from occurrence table (0-indexed, OK)
- `mcp.rs:435` - `m.line.saturating_sub(1)` - already correct (fixed in V26)
- `mcp.rs:1067` - `meta.lines.get(*line as usize)` - `line` from caller context
- `mcp.rs:1161,1212,1260,1293` - `meta.lines.get(h.line as usize)` - `h.line` from occurrence (0-indexed, OK)
- `mcp.rs:1732` - `meta.lines.get(h.line as usize)` - same
- `pack.rs:1282` - `line.max(0) as usize` - likely correct

**Expected findings:** 3-5 sites where 1-indexed input indexes 0-indexed array.

**Audit method:**
1. For each `lines.get(X as usize)` site, trace X to its source
2. If X comes from MCP params or 1-indexed user input, apply `.saturating_sub(1)`
3. If X comes from occurrence table (always 0-indexed), leave alone

**Phase output:** List of sites needing `.saturating_sub(1)` conversion.

### Phase 3: Empty-string-to-default audit (30 min)

**Goal:** Find MCP param handlers that convert empty strings to default values incorrectly.

**Pattern:** Handler takes `args.get("X").and_then(|v| v.as_str()).unwrap_or("")`
and uses the empty string as a CWD or path, when it should be None.

**Sites to check:**
- `path_filter` handler
- `path` handler (workdir)
- `anchor_file` handler (already fixed in V40 for find_references)
- `anchor_line` handler

**Expected findings:** 0-2 sites with the same bug as V40's `resolve_af("")`.

**Audit method:**
1. Grep for `unwrap_or("")` on MCP param values
2. Check if the empty string is passed to a path/lookup function
3. If so, convert empty string to None or skip the operation

**Phase output:** List of sites needing Option conversion.

## Fix execution (1-2 hours)

Apply fixes per phase:

1. **Silent errors:** Replace `.unwrap_or(0)` with `?` or explicit error returns.
   For DB queries that should always succeed, use `.expect("context")` instead
   of `.unwrap_or(0)`.

2. **Off-by-one:** Add `.saturating_sub(1)` when caller passes 1-indexed line
   numbers to functions that expect 0-indexed.

3. **Empty-string defaults:** Change handler logic to skip operations when the
   param is empty, or use `Option<PathBuf>` throughout.

## Verification (30 min)

**Phase 5: Run 4-seed reliary self-benchmark.**

Expected: V50's 25.0 ± 0.0 score should hold or improve. If a bug was silently
corrupting 1-2 query results, the fix should push the score to 26-27.

Watch for:
- Any test failures (especially in `qualified.rs`, `symbol.rs`, MCP handler)
- Regressions in other benchmarks (tokio corpus)
- Wall time changes (should not regress significantly)

## Risks

| Phase | Risk of regression |
|-------|---------------------|
| 1. Silent errors | Low - these bugs are subtle, fixing them usually doesn't change visible behavior |
| 2. Off-by-one | Medium - if the off-by-one was "lucky" (file happened to have content at both indices), fixing it might break what currently works |
| 3. Empty-string defaults | Low |
| 4. Fix | Medium |
| 5. Bench | - |

## Grammar-free verification

All fixes are pure code analysis. No new tool features, no AST, no language
detection. We're fixing existing bugs to make the tool MORE correct, not
adding capabilities.

## Is this fitting?

| Change | Fitting? | Why |
|--------|---------|-----|
| Fix silent errors | No | Any user benefits from correct error handling |
| Fix off-by-one | No | Lines that match the actual content is universally correct |
| Fix empty-string defaults | No | General input validation |

None are tokio-specific, Python-specific, or benchmark-specific. All improve
the tool for any user on any repo.

## Effort summary

| Phase | Time |
|-------|------|
| 1. Silent error audit | 1h |
| 2. Off-by-one audit | 1h |
| 3. Empty-string audit | 30min |
| 4. Fix | 1-2h |
| 5. Verify | 30min |
| **Total** | **~5 hours** |

## Execution order

1. Run audit scripts (Phase 1, 2, 3)
2. Compile list of bugs per phase
3. Fix bugs in order of hot-path frequency
4. Run tests after each fix
5. Run full bench at the end
6. Commit if bench improves or stays stable