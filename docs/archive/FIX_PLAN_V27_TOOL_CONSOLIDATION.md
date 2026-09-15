# V27: Tool Consolidation — 19 → 7 Tools

## Goal

Reduce 19 MCP tools to 7 intuitive tools. Increase judge score without fitting. Reduce dead-ends and token usage.

## Why we have 19 tools

Each variant was built to solve a specific problem (type_flow for type-aware matching, boltzmann for probability scores, with_source for inline source). We never merged variants back into the primary tool. Some are research experiments (boltzmann, query_ast, trace_path). Some are internal (compress, prior, stats, retrieve, risk).

## The 7-tool surface

| # | Tool | Description (for system prompt) | Absorbs |
|---|------|----------------------------------|---------|
| 1 | `reliary_search` | Search for files/symbols by name or topic. BM25 full-text search. | search |
| 2 | `reliary_find_references` | Find all usages of a symbol. Returns file:line + source per hit. Type-aware — filters out same-named different methods. Params: format=json\|grep, with_source=true\|false, path_filter, limit. | find_references, find_references_with_source, find_references_type_flow, find_references_boltzmann |
| 3 | `reliary_goto_def` | Jump to the definition of a symbol. Given a usage line, returns the most likely definition site. | goto_def |
| 4 | `reliary_call_graph` | Who calls X? What does X call? Returns callers and callees with source. Params: direction=inbound\|outbound\|both, depth=1-3. | callgraph_v2, callgraph (old v1), call_graph (file-local), trace_path |
| 5 | `reliary_list_methods` | List all methods on a type. Scans for impl blocks matching the type name. Returns method names with file:line:source. | methods_on |
| 6 | `reliary_find_dead_code` | Find unused/orphaned functions in the codebase. Cross-references against the full index. Params: path (module prefix), functions_only. | dead_symbols, dead |
| 7 | `reliary_describe` | Explain a symbol: purpose, signature, location, callers, methods, surprise facts. One call gives holistic context. | pack, pack_query, plan, risk (partial) |

## Tools to DELETE from MCP surface (12)

### Merge into find_references (3 variants)
- `reliary_find_references_with_source` → use `with_source=true` param
- `reliary_find_references_type_flow` → use `algorithm=type_flow` param (internal, auto-selected)
- `reliary_find_references_boltzmann` → use `algorithm=boltzmann` param (internal, auto-selected)

### Merge into call_graph (3 variants)
- `reliary_callgraph` (old v1) → delete, replaced by call_graph
- `reliary_call_graph` (file-local) → use `scope=file` param
- `reliary_trace_path` → use `direction` + `depth` params

### Merge into find_dead_code (1 variant)
- `reliary_dead` → delete, merged into find_dead_code

### Merge into file_structure (2 variants)
- `reliary_brace_graph` → merged into `reliary_file_structure`
- `reliary_architecture` → merged into `reliary_file_structure`

Wait — file_structure isn't in the 7-tool surface. Let me reconsider:

Actually, `file_structure` can be absorbed into `describe`:
- `describe(file="path")` → returns file structure tree
- `describe(name="symbol")` → returns symbol description

### Merge into describe (3 variants)
- `reliary_pack` → delete (describe generates this internally)
- `reliary_pack_query` → merged into describe
- `reliary_plan` → delete (experimental, not helping)

### Remove from MCP surface (5 internal tools)
- `reliary_compress` — internal, used by sift
- `reliary_prior` — cross-session memory, not MCP-relevant
- `reliary_stats` — diagnostic
- `reliary_retrieve` — internal cache
- `reliary_risk` — can be called from describe output

### Other tools to remove
- `reliary_query_ast` — research experiment, not used by model
- `reliary_scope` — rarely used, low value
- `reliary_find_references_pattern_hybrid` — internal, auto-selected

## Final 7-tool surface

| # | Tool | Key params |
|---|------|------------|
| 1 | `reliary_search` | query, limit |
| 2 | `reliary_find_references` | name, anchor_file?, anchor_line?, format?, with_source?, path_filter?, limit? |
| 3 | `reliary_goto_def` | name, anchor_file, anchor_line |
| 4 | `reliary_call_graph` | name, anchor_file?, anchor_line?, direction?, depth? |
| 5 | `reliary_list_methods` | name (type name) |
| 6 | `reliary_find_dead_code` | path, functions_only? |
| 7 | `reliary_describe` | name? (symbol name) OR file? (file path) |

## System prompt tool routing

```
TOOL SELECTION:
- "where is X defined?" → reliary_goto_def
- "find references/usages of X" → reliary_find_references
- "who calls X?" / "what does X call?" → reliary_call_graph
- "list methods on Type X" → reliary_list_methods
- "find dead/unused code" → reliary_find_dead_code
- "explain/describe X" → reliary_describe
- "search for files about topic" → reliary_search
```

## Implementation phases

### Phase 1: Rename existing tools (30 min)

Rename in mcp.rs PRIMARY_TOOLS list and tool definitions:
- `reliary_dead_symbols` → `reliary_find_dead_code`
- `reliary_methods_on` → `reliary_list_methods`
- `reliary_callgraph_v2` → `reliary_call_graph`
- `reliary_pack_query` → `reliary_describe`

Keep old names as aliases for backwards compatibility (the MCP handler accepts both).

### Phase 2: Remove 12 redundant tools from PRIMARY_TOOLS (30 min)

Remove from the PRIMARY_TOOLS array in mcp.rs:
- find_references_with_source (merge into find_references)
- find_references_type_flow (merge into find_references)
- find_references_boltzmann (merge into find_references)
- callgraph (old v1)
- call_graph (file-local)
- trace_path
- dead
- brace_graph
- architecture
- pack
- plan
- query_ast
- scope
- compress
- prior
- stats
- retrieve
- risk
- find_references_pattern_hybrid (internal)

Keep the Rust implementations — just stop exposing them as MCP tools.

### Phase 3: Merge find_references_with_source into find_references (1 hour)

The `find_references` tool already accepts `format=json|grep` and `with_source` params. The with_source variant is already handled via the `format` and `with_source` params in the same handler. Verify:
- `with_source=true` → includes source text per hit (current _with_source behavior)
- `with_source=false` → file:line only (current find_references behavior)
- `format=grep` → grep-style output (current _with_source grep format)
- `format=json` → JSON output (current _with_source JSON format)

The handler already has this logic — just ensure the `with_source` param is wired correctly.

### Phase 4: Merge callgraph_v2 + trace_path into call_graph (1 hour)

The callgraph_v2 handler already accepts `direction` (inbound/outbound/both). Add:
- `depth` param (default 1, max 3) — from trace_path
- `scope=file` param → falls through to old call_graph (file-local)
- Old callgraph (v1) → delete from dispatch, keep code

### Phase 5: Merge pack + pack_query + plan into describe (1 hour)

`reliary_describe` accepts:
- `name="symbol"` → returns pack_query output (purpose, signature, callers, methods)
- `file="path"` → returns brace_graph output (file structure tree)
- No params → returns pack output (full codebase overview)

The describe tool internally calls:
- `pack_query(name)` when `name` is provided
- `brace_graph(file)` when `file` is provided
- `pack(path)` when neither is provided

### Phase 6: Merge dead_symbols + dead into find_dead_code (30 min)

The `find_dead_code` tool already accepts `path` and `functions_only`. The `dead` tool is a summary variant. Merge:
- `format=summary` → returns the old `dead` summary format
- `format=list` (default) → returns the old `dead_symbols` structured list

### Phase 7: Trim token usage (30 min)

1. Reduce find_references context lines from 3+3 to 1+1
2. Cap hits at 15 (from 20)
3. Remove plan tool entirely (saves ~3 calls × 2k bytes)
4. Compact call_graph output: qualified names only, no source preview by default
5. Compact describe output: signature + callers only, no full pack

### Phase 8: Update system prompt (30 min)

Replace the 19-tool list with 7 tools + routing guide. The prompt should be SHORT:

```
You have 7 tools for code intelligence:

1. reliary_search(query) — find files by name or topic
2. reliary_find_references(name) — find all usages of a symbol
3. reliary_goto_def(name, anchor_file, anchor_line) — jump to definition
4. reliary_call_graph(name) — who calls X? what does X call?
5. reliary_list_methods(name) — list methods on a type
6. reliary_find_dead_code(path) — find unused functions
7. reliary_describe(name) — explain a symbol (purpose, signature, callers, methods)

TOOL SELECTION:
- "where is X defined?" → goto_def
- "find usages of X" → find_references
- "who calls X?" / "what does X call?" → call_graph
- "list methods on X" → list_methods
- "find dead code" → find_dead_code
- "explain X" → describe
- "find files about topic" → search
```

### Phase 9: Update harness (30 min)

Update multi_turn_harness.py:
- Remove plan tool from RELIARY_TOOLS
- Remove with_source, type_flow, boltzmann from RELIARY_TOOLS
- Rename tools to match new names
- Update RELIARY_SYS system prompt
- Remove dead code for old tools

### Phase 10: Re-index, test, bench, judge (1 hour)

1. Re-index tokio
2. Run unit tests (verify no regressions)
3. Run 3-way bench (A, B, C, seeds 42+17)
4. Run LLM judge
5. Compare to V26b

## Expected outcome

| Metric | V26b | V27 target | Why |
|--------|------|-----------|-----|
| Judge score | 11.0 | **16-20** | Model uses right tool per question |
| Dead-ends | 12-16 | **2-4** | Model doesn't dead-end on wrong tool |
| Tool calls | 29 | **15-20** | Fewer failed attempts |
| WC | 190k | **100-130k** | Fewer calls + trimmed output |
| Billed cost | 35.5k | **20-25k** | Fewer calls + cache stays intact |
| Tool bytes | 12.6k | **8-10k** | Trimmed context + fewer calls |

## What NOT to do (anti-fitting rules)

- No tokio-specific tool hints
- No hardcoded file paths in system prompt
- No benchmark-specific output formatting
- No per-query tool routing (general routing only)
- No manual DB inserts
- No threshold tuning for specific repos

## Grammar-free verification

All 7 tools are grammar-free:
- search: BM25 on tokenized phrases
- find_references: occurrence table + type_flow (structural, no AST)
- goto_def: file_meta + occurrence lookup
- call_graph: brace_graph extraction (structural, no AST)
- list_methods: scan for impl blocks (structural, no AST)
- find_dead_code: cross-reference occurrence table
- describe: pack_query + brace_graph + occurrence

Zero keywords. Zero AST. Zero language detection.

## Effort

| Phase | Time |
|-------|------|
| 1. Rename tools | 30 min |
| 2. Remove redundant tools | 30 min |
| 3. Merge find_references variants | 1 hour |
| 4. Merge call_graph variants | 1 hour |
| 5. Merge pack+plan into describe | 1 hour |
| 6. Merge dead variants | 30 min |
| 7. Trim token usage | 30 min |
| 8. Update system prompt | 30 min |
| 9. Update harness | 30 min |
| 10. Re-index, test, bench, judge | 1 hour |
| **Total** | **~7 hours** |