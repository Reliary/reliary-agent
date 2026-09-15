# V45 — Silent BM25 Fallback (Grep-Like Never-Empty)

## Root Cause

The model enters a dead-end spiral when `find_references` returns empty. Even with one-line answers and raw code, the model retries with slightly different parameter values (typos, wrong concepts) because "not found" triggers LLM self-correction loops. Grep has 0 dead-ends because it ALWAYS returns context — even for nonsense queries. The model never enters a retry loop.

## The Plan

### Phase 1: Revert V44 entirely (5 min)

The `search_auto_route` helper and `find_similar_phrase` in symbol.rs are wrong approaches:
- `search_auto_route` is in the wrong handler (model calls `find_references`, not `search`)
- `find_similar_phrase` returns wrong near-miss suggestions that worsen the spiral
- `find_references` proactive correction was added to the wrong call path

Remove: `search_auto_route` helper function from mcp.rs
Remove: `find_similar_phrase` from symbol.rs (keep if no references remain)
Remove: proactive correction block from find_references handler
Keep: `stem_identifier` fixes (V38) and `classify_structural` fixes (V26-V30) — they're correct

### Phase 2: Add silent BM25 fallback to find_references (30 min)

**File: `crates/reliary-agent/src/mcp.rs`**

After the 4-fallback `phrase_id_for` chain AND after `hits` is built, before any empty-result return:

```rust
// After: let hits = ... (the V39 direct query)
// Before: any "No matches" or recovery_hint blocks

if hits.is_empty() && !sym.is_empty() {
    // V45: Silent BM25 fallback — never return empty.
    // The model enters a retry loop on "not found". grep-like
    // behavior: always return contextual results.
    let bm25_results = reliary_search::search::search_fts5(&db, sym, 3);
    if bm25_results.is_empty() {
        let broader: String = sym.chars().take(20).collect();
        // Try broader search
    }
    if !bm25_results.is_empty() {
        let files: Vec<String> = bm25_results.iter()
            .map(|r| {
                let f_short = std::path::Path::new(&r.file)
                    .file_name()
                    .map(|x| x.to_string_lossy().to_string())
                    .unwrap_or_else(|| r.file.clone());
                format!("{}", f_short)
            })
            .collect();
        let code_hint = if bm25_results.len() == 1 {
            // Single result — show top line as evidence
            reliary_search::file_meta::get(&bm25_results[0].file)
                .and_then(|meta| meta.lines.first().cloned())
                .map(|s| format!("\n{}:1    {}", files[0], s.trim()))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let text = format!("No exact match for \"{}\". Closest files: {}{}\n", 
            sym, files.join(", "), code_hint);
        return DispatchResult::Success(serde_json::json!({
            "content": [{ "type": "text", "text": text }]
        }));
    }
}
```

### Phase 3: Run 3-seed bench against tokio corpus (2 hours)

```bash
cd $HOME/src/reliary8
python3 bench/long_session_bench.py --conditions "A,B,C" --seeds 42 17 123 --timeout 300
```

Then run accuracy scorer on results.

## Expected Impact

| Metric | V42 | V45 target |
|--------|-----|-----------|
| Dead-ends (tokio) | 8 | **0-3** |
| Dead-ends (reliary) | 6 | **0-3** |
| Score | 23.4 | 23-25 (preserved) |
| WC | 120k | 130-140k (+5-10% from fallback results) |
| Billed | 22k | 24-28k |

## Grammar-Free Verification

| Fix | Grammar-free? | Why |
|-----|--------------|-----|
| BM25 fallback | Yes | BM25 works on any text, any language |
| "Never return empty" | Yes | Universal UX pattern, not benchmark-specific |

## Is This Fitting?

No. The "never return empty" pattern is universal:
- Google doesn't return "no results" — it returns closest matches
- grep doesn't return empty — it returns matching lines from any file
- Any search tool that returns empty triggers retry behavior in LLMs
- This fix helps ALL users on ALL repos, not just our benchmark

## The Key Insight

We built the perfect tool that returns exactly the right answer or explicitly says "not found." But the LLM is a stochastic pattern matcher, not a compiler. "Not found" triggers retry loops. We need a tool that always gives the model SOMETHING to work with.

## Effort

| Phase | Time |
|-------|------|
| 1. Revert V44 | 5m |
| 2. Add BM25 fallback | 30m |
| 3. Run bench + score | 2h |
| **Total** | **~2.5 hours** |
