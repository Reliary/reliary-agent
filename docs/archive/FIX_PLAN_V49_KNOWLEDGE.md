# V49 — Knowledge Output (Not Search Results)

## Problem

V42 (our peak) returns raw search hits via `find_references`. The model has to extract
structure from raw file:line data. Some runs succeed, some fail — 2.7× variance.

Grep works because its output IS the knowledge (raw source lines). Altbackend works because
its output IS relationships (graph). Reliary returns raw hits and expects the model to
synthesize. It fails.

Stria and quale returned KNOWLEDGE (edit: file, verify: file, risk: level). We've been
returning SEARCH RESULTS (file:line + source). That's the gap.

## Approach

Make `find_references` return a knowledge summary instead of raw hits:

```
consume (AsyncBufRead::consume)
  defined at io/util/take.rs:121 — fn consume(self: Pin<&mut Self>, amt: usize)
  implemented by 6 types:
    Take::consume (take.rs:126), Empty::consume (empty.rs:89),
    Chain::consume (chain.rs:128), BufWriter::consume (buf_writer.rs:284),
    BufStream::consume (buf_stream.rs:194), BufReader::consume (buf_reader.rs:140)
  called from 3 sites: buf_reader.rs:117, buf_writer.rs:312, buf_stream.rs:272
```

The model copies relationships verbatim. No synthesis needed. No hallucination possible.

## What changes

### Phase 1: Add knowledge extraction functions (3 hours)

**1a. `extract_knowledge(name, phrase_id, db)` — returns `Knowledge` struct:**

```rust
struct Knowledge {
    definition: Option<(String, i32, String)>,  // (file, line, signature)
    implementors: Vec<(String, String, i32)>,    // (qualified_name, file, line)
    callers: Vec<(String, i32)>,                 // (file, line)
    callees: Vec<String>,                        // names of called functions
    methods: Vec<String>,                        // method names (if type)
    dead_items: Vec<(String, String, i32)>,      // (name, file, line) if dead_only
}
```

Data sources:
- `definition`: occurrence table WHERE is_def=1, ORDER BY centrality_core ASC
- `implementors`: occurrence table WHERE is_def=1, different file from definition
- `callers`: occurrence table WHERE is_def=0, exclude test file paths
- `callees`: call_graph (existing function)
- `methods`: find_methods_on (existing function)
- `dead_items`: dead_symbols (existing function)

**1b. Format as structured text:**

```
{name} ({trait_name}::{name})
  defined at {file}:{line} — {signature}
  implemented by {count} types:
    {qualified_name} ({file}:{line}), ...
  called from {count} sites: {file}:{line}, ...
  calls: {function}, {function}, ...
  methods: {method}, {method}, ...
  dead items: {name} ({file}:{line}), ...
```

The format adapts based on what data is available:
- If `def_only=true`: show only "defined at" line
- If `usage_only=true`: show only "called from" lines
- If `methods=true`: show only "methods" line
- If `dead_only=true`: show only "dead items" line
- Default (no params): show all available knowledge
- If `path_filter=X`: filter all results to path prefix X

### Phase 2: Replace find_references output (2 hours)

Replace the current def_only/usage_only/path_filter/default output blocks in mcp.rs
with a single `extract_knowledge` call + format. The current flow:

```
find_references → phrase_id fetch → occurrence query → (130 lines of format logic
for def_only/usage_only/path_filter/default with V40/V42/V48 remnants)
```

Replaced by:

```
find_references → phrase_id fetch → extract_knowledge(name, phrase_id, db)
→ format_knowledge(knowledge, params) → return as text
```

### Phase 3: Update system prompt (15 min)

Remove tool selection hints ("use def_only for definitions"). The tool now
automatically returns the right knowledge based on context. Model just calls
`find_references(name=X)` and gets structured knowledge back.

### Phase 4: Test + bench (2 hours)

1. Re-index tokio with corrected index
2. Test: `find_references("consume")` returns structured knowledge
3. Test: `find_references("block_on", def_only=true)` returns definition knowledge
4. Test: `find_references("sleep", methods=true)` returns method knowledge
5. Run 5-seed 3-way bench
6. Run LLM judge

## Expected impact

| Query | Current (V42 judge) | V49 target | Mechanism |
|-------|--------------------|-----------|-----------|
| q1 (consume impls) | 0-2 | **3** | "implemented by Take, Empty, Chain..." — model copies |
| q2 (consume callers) | 0-2 | **3** | "called from buf_reader.rs:117" — model copies |
| q3 (block_on def) | 0-2 | **3** | "defined at runtime.rs:340" — model copies |
| q4 (block_on chain) | 0-1 | **2-3** | "calls: block_on_inner, CurrentThread::block_on" — model copies |
| q5 (spawn callers) | 0 | **2-3** | "called from local.rs:677" — model copies |
| q6 (sleep def) | 3 | **3** | Already works |
| q7 (sleep methods) | 0-2 | **3** | "methods: far_future, deadline..." — model copies |
| q8 (bufwriter) | 0-1 | **2** | "defined at buf_writer.rs:249" + "methods: poll_write, poll_flush..." |
| q9 (consume recheck) | 0 | **3** | Same as q1 |
| q10 (dead code) | 0 | **1-2** | "dead items: set_limit (take.rs:45)..." (dead_only=true) |
| **Total** | **10.0** | **22-28** | (+12-18) |

## Cost trade-off

Knowledge output is slightly more verbose than V42's one-line but less verbose than
V46's `/// @rel` comment blocks or V48's grep-like 3-lines-per-hit format.

| Metric | V42 | V49 (estimated) |
|--------|-----|-----------------|
| Output per query | ~100 chars | ~300-500 chars |
| Tool bytes per session | ~15k | ~25-35k |
| Billed cost | 46k | 55-65k |
| Judge | 10.0 | 22-28 |

Cost goes up 20-40% but accuracy goes up 120-180%. Cost-per-judge-point improves dramatically.

## Grammar-free?

**Yes.** All knowledge extraction uses:
- `occurrence` table (is_def, tag — set by grammar-free classify_structural at ingest)
- `call_graph` (set by grammar-free brace_graph + scan_identifiers)
- `find_methods_on` (set by grammar-free brace_graph walk)
- `dead_symbols` (set by occurrence table SQL queries)

No AST, no parsers, no language detection, no keywords.

## Not fitting?

| Aspect | Fitting? | Why |
|--------|---------|-----|
| Knowledge output | No | Any user benefits from "implemented by X, Y, Z" over 20 raw hits |
| Structured summary | No | Any user benefits from "called from A, B, C" over raw file:line |
| Automatic knowledge extraction | No | All data from existing index — no new data sources |
| Format adaptation | No | Always show available info — no benchmark-specific tuning |

## Deletion list

The V49 knowledge format replaces these scattered output paths:
- def_only output (V40-V42 one-line format) — replaced by "defined at" line
- usage_only output (V40-V42 caller list) — replaced by "called from" lines
- path_filter synthesized output (V30-V48) — replaced by path_filter on all knowledge
- default mode output (V40-V42 abbreviated) — replaced by full knowledge
- method listing (V37 methods=true routing) — replaced by "methods" line
- dead code output (V37 dead_only=true routing) — replaced by "dead items" line

## Total effort: ~7 hours

| Phase | Hours |
|-------|-------|
| 1. extract_knowledge function | 3h |
| 2. Replace find_references output | 2h |
| 3. System prompt update | 15min |
| 4. Test + bench | 2h |
