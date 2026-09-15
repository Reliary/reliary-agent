# FIX PLAN V23 — Port Stria Features for Accuracy

## Goal
Beat codebase-memory-mcp (altbackend) and grep on LLM judge accuracy by porting 3 proven features from stria that help weaker models find the right code.

## Background

The LLM judge (deepseek-v4-pro) scored reliary at 7/30 while grep scored 15/30 and altbackend scored 11.5/30. The gap is NOT in tool capability (our tools return correct data) but in RANKING and GUIDANCE — the model gets too many results and picks the wrong ones.

Stria solved this with 3 features that are pure math on the existing phrase index (grammar-free):
1. **Quale reranking** — definition-weighted BM25 (is_def×5 boost)
2. **Hologram plan** — task→file mapping (edit/verify/read_first/coupled/risk)
3. **Proximity bonus** — multi-term co-occurrence within N lines

All 3 are proven in stria's testing with weaker models. All 3 are grammar-free. All 3 use the existing SQLite index — no new schema, no new indexing, no language-specific code.

---

## Phase 1: Quale Reranking (2 hours)

### What it does
Re-ranks BM25 search candidates by whether a file DEFINES the query terms vs merely using them. A file defining `block_on` (is_def=1) gets ×5 score boost over a file that just mentions it (is_def=0).

### Why it matters
- **q3 (block_on def):** `task/local.rs` has 20 block_on mentions in tests (is_def=0) and ranks above `runtime/runtime.rs` which defines it (is_def=1). Quale reranking fixes this — the definition file gets ×5.
- **q5 (spawn callers):** Test files mentioning `thread::spawn` (is_def=0) rank above `Handle::spawn` definition (is_def=1). Quale reranking fixes this.
- **q1 (consume impls):** Files importing `consume` (is_def=0) rank alongside files defining `consume` (is_def=1). Quale reranking separates them.

### Implementation

**File:** `crates/reliary-search/src/search.rs`

Port stria's `quale_rerank()` function:
1. After BM25 scoring produces candidates `Vec<(i64, f64)>` (file_id, score)
2. For each query term, query `phrase_occ` to find which candidate files have `is_def=1` for that term
3. Compute `def_count = Σ (1 + count).ln() * idf_norm` per file — IDF-weighted definition count
4. Final score = `def_count + bm25_normalized * 1e-6` (quale is primary, BM25 is micro-tiebreaker)

**Integration point:** After `search.rs` line 177 (the `select_nth_unstable_by` sort), before building results. The quale reranking re-sorts the top-N candidates.

**Key SQL** (from stria, adapted for reliary8's schema):
```sql
-- Find files with is_def=1 for query terms
SELECT po.file_id, po.flags
FROM phrase_occ po
JOIN phrases p ON p.id = po.phrase_id
WHERE po.file_id IN (...) AND p.phrase LIKE ?1
```

Then unpack `flags` to check `is_def` bit.

**Tests:**
- `test_quale_rerank_prefers_definition_files` — file with is_def=1 ranks above file with is_def=0 at same BM25
- `test_quale_rerank_preserves_order_when_no_defs` — no is_def hits → BM25 order preserved
- `test_quale_rerank_weights_rare_terms_more` — rare term (high IDF) definition gets bigger boost

### Files to change
- `crates/reliary-search/src/search.rs` — add `quale_rerank()` function, call it after BM25 sort
- `crates/reliary-search/src/search.rs` — add tests

### Expected impact
| Query | Current judge | After quale reranking |
|-------|--------------|---------------------|
| q3 | 0 | **3** (runtime.rs > test files) |
| q5 | 0 | **2** (Handle::spawn > thread::spawn) |
| q1 | 2 | **3** (io/util defs > imports) |
| q9 | 0 | **1** (io/util defs rank higher) |

---

## Phase 2: Hologram Plan (3 hours)

### What it does
Given a task/question description, returns a structured plan:
- `edit`: most likely file to look at (BM25 + quale reranking + is_def×5)
- `verify`: test files related to the edit file
- `read_first`: top 3 files to read before answering
- `coupled`: 4 files coupled to the edit file (vocabulary overlap)
- `risk`: low/moderate/high

### Why it matters
The model currently has to search, read, and guess which file contains the answer. A hologram plan directly tells it "for this question, look at runtime/runtime.rs first, and these 3 other files are relevant." This eliminates the exploration phase.

- **q3 (block_on def):** Plan says `edit: runtime/runtime.rs` → model goes directly to the right file
- **q8 (bufwriter write):** Plan says `edit: io/util/buf_writer.rs` → model finds poll_write
- **q10 (dead code):** Plan says `read_first: io/util/empty.rs, io/util/take.rs` → model finds dead functions

### Implementation

**File:** `crates/reliary-search/src/pack.rs` (or new `plan.rs` module)

Port stria's `hologram_plan()` function:
1. Extract phrases from the task description using `zone::extract_phrases()`
2. For each phrase, BM25 search across the index
3. Apply is_def×5 multiplier (quale-style)
4. Top result = `edit` file
5. Filter results for test files → `verify`
6. Top 3 non-test results → `read_first`
7. Skip 1, take 4 → `coupled`
8. Risk based on score magnitude

**MCP tool:** Add `reliary_plan` to `mcp.rs`:
```json
{
  "name": "reliary_plan",
  "description": "Given a task/question, returns the most likely file to look at, related test files, coupled files, and risk level. Call this FIRST before searching.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "task": {"type": "string", "description": "The task or question description"},
      "path": {"type": "string"}
    },
    "required": ["task"]
  }
}
```

**Output format:**
```json
{
  "edit": "runtime/runtime.rs",
  "verify": ["runtime/tests/task_combinations.rs"],
  "read_first": ["runtime/runtime.rs", "runtime/scheduler/current_thread/mod.rs", "runtime/context/runtime.rs"],
  "coupled": ["runtime/scheduler/current_thread/mod.rs", "runtime/scheduler/multi_thread/mod.rs", "runtime/context/runtime.rs", "runtime/metrics.rs"],
  "risk": "moderate"
}
```

**Tests:**
- `test_hologram_plan_finds_definition_file` — "where is block_on defined" → edit=runtime/runtime.rs
- `test_hologram_plan_finds_test_files` — "verify the block_on implementation" → verify contains test files
- `test_hologram_plan_coupled_files` — coupled files share vocabulary with edit file

### Files to change
- `crates/reliary-search/src/pack.rs` or new `crates/reliary-search/src/plan.rs` — add `hologram_plan()`
- `crates/reliary-agent/src/mcp.rs` — add `reliary_plan` tool dispatch + schema
- `bench/multi_turn_harness.py` — add `tool_reliary_plan` wrapper, add to RELIARY_TOOLS, update RELIARY_SYS

### Expected impact
| Query | Current judge | After hologram plan |
|-------|--------------|---------------------|
| q3 | 0 | **3** (plan → runtime/runtime.rs) |
| q8 | 0 | **2** (plan → buf_writer.rs) |
| q10 | 0 | **1** (plan → io/util/ files) |
| q2 | 0 | **1** (plan → buf_writer.rs + take.rs) |

---

## Phase 3: Proximity Bonus (1 hour)

### What it does
When 2+ query terms appear within N lines of each other in a file, the file gets a bonus. E.g., searching for "block_on schedule wake" — a file where all 3 appear on adjacent lines (scheduler file) ranks higher than one where they're spread across 500 lines.

### Why it matters
- **q4 (call chain):** The query asks about `block_on → spawn → schedule → wake → push → queue`. These terms cluster in scheduler files. Proximity bonus would boost scheduler files above general runtime files.

### Implementation

**File:** `crates/reliary-search/src/search.rs`

Port stria's `proximity_bonus()`:
1. After BM25 + quale reranking, for the top 20 candidates
2. For each candidate, query `occurrence` table for line numbers of each query term
3. Compute proximity: for each pair of terms, find min distance between their line sets
4. Bonus = Σ (max_gap - min_dist + 1) / max_gap for pairs within max_gap
5. Add bonus to final score (weight: 0.1 × proximity, 0.9 × quale+BM25)

**Key function** (from stria):
```rust
pub fn proximity_bonus(line_sets: &[&[usize]], max_gap: usize) -> f64 {
    // For each pair of term line-sets, find min distance
    // Bonus = Σ (max_gap - min_dist + 1) / max_gap, averaged over pairs
}
```

**Integration:** After quale reranking, before final sort. Query `SELECT line FROM occurrence WHERE file_id=?1 AND phrase_id IN (...)` for each candidate file.

**max_gap:** 50 lines (configurable). Terms within 50 lines of each other indicate they're in the same function/module.

**Tests:**
- `test_proximity_bonus_same_line` — terms on same line → max bonus
- `test_proximity_bonus_far_apart` — terms 100 lines apart → 0 bonus
- `test_proximity_bonus_boosts_scheduler` — "block_on schedule wake" → scheduler files rank higher

### Files to change
- `crates/reliary-search/src/search.rs` — add `proximity_bonus()` + integrate into search
- `crates/reliary-search/src/search.rs` — add tests

### Expected impact
| Query | Current judge | After proximity |
|-------|--------------|-----------------|
| q4 | 2 | **3** (scheduler files cluster) |

---

## Phase 4: Integration + Bench (1 hour)

### System prompt update

Update `RELIARY_SYS` in `multi_turn_harness.py`:
```
Tools:
- plan(task): PRIMARY tool for any question. Given a task/question, returns the most likely file(s) to look at, related test files, and coupled files. Call this FIRST.
- pack_query(name): Focused context for a specific symbol (~2k chars).
- callgraph(name): Cross-reference questions ("who calls X", "what does X call").
- find_references(name): Find all references to a symbol.
- search(query): Full-text search.
```

### Run 3-way bench

1. Re-index tokio with all fixes
2. Run A, B, C with seeds 42, 17
3. Run LLM judge (deepseek-v4-pro) on all results
4. Compare: A judge score should be ≥ 14 (matching/beating grep at 15, altbackend at 11.5)

### Success criteria

| Metric | Target | Current |
|--------|--------|---------|
| A judge score | ≥ 14/30 | 7/30 |
| A vs B (altbackend) | A ≥ B | A=7, B=11.5 |
| A vs C (grep) | A ≥ C | A=7, C=15 |
| Billed cost | ≤ 27k | 25.7k |
| Cache safety | preserved | 95% hit rate |
| Score variance | σ ≤ 1.5 | σ=0.7 |

### Rollback

If judge score doesn't improve:
1. Check quale reranking: verify is_def×5 boost is applied (add eprintln)
2. Check hologram plan: verify it returns the right file (add eprintln)
3. Check proximity: verify scheduler files get proximity bonus (add eprintln)
4. If all 3 are working but score doesn't improve, the issue is model-side (tool selection, not tool quality)

---

## Summary

| Phase | Feature | Effort | Impact | Files |
|-------|---------|--------|--------|-------|
| 1 | Quale reranking | 2h | +3-5 judge points | search.rs |
| 2 | Hologram plan | 3h | +3-4 judge points | pack.rs/plan.rs, mcp.rs, harness |
| 3 | Proximity bonus | 1h | +1 judge point | search.rs |
| 4 | Integration + bench | 1h | Verification | harness, judge |
| **Total** | | **7h** | **+7-10 judge points** | |

All grammar-free. All ported from proven stria code. All use the existing SQLite index. No new schema, no new indexing, no language-specific code.

Expected final: A judge = 14-17/30 (beating altbackend at 11.5, matching/beating grep at 15).