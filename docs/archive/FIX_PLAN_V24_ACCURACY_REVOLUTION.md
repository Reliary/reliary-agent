# FIX_PLAN_V24_ACCURACY_REVOLUTION.md

## Goal: Judge score 9/30 → 24-28/30. Beat altbackend (13.5) and grep (13.5) on accuracy AND cost.

## Root cause of current failure

V23 added quale reranking + hologram plan + proximity bonus, but placed them in
`search_fts5()` — the WRONG code path. The model uses `find_references`, not
`search`. The features never reached the model's output.

Additionally, our tools return `{file, line, col}` — the minimum possible
information. The model can't disambiguate `block_on` at `task/local.rs:18` from
`block_on` at `runtime/runtime.rs:375`. Altbackend solves this with qualified
names (`tokio::runtime::Runtime::block_on`). Grep solves this with surrounding
context (`impl Runtime {` 3 lines above).

We need BOTH advantages in ONE tool call.

---

## Phase 1: Qualified Names (2h) — THE key fix

### What

Add `qualified_name` to every `OccHit` in find_references output. Derive
grammar-free from two sources:

1. **File-path heuristic** (fast path, 80% coverage):
   - `runtime/runtime.rs` → `Runtime` (capitalize basename without extension)
   - `io/util/take.rs` → `Take` (capitalize basename)
   - `task/local.rs` → `Local` (capitalize basename)
   - Strip common prefixes like `mod`, `lib`, `tests`
   - Only use if the capitalized name is PascalCase (starts uppercase)

2. **Brace-graph walk-up** (fallback, 95% coverage):
   - From the hit line, walk UP through `file_meta::brace_graph` parent nodes
   - Find the nearest node with role `type_def` or `impl_target`
   - Use that type name as the qualifier
   - If the hit is inside `impl Runtime`, qualifier = `Runtime`
   - If the hit is inside `impl BufWriter<W> for AsyncWrite`, qualifier = `BufWriter`

### Where

**File:** `crates/reliary-search/src/type_flow.rs`
**Function:** `find_references_auto` (line 800)

### Code sketch

```rust
// New helper in type_flow.rs or a new module qualified.rs

/// Derive qualified name for a hit. Grammar-free:
/// 1. Fast path: file-path basename → capitalize
/// 2. Fallback: brace-graph walk-up to nearest type def
pub fn derive_qualified_name(
    file_path: &str,
    line: i32,
    meta: &crate::file_meta::FileMeta,
) -> String {
    // Fast path: file-path heuristic
    let basename = file_path.rsplit('/').next()
        .and_then(|f| f.rsplit('.').next_back())
        .unwrap_or("");
    // Try the SECOND-to-last component if basename is generic (mod.rs, lib.rs)
    let type_name = if basename == "mod" || basename == "lib" || basename == "tests" {
        // Use the directory name instead
        file_path.rsplitn(2, '/').nth(1)
            .and_then(|p| p.rsplit('/').next())
            .unwrap_or(basename)
    } else {
        basename
    };
    let capitalized = capitalize_first(type_name);

    // Check if capitalized looks like a type name (PascalCase)
    if capitalized.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
        // Verify: does the brace graph have a type def at or above this line?
        if let Some(type_def_name) = brace_graph_type_lookup(meta, line) {
            return format!("{}::{}", type_def_name, /* function name */);
        }
        // Fallback to file-path heuristic
        return format!("{}::*", capitalized);
    }

    // Brace-graph fallback
    if let Some(type_def_name) = brace_graph_type_lookup(meta, line) {
        return format!("{}::*", type_def_name);
    }

    // Last resort: just the basename
    capitalized
}

/// Walk up brace graph from line to find nearest type def.
fn brace_graph_type_lookup(meta: &FileMeta, line: i32) -> Option<String> {
    let bg = &meta.brace_graph;
    // Find the node containing this line
    let node = bg.nodes.iter()
        .find(|n| n.start_line <= line as usize && line as usize <= n.end_line)?;
    // Walk up parent chain
    let mut current = node;
    loop {
        if current.role == "type_def" || current.role == "impl_target" {
            return Some(current.name.clone());
        }
        current = current.parent.as_ref().and_then(|p| bg.nodes.get(*p))?;
    }
}
```

### Output format change

Current:
```json
{"file": "runtime/runtime.rs", "line": 375, "col": 4}
```

New:
```json
{"file": "runtime/runtime.rs", "line": 375, "col": 4, "qualified_name": "Runtime::block_on"}
```

### Impact

Fixes q3 (block_on def): model sees `Runtime::block_on` vs `LocalSet::block_on` and picks the right one.
Fixes q5 (spawn callers): model sees `Handle::spawn` vs `thread::spawn`.
Fixes q8 (bufwriter write): model sees `BufWriter::poll_write`.

### Tests

```rust
#[test]
fn test_qualified_name_from_filepath() {
    assert_eq!(derive_qualified_name("runtime/runtime.rs", 375, &empty_meta), "Runtime::*");
    assert_eq!(derive_qualified_name("io/util/take.rs", 121, &empty_meta), "Take::*");
    assert_eq!(derive_qualified_name("task/local.rs", 18, &empty_meta), "Local::*");
}

#[test]
fn test_qualified_name_mod_rs() {
    // mod.rs uses directory name
    assert_eq!(derive_qualified_name("runtime/scheduler/mod.rs", 202, &empty_meta), "Scheduler::*");
}

#[test]
fn test_qualified_name_brace_graph_fallback() {
    // Build a meta with brace graph containing impl Runtime at line 370
    // Hit at line 375 should resolve to Runtime
    let meta = FileMeta {
        // ... brace graph with impl Runtime at line 370-400
        ..Default::default()
    };
    assert_eq!(derive_qualified_name("runtime.rs", 375, &meta), "Runtime::block_on");
}
```

---

## Phase 2: Context Window (1h) — match grep's advantage

### What

Add 3 lines before + 3 lines after each hit to every find_references response.
The model sees `impl Runtime {` above the `block_on` definition, giving it the
same context grep provides — but from our structured index (no disk read,
data from `file_meta::get().lines` cache).

### Where

**File:** `crates/reliary-agent/src/mcp.rs`
**Function:** `reliary_find_references` dispatch (line ~864)
**File:** `crates/reliary-agent/src/mcp.rs`
**Function:** `reliary_find_references_with_source` dispatch (line ~1093)

### Code sketch

```rust
// In mcp.rs, after collecting hits, add context lines:
fn add_context_window(hits: &[OccHit], wd: &str) -> Vec<serde_json::Value> {
    hits.iter().map(|h| {
        // Get file_meta cache (Arc<FileMeta>)
        let context_lines: Vec<String> = if let Some(meta) = reliary_search::file_meta::get(&h.file_path) {
            let start = (h.line as usize).saturating_sub(3);
            let end = (h.line as usize + 4).min(meta.lines.len());
            meta.lines[start..end].iter()
                .enumerate()
                .map(|(i, line)| format!("{}: {}", start + i + 1, line))
                .collect()
        } else {
            vec![]
        };

        serde_json::json!({
            "file": relpath_with(&h.file_path, wd),
            "line": h.line + 1,
            "col": h.col,
            "qualified_name": derive_qualified_name(&h.file_path, h.line, ...),
            "context": context_lines,
        })
    }).collect()
}
```

### Output format change

Current:
```json
{"file": "runtime.rs", "line": 375, "col": 4}
```

New:
```json
{
    "file": "runtime.rs",
    "line": 375,
    "col": 4,
    "qualified_name": "Runtime::block_on",
    "context": [
        "372: impl Runtime {",
        "373:     /// Block the current thread on the future.",
        "374:     ///",
        "375:     pub fn block_on<F: Future>(&self, future: F) -> F::Output {",
        "376:         ...",
        "377:     }"
    ]
}
```

### Impact

Fixes q1 (consume impls): model sees `impl Take<Reader>` above `fn consume`.
Fixes q7 (sleep methods): model sees `impl Sleep` above method defs.
Fixes q8 (bufwriter write): model sees `impl BufWriter<W>` above `poll_write`.

### Token cost

Context window adds ~200 chars per hit. With 10 hits, that's ~2k extra chars
per call. Over 10 turns, that's ~20k extra tokens. BUT: the model needs FEWER
calls because it can see the context directly (no need to call `read` to
verify the enclosing type). Net: probably neutral or positive.

### Tests

```rust
#[test]
fn test_context_window_3_before_3_after() {
    let meta = FileMeta {
        lines: vec![
            "line 1".into(), "line 2".into(), "line 3".into(),
            "target line".into(), "line 5".into(), "line 6".into(),
            "line 7".into(),
        ],
        ..Default::default()
    };
    // Hit at line 3 (0-indexed)
    let ctx = get_context(&meta, 3, 3, 3);
    assert_eq!(ctx.len(), 7); // 3 before + 1 target + 3 after
    assert_eq!(ctx[0], "1: line 1");
    assert_eq!(ctx[3], "4: target line");
}
```

---

## Phase 3: Caller-Count Ranking (30min) — in the RIGHT path

### What

Replace `LENGTH(f.file_path) ASC` ranking with `caller_count DESC` in
`find_references_auto`. A definition with 50 callers is more central than
one with 3 callers.

### Where

**File:** `crates/reliary-search/src/type_flow.rs`
**Function:** `find_references_auto` (line 831-849)
**Both SQL queries** (with and without type_hint)

### Current SQL

```sql
SELECT f.file_path, o.line, o.block_id FROM occurrence o
JOIN file_map f ON f.id = o.file_id
WHERE o.phrase_id = ?1 AND o.is_def != 0
ORDER BY (f.file_path LIKE '%/tests/%') ASC,
         LENGTH(f.file_path) ASC,
         o.occ_id LIMIT 10
```

### New SQL

```sql
SELECT f.file_path, o.line, o.block_id,
       (SELECT COUNT(*) FROM occurrence o2
        WHERE o2.phrase_id = o.phrase_id
        AND o2.file_id = o.file_id
        AND o2.is_def = 0) as caller_count
FROM occurrence o JOIN file_map f ON f.id = o.file_id
WHERE o.phrase_id = ?1 AND o.is_def != 0
ORDER BY (f.file_path LIKE '%/tests/%') ASC,
         caller_count DESC,
         LENGTH(f.file_path) ASC,
         o.occ_id LIMIT 10
```

### Impact

`Runtime::block_on` (50 callers) ranks above `LocalSet::block_on` (5 callers)
ranks above `block_on` free function (3 callers).

Fixes q3 (block_on def): Runtime::block_on picked as anchor.
Fixes q5 (spawn callers): Handle::spawn picked as anchor.

### Tests

```sql
-- Verify: runtime/runtime.rs has more non-def occurrences of block_on
-- than task/local.rs
SELECT f.file_path, COUNT(*) as callers
FROM occurrence o JOIN file_map f ON f.id = o.file_id
WHERE o.phrase_id = (SELECT id FROM phrases WHERE phrase = 'block_on')
AND o.is_def = 0
GROUP BY f.file_path ORDER BY callers DESC;
```

---

## Phase 4: Top 3 Candidates (30min) — let the model decide

### What

Instead of picking ONE anchor and committing to it, return the top 3
candidate definitions with qualified names + caller counts. The model
disambiguates based on context.

### Where

**File:** `crates/reliary-search/src/type_flow.rs`
**Function:** `find_references_auto` (line 875-878)

### Current behavior

```rust
if let Some((anchor_file, anchor_line)) = best {
    return find_references_type_flow(db, raw_name, &anchor_file, anchor_line, threshold);
}
```

Picks ONE best and runs full type_flow with it. If wrong, everything downstream
is wrong.

### New behavior

```rust
// Return top 3 candidates to the caller (don't commit to one).
// The MCP handler includes them in the response so the model can pick.
let candidates: Vec<(String, i32, i64)> = /* collect top 3 from the SQL loop */;

// Still pick the best one for the main search
if let Some((anchor_file, anchor_line)) = best {
    let mut hits = find_references_type_flow(db, raw_name, &anchor_file, anchor_line, threshold)?;
    // Attach candidate info to the response
    return Ok(hits); // MCP handler will also include candidates
}
```

### MCP handler change (mcp.rs)

```rust
// In the find_references dispatch, after getting hits:
let response = serde_json::json!({
    "query": sym,
    "hits": arr,
    "count": hits.len(),
    "candidates": candidates.iter().map(|(fp, line, count)| {
        serde_json::json!({
            "qualified_name": derive_qualified_name(fp, *line, ...),
            "file": relpath_with(fp, &wd),
            "line": line + 1,
            "callers": count,
        })
    }).collect::<Vec<_>>(),
});
```

### Output format

```json
{
    "query": "block_on",
    "candidates": [
        {"qualified_name": "Runtime::block_on", "file": "runtime.rs", "line": 375, "callers": 50},
        {"qualified_name": "LocalSet::block_on", "file": "local.rs", "line": 18, "callers": 5},
        {"qualified_name": "block_on", "file": "block_on.rs", "line": 17, "callers": 3}
    ],
    "hits": [...],
    "count": 15
}
```

### Impact

Model sees 3 candidates with qualified names and picks the right one.
Even if our ranking is wrong, the model can self-correct.

Fixes q3, q5, q8 — all "which definition?" questions.

### Tests

```rust
#[test]
fn test_top_3_candidates_returned() {
    let db = /* test db with multiple block_on defs */;
    let hits = find_references_auto(&db, "block_on", 0.1).unwrap();
    // The MCP handler should include candidates in the response
    // Verify: candidates has 3 entries with qualified names
}
```

---

## Phase 5: Token Reduction (1h) — fix the WC bloat

### What

V23's plan tool bloated WC to 285k. Fix by:

1. **System prompt**: "Call plan ONCE at the start. For subsequent questions, use find_references or callgraph directly."
2. **Compact hit format**: relative paths + qualified names (50 chars vs 100)
3. **Compact callgraph**: qualified names only, no source preview (150 chars vs 500)
4. **Plan output**: already trimmed in V23b (edit + verify + risk only)
5. **Context window**: 3+3 lines but compact (no leading whitespace)

### Where

**File:** `bench/multi_turn_harness.py` — `RELIARY_SYS` (line 463)
**File:** `crates/reliary-agent/src/mcp.rs` — find_references output formatting
**File:** `crates/reliary-agent/src/mcp.rs` — callgraph output formatting

### System prompt change

```python
RELIARY_SYS = """You are a code intelligence agent with limited turns. You MUST answer in 3-5 tool calls.

Tools:
- plan(task): CALL ONCE at the start for orientation. Returns the most likely file + risk.
- find_references(name): PRIMARY tool. Returns file:line + qualified_name + context (3 lines before/after) per hit.
- callgraph(name): For "who calls X" / "what does X call". Returns qualified names of callers/callees.
- goto_def(name): Find the definition site of a symbol.
- search(query): Full-text search for unknown symbols.

Rules:
- Call plan ONCE. Then use find_references or callgraph for each question.
- Use the qualified_name field to disambiguate same-named symbols.
- Use the context field to understand the enclosing type.
- Answer in 1-2 tool calls per question after the initial plan.
"""
```

### Compact hit format

Current: `{"file": "/tmp/tokio-corpus/tokio/src/runtime/runtime.rs", "line": 375, "col": 4}`
New: `{"f": "runtime/runtime.rs", "l": 376, "q": "Runtime::block_on", "ctx": ["impl Runtime {", ...]}`

Saves ~40 chars per hit × 15 hits = 600 chars per call × 10 turns = 6k tokens.

### Expected WC

| Source | Current (V23) | After V24 |
|--------|--------------|-----------|
| Plan (1 call) | ~500 chars | ~200 chars |
| Find_references (10 calls) | ~20k chars | ~15k chars (compact + context) |
| Callgraph (5 calls) | ~5k chars | ~3k chars (no source preview) |
| Per-turn overhead | ~2k chars | ~1k chars (compact format) |
| **Total tool bytes** | **285k** | **~100-120k** |

### Tests

- Verify WC < 150k on 2-seed bench
- Verify score ≥ 20/30 (no regression from compact format)

---

## Phase 6: Quale + Proximity in the RIGHT path (1h)

### What

Move `quale_rerank` and `proximity_bonus` from `search_fts5` (wrong path)
to the ACTUAL paths the model uses:

1. `find_references_auto` → add quale reranking (is_def×5 boost)
2. `find_references_type_flow` → add proximity bonus (term clustering)
3. `find_references_pattern_hybrid` → add quale reranking

### Where

**File:** `crates/reliary-search/src/type_flow.rs`
**Function:** `find_references_auto` (line 800) — add quale after SQL results

**File:** `crates/reliary-search/src/pattern.rs`
**Function:** `find_references_pattern_hybrid` (line 256) — add quale

**File:** `crates/reliary-search/src/type_flow.rs`
**Function:** `find_references_type_flow` (line 961) — add proximity

### Code sketch (quale in find_references_auto)

```rust
// After collecting hits from SQL:
let mut hits: Vec<OccHit> = /* collected from SQL */;

// V24: Apply quale reranking — boost is_def hits ×5
for hit in &mut hits {
    if hit.is_def {
        hit.similarity *= 5.0; // Quale boost
    }
}

// V24: Apply proximity bonus
if let Some(meta_map) = /* get file_meta for each hit's file */ {
    apply_proximity_bonus_to_hits(&mut hits, &meta_map, terms);
}

// Sort by modified score
hits.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(Equal));
```

### Code sketch (proximity in find_references_type_flow)

```rust
// After computing cosine similarity for each hit:
// V24: Add proximity bonus for hits where query terms cluster
let query_terms = extract_terms_from_name(raw_name);
for hit in &mut hits {
    let meta = file_meta::get(&hit.file_path);
    if let Some(m) = meta {
        let bonus = compute_proximity_for_file(&m, &query_terms, hit.line as usize);
        hit.similarity += bonus * 0.1; // Small boost, doesn't dominate
    }
}
```

### Impact

Quale: `is_def=1` hits rank above `is_def=0` hits. The model sees definitions first.
Proximity: files where "block_on" and "scheduler" and "wake" appear within 50 lines
get a bonus. Helps q4 (call chain).

### Tests

```rust
#[test]
fn test_quale_boost_in_find_references() {
    let db = /* test db */;
    let hits = find_references_auto(&db, "block_on", 0.1).unwrap();
    // First hit should be a definition (is_def=true)
    assert!(hits[0].is_def, "first hit should be a definition after quale boost");
}

#[test]
fn test_proximity_bonus_in_type_flow() {
    // File with block_on and scheduler within 10 lines should rank higher
    // than file with block_on and scheduler 500 lines apart
}
```

---

## Verification Plan

### Step 1: Unit tests (30 min)

Run all existing tests + new tests for each phase. Verify:
- Qualified name derivation works on Rust, Python, Go file paths
- Context window returns correct lines
- Caller-count ranking puts Runtime::block_on first
- Top 3 candidates are returned with qualified names
- Quale boost puts is_def hits first
- Proximity bonus fires for clustered terms

### Step 2: Integration test (15 min)

Call `find_references("block_on")` via MCP and verify:
- First hit has `qualified_name: "Runtime::block_on"`
- First hit has `context` with `impl Runtime {` 3 lines above
- Response has `candidates` with top 3 definitions
- `Runtime::block_on` has highest `callers` count

### Step 3: Re-index tokio (5 min)

```bash
cd /tmp/tokio-corpus/tokio/src
rm -f .reliary/index.sqlite
reliary trust .
reliary build-all .
```

### Step 4: Run 3-way bench (1 hour)

```bash
python3 bench/long_session_bench.py --seeds 42 17 --conditions A,B,C
```

### Step 5: Run LLM judge (30 min)

```bash
python3 bench/llm_judge.py --input bench/results/latest.jsonl
```

### Step 6: Verify results

| Check | Pass criteria |
|-------|-------------|
| A judge score | ≥ 20/30 |
| A vs B judge | A > B (beat altbackend) |
| A vs C judge | A > C (beat grep) |
| A WC | < 150k |
| A billed | < 25k |
| A cost/judge-point | < 1,500 |
| No regression | All unit tests pass |

---

## Summary

| Phase | Feature | Time | Impact |
|-------|---------|------|--------|
| 1 | Qualified names (file-path + brace-graph) | 2h | +5-8 judge |
| 2 | Context window (3+3 lines) | 1h | +3-5 judge |
| 3 | Caller-count ranking in find_references | 30min | +2-3 judge |
| 4 | Top 3 candidates with qualified names | 30min | +2-3 judge |
| 5 | Token reduction (compact + plan-once) | 1h | -60% WC |
| 6 | Quale + proximity in right path | 1h | +2-3 judge |
| **Total** | | **6h** | **+14-22 judge, -60% WC** |

### Expected final results

| Metric | Current (V23) | After V24 | Target |
|--------|--------------|-----------|--------|
| Judge | 9.0 | **24-28** | Beat 13.5 |
| WC | 285k | **100-120k** | < 150k |
| Billed | 48k | **18-22k** | < 25k |
| vs altbackend | Lose (9 vs 13.5) | **Win** (24-28 vs 13.5) | Beat |
| vs grep | Lose (9 vs 13.5) | **Win** (24-28 vs 13.5) | Beat |
| vs altbackend cost | Lose (48k vs 30k) | **Win** (18-22k vs 30k) | Beat |
| vs grep cost | Lose (48k vs 41k) | **Win** (18-22k vs 41k) | Beat |

### Grammar-free verification

ALL 6 phases are grammar-free:
- Phase 1: file-path string manipulation + brace-graph tree walk
- Phase 2: line array indexing
- Phase 3: SQL COUNT
- Phase 4: SQL LIMIT 3
- Phase 5: output format compaction
- Phase 6: is_def flag + line-number distance math

Zero keywords. Zero AST. Zero language detection. Zero tree-sitter.

### Why this will work where V23 didn't

V23 added features to the WRONG code path (search_fts5). V24 puts them in
the RIGHT path (find_references_auto + pattern_hybrid + type_flow).

V23 returned ambiguous `{file, line}` — model couldn't disambiguate.
V24 returns `qualified_name` + `context` + `candidates` — model has ALL
the information it needs in ONE call.

V23 let the model call plan 10 times (bloat). V24 says "call plan ONCE".

V23's WC was 285k. V24 targets 100-120k via compact format + plan-once.