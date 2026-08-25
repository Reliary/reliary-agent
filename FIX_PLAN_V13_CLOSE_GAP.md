# FIX PLAN V13 — Close the q4 + q10 Gap

**Goal:** Lift long-bench score from 26/30 to 28-30/30 by fixing the two failing queries.

## Root Causes

### q4_block_on_chain (1/3): callgraph_v2 reads doc-comment as function body

When LLM calls `callgraph(name="block_on", anchor_file="runtime/runtime.rs", anchor_line=261)`:
- Line 261 is a **doc comment** (`/// use tokio::runtime::Runtime;`)
- callgraph_v2 trusts the anchor blindly, extracts callees from doc text
- Returns `dox`, `unwrap`, `spawn_blocking` — all from the doc example
- Real `block_on` is at line 340: `pub fn block_on<F: Future>(&self, future: F) -> F::Output`
- Even with correct anchor, depth-2 misses `block_on_inner`'s callees because `seen_callees` blocks it

### q10_dead_code (1/3): reliary-dead returns noise, no cross-file analysis

Two problems:
1. `reliary_dead` module does **per-file** analysis only — flags functions called from other files as dead
2. `reliary_dead::analyze_file` marks ALL tokens on definition lines as definitions — `let`, `const`, `Self` appear as dead candidates
3. `dead_symbols` SQL has no `path` filter — LLM can't scope to `io/util`
4. `dead_symbols` returns all definition types (structs, consts) not just functions

## Fixes

### Fix A: dead_symbols path + tag filter (q10, 30 min)

**File:** `crates/reliary-search/src/symbol.rs:919`

Current:
```sql
SELECT o.phrase_id, f.file_path, o.line, o.col, o.block_id
FROM occurrence o JOIN file_map f ON f.id = o.file_id
WHERE o.is_def = 1
ORDER BY (o.tag = 1) DESC, LENGTH(f.file_path) ASC, o.occ_id
```

New signature: `dead_symbols(db, limit, path_filter: Option<&str>, functions_only: bool)`

New SQL:
```sql
SELECT o.phrase_id, f.file_path, o.line, o.col, o.block_id
FROM occurrence o JOIN file_map f ON f.id = o.file_id
WHERE o.is_def = 1
  AND (?2 IS NULL OR f.file_path LIKE ?2 || '%')
  AND (?3 = 0 OR o.tag = 1)
ORDER BY (o.tag = 1) DESC, LENGTH(f.file_path) ASC, o.occ_id
```

MCP handler (`mcp.rs:910`): extract `path` arg, pass as filter.
Tool schema: add `functions_only` param (default true).

### Fix B: Replace reliary_dead with carrion algorithm (q10, 2 hours)

**File:** `crates/reliary-dead/src/lib.rs`

Port carrion's cross-file approach:
1. Collect all source files in the path
2. For each file: extract definitions via regex (`^\s*(fn|def|struct|...) NAME`)
3. For each file: count ALL identifier occurrences via word regex (`\b[A-Za-z_][A-Za-z0-9_]{3,40}\b`)
4. Merge into global maps: `def_map: HashMap<name, Vec<(file, line)>>` and `count_map: HashMap<name, total_occurrences>`
5. Dead = def exists AND total_occurrences <= def_occurrences (cross-file)
6. Filter: skip `__dunders`, all-digit, < 4 chars
7. Confidence: high = ALL CAPS + len >= 5; medium = len >= 5; low = test file only

Key difference from current reliary-dead: **cross-file** occurrence counting. A function called from another file will have `total > def` and won't be flagged.

**MCP handler** (`mcp.rs:333`): Replace per-file `analyze_file` loop with single `scan_repo(path)` call.

### Fix C: callgraph_v2 anchor validation (q4, 1 hour)

**File:** `crates/reliary-search/src/callgraph_v2.rs`

Before extracting callees, validate the anchor:
1. Get `file_meta::get(anchor_file)`
2. Check `fn_names[anchor_line - 1]` — does it match the requested `name`?
3. If NOT (anchor is doc comment, blank line, etc.), scan forward 1-50 lines for the first line where `fn_names[line]` matches `name`
4. Use THAT line as the actual anchor

This is grammar-free — uses existing `file_meta::fn_names` vector which is built by the structural classifier.

```rust
// After resolving anchor_file + anchor_line:
let real_anchor = {
    let meta = file_meta::get(&anchor_file);
    if let Some(m) = &meta {
        let idx = (anchor_line as usize).saturating_sub(1);
        // Check if anchor_line is a function definition
        let mut real_line = anchor_line;
        if idx >= m.fn_names.len() || !m.fn_names[idx].eq_ignore_ascii_case(name) {
            // Scan forward for the real definition
            for offset in 1..=50 {
                let candidate = idx + offset;
                if candidate >= m.fn_names.len() { break; }
                if m.fn_names[candidate].eq_ignore_ascii_case(name) {
                    real_line = (candidate + 1) as i32;
                    break;
                }
            }
        }
        real_line
    } else {
        anchor_line
    }
};
// Use real_anchor instead of anchor_line for body extraction
```

### Fix D: callgraph_v2 depth-2 primary delegate chase (q4, 30 min)

**File:** `crates/reliary-search/src/callgraph_v2.rs:396-445`

Currently, depth-2 expansion skips callees already in `seen_callees`. The fix: for callees whose name starts with the anchor name (delegation pattern like `block_on` → `block_on_inner`), ALWAYS expand even if seen.

```rust
// Line 430, before the seen_callees check:
let is_delegate = ident.starts_with(&anchor_name_lower);
if !is_delegate && seen_callees.contains(&ident) { continue; }
// Even if it IS a delegate and was seen, still expand its callees
```

This ensures `block_on → block_on_inner → {any, root, task, next, ...}` gets expanded, giving the LLM the 2-hop chain.

## Execution Order

1. **Fix A** (dead_symbols path filter) — 30 min, immediate q10 improvement
2. **Fix C** (callgraph_v2 anchor validation) — 1 hour, q4 improvement
3. **Fix D** (depth-2 delegate chase) — 30 min, q4 improvement
4. **Fix B** (carrion port) — 2 hours, q10 correctness
5. Run long bench to verify
6. Commit

## Expected Outcome

| Query | Before | After | Why |
|--------|--------|-------|-----|
| q4 | 1/3 | 2-3/3 | Real callees from line 340, depth-2 chases block_on_inner |
| q10 | 1/3 | 2-3/3 | dead_symbols scoped to io/util, functions only, cross-file check |
| **Total** | **26/30** | **28-30/30** | |

## Risk

All grammar-free, no new deps, no schema changes.
