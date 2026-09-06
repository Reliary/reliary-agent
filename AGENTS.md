# reliary8 — Agent Usage Guide

This file tells an LLM coding agent when and how to use reliary8's MCP tools.

## When to use reliary

Use `reliary_*` tools for code intelligence questions instead of grep/read cycles. reliary is grammar-free (no AST, no tree-sitter), works on every language, and is faster than grep on indexed repos.

## Tool selection guide

| Question | Tool | Why |
|----------|------|-----|
| "find references to X" | `reliary_find_references(name="X")` | One-line answer with raw code evidence. Copy it verbatim. |
| "where is X defined?" | `reliary_find_references(name="X", def_only=true)` | Returns the top definition with source code. |
| "who calls X?" / "callers of X" | `reliary_find_references(name="X", usage_only=true)` | Returns call sites only. For the graph view, use `reliary_call_graph(direction="inbound")`. |
| "what does X call?" | `reliary_call_graph(name="X", direction="outbound")` | Callees with source. |
| "find files about topic Y" | `reliary_search` | BM25 file search. Use when you don't know the exact symbol name. |
| "find unused code" | `reliary_find_dead_code(path="...")` or `reliary_find_references(dead_only=true, path="...")` | Path-scoped dead-code list. |
| "what methods does Type X have?" | `reliary_list_methods(name="X")` or `reliary_find_references(name="X", methods=true)` | Method names with file:line. |
| "explain X" | `reliary_describe(name="X")` | Purpose, signature, callers, methods. |
| "find code like X" | `reliary_similar(name="X")` | Near-clone detection. |

**Do not call tools that are not in the tool list above.** The menu exposes 8 tools (`search`, `find_references`, `goto_def` [deprecated], `call_graph`, `list_methods`, `find_dead_code`, `describe`, `similar`). Specialist research variants exist as dispatch targets but are not listed — do not guess their names.

## When NOT to use reliary

- **Plain file reads** — use `read` directly. reliary reads happen via tool calls.
- **Recent file changes** — use `git diff` for that, not reliary.
- **Binary files** (images, compiled binaries) — reliary only indexes source text.

## Indexing

Before any `reliary_*` query, the repo must be indexed. Run `reliary trust <path>` once per project. Indexing takes seconds for most repos.

## Performance vs altbackend vs grep

Deterministic claim verification (F1 = how many model claims `symbol at file:line` verify against the index; 4 seeds 42/17/123/456 on a reliary corpus snapshot; repro in README):

| Backend | F1 | Precision | Billed | Dead-ends | Wall |
|---------|----|-----------|--------|-----------|------|
| reliary8 | **0.642** | **0.986** | **9,574** | **0.0** | fastest |
| altbackend | 0.299 | 0.539 | 39,938 | 5.0 | — |
| grep | 0.686 | 0.904 | 61,322 | 2.2 | — |

## Single-call vs multi-call

reliary's `with_source` collapses the workflow into one call:

```
reliary_find_references_with_source(name, anchor_file, anchor_line, path)
  → file:line + similarity + source code per hit
```

altbackend's equivalent requires two calls:

```
altbackend_search_graph(query=...)
  → qualified_name per hit
altbackend_get_code_snippet(qualified_name=...)
  → source code per qualified_name
```

One call vs two. One round-trip vs two. Half the latency.

## Examples

### Find where ClassifyStructral is defined
```
reliary_find_references(name="classify_structural", def_only=true)
```
Returns the top definition with source code. Copy the `file:line` into your response.

### Get callers of a symbol
```
reliary_find_references(name="classify_structural", usage_only=true)
```

## Available tool surface (8 tools in primary menu)

**Symbol queries** (`reliary_find_references` is the entry point for all of these; the others are aliases kept for convenience):
- `reliary_find_references` — def_only / usage_only / methods / dead_only / path_filter modes
- `reliary_goto_def` — deprecated; use `def_only=true` instead
- `reliary_call_graph` — callers/callees, direction in/out/both, depth
- `reliary_list_methods` — methods on a type
- `reliary_find_dead_code` — unused code, path-scoped
- `reliary_describe` — symbol overview; `methods`/`dead_only` route to the same handlers
- `reliary_similar` — near-clone detection

**File queries**:
- `reliary_search` — BM25 file search (never returns empty)

## Compressing tool output

For bash commands with verbose output (`cargo test`, `git diff`, `grep pattern .`), prefix with `reliary wrap`:

```
reliary wrap cargo test
reliary wrap git diff
reliary wrap grep "pattern" .
```

This pipes output through reliary's universal compressor before it reaches context.
46.3% average compression across the 6 fixtures in the V14 benchmark (see `~/src/sift/scripts/bench_vs_rtk.py`). Works on ANY command in ANY language.
No cache bust — the LLM builds reasoning on compressed text from the start (rtk pattern).
Content readers on source files (`cat`/`head`/`tail`/`less`/`bat <source.rs>`) pass through uncompressed.

For automatic interception (no manual prefix needed), set `RELIARY_SIFT_BASH=1` and install the appropriate hook:
- **Pi**: gate.js handles it automatically when `RELIARY_SIFT_BASH=1`
- **Claude Code**: `hooks/claude-pretooluse.sh` in `~/.claude/hooks/`
- **OpenCode**: `hooks/opencode-reliary-sift.js` in `~/.opencode/plugins/`

## License

MIT