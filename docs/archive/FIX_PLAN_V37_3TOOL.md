# V37: 3-Tool Surface — Simplify Interface, Not Functionality

## Goal

Reduce judge variance by reducing the tool surface from 7 to 3. The model picks the wrong tool 30% of the time with 7 options. With 3 options, the choice is trivially correct.

## Principle

**Zero functionality reduction.** Every Rust function stays. We just stop exposing them as separate MCP tools and route through 3 entry points with params.

## Current 7 tools → Target 3 tools

| Current tool | Merged into | Param to restore original behavior |
|-------------|-------------|-------------------------------------|
| `goto_def` | `find_references` | `def_only=true` — returns only is_def=1 hits |
| `call_graph` | `find_references` | `usage_only=true` — returns only is_def=0 hits (callers) |
| `list_methods` | `describe` | `methods=true` — calls find_methods_on internally |
| `find_dead_code` | `describe` | `dead_only=true` — calls dead_symbols internally |
| `search` | `search` | Unchanged |
| `find_references` | `find_references` | Unchanged (add `def_only` and `usage_only` params) |
| `describe` | `describe` | Unchanged (add `methods` and `dead_only` params) |

## The 3 tools

### 1. `reliary_search(query)`
Unchanged. BM25 full-text search. For "find files about topic".

### 2. `reliary_find_references(name, path_filter?, def_only?, usage_only?)`
The unified symbol lookup tool.

| Param | Values | Effect |
|-------|--------|--------|
| `name` | string (required) | Symbol name to search for |
| `path_filter` | string (optional) | Restrict to module path prefix (e.g. "io/util/") |
| `def_only` | bool (optional) | Only return definitions (is_def=1). Replaces goto_def. |
| `usage_only` | bool (optional) | Only return call sites (is_def=0). Replaces call_graph(inbound). |
| `limit` | int (optional, default 12) | Max hits |

When `def_only=true`:
- Query occurrence table WHERE is_def=1
- Apply centrality ranking (same as current top_candidate_definitions)
- Return "The answer is: X is defined at file:line"

When `usage_only=true`:
- Query occurrence table WHERE is_def=0
- Apply test file exclusion
- Return "The answer is: X is called from: [list]"

When neither is set:
- Return all hits (current behavior)
- Synthesized output when path_filter is active

### 3. `reliary_describe(name, methods?, dead_only?)`
The unified symbol description tool.

| Param | Values | Effect |
|-------|--------|--------|
| `name` | string (required) | Type or symbol name |
| `methods` | bool (optional) | List methods on the type. Replaces list_methods. |
| `dead_only` | bool (optional) | Find dead code in the module. Replaces find_dead_code. |
| `path` | string (optional) | Module path for dead_only (e.g. "io/util") |

When `methods=true`:
- Call find_methods_on(name) internally
- Return "The answer is: methods on Type: [list]"

When `dead_only=true`:
- Call dead_symbols(path) internally
- Return "The answer is: N dead items in path: [list]"

When neither is set:
- Call pack_query(name) — current describe behavior
- Return struct definition + purpose

## System prompt (V37)

```
You have 3 tools:

1. search(query) — Find files/symbols by topic. BM25 search.
2. find_references(name, def_only?, usage_only?, path_filter?) — Find symbol usages.
   - def_only=true → "where is X defined" (returns definitions only)
   - usage_only=true → "who calls X" (returns call sites only)
   - path_filter="io/util/" → restrict to a module
3. describe(name, methods?, dead_only?) — Explain a symbol or find dead code.
   - methods=true → "list methods on Type X"
   - dead_only=true → "find dead code in module X"

TOOL SELECTION:
- "where is X defined?" → find_references(name=X, def_only=true)
- "who calls X?" → find_references(name=X, usage_only=true)
- "find implementations in module X" → find_references(name=X, path_filter="X/")
- "list methods on Type X" → describe(name=X, methods=true)
- "find dead code" → describe(name=".", dead_only=true, path="module")
- "explain X" → describe(name=X)
- "search for files about topic" → search(query="topic")

RULES:
- When tool output contains "The answer is:", QUOTE that line verbatim in your final answer.
- Your final answer MUST match the quoted tool output. Do NOT add types, files, or line numbers not in the tool output.
- Your training data may be from a different version — trust the tool output over your training data.
- Answer in 1-2 tool calls per question.
```

## Implementation steps

### Step 1: Add `def_only` param to find_references (30 min)
- Add `def_only` to tool schema
- When `def_only=true`, filter hits to is_def=1 only
- Apply centrality ranking (reuse top_candidate_definitions query)
- Output: "The answer is: X is defined at file:line"

### Step 2: Add `usage_only` param to find_references (30 min)
- Add `usage_only` to tool schema
- When `usage_only=true`, filter hits to is_def=0 only
- Apply test file exclusion
- Output: "The answer is: X is called from: [list]"

### Step 3: Add `methods` param to describe (30 min)
- Add `methods` to tool schema
- When `methods=true`, call find_methods_on(name) internally
- Output: "The answer is: methods on Type: [list]"

### Step 4: Add `dead_only` param to describe (30 min)
- Add `dead_only` and `path` to tool schema
- When `dead_only=true`, call dead_symbols(path) internally
- Output: "The answer is: N dead items in path: [list]"

### Step 5: Update PRIMARY_TOOLS to 3 tools (15 min)
- Remove: goto_def, call_graph, list_methods, find_dead_code
- Keep: search, find_references, describe

### Step 6: Update aliases (15 min)
- `reliary_goto_def` → alias to `find_references(def_only=true)`
- `reliary_call_graph` → alias to `find_references(usage_only=true)` (for outbound, keep call_graph as internal)
- `reliary_list_methods` → alias to `describe(methods=true)`
- `reliary_find_dead_code` → alias to `describe(dead_only=true)`

### Step 7: Update system prompt (15 min)
- Replace 7-tool prompt with 3-tool prompt
- Update TOOL SELECTION to show param-based routing

### Step 8: Update Python harness (30 min)
- Update RELIARY_TOOLS dict to 3 tools
- Update RELIARY_SYS system prompt
- Update tool wrappers

### Step 9: Build, test, bench (1 hour)
- Build release
- Run tests
- Re-index tokio
- Run 3-way bench (A, B, C) with 2 seeds
- Run LLM judge

## What stays (Rust code)

All Rust functions remain unchanged:
- `goto_def()` — called internally by find_references when def_only=true
- `build_call_graph()` — called internally for call chain queries
- `find_methods_on()` — called internally by describe when methods=true
- `dead_symbols()` — called internally by describe when dead_only=true
- `top_candidate_definitions()` — called internally for centrality ranking
- `build_callers()` — called internally for usage_only queries

No Rust code deleted. No functionality lost. Just MCP tool consolidation.

## Expected impact

| Metric | V36 | V37 target |
|--------|-----|-----------|
| Judge | 10.5 | **16-20** |
| Variance | ±3 | **±1-2** |
| WC | 127k | **100-130k** |
| Dead-ends | 4 | **2-3** |
| Tool surface | 7 | **3** |

## Grammar-free verification

| Change | Grammar-free? |
|--------|--------------|
| def_only param | Yes — SQL filter on is_def column |
| usage_only param | Yes — SQL filter on is_def column |
| methods param | Yes — calls existing function |
| dead_only param | Yes — calls existing function |
| System prompt | N/A — documentation |
| Tool consolidation | Yes — no new logic, just routing |

## Is this fitting?

**No.** Reducing from 7 to 3 tools is standard API simplification. Any user benefits from fewer, clearer tools. The params restore all original functionality. No benchmark-specific logic.

## Total effort: ~4 hours