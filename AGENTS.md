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
| "find files about topic Y" | `reliary_search` | BM25 file search with definition-first ranking (the file where the symbol is defined ranks above files that merely reference it; tests/bench/docs are demoted). Use when you don't know the exact symbol name. |
| "find unused code" | `reliary_find_dead_code(path="...")` or `reliary_find_references(dead_only=true, path="...")` | Path-scoped dead-code list. |
| "what methods does Type X have?" | `reliary_list_methods(name="X")` or `reliary_find_references(name="X", methods=true)` | Method names with file:line. |
| "which types implement trait T?" / "what derives T?" | `reliary_find_references(name="T")` | Trait implementors: `T is implemented by N types: ...` with file:line. |
| "explain X" | `reliary_describe(name="X")` | Purpose, signature, callers, methods — plus an `at a glance` block (definition, top callers, tests, risk). |
| "find code like X" | `reliary_similar(name="X")` | Near-clone detection. |

Every tool response ends with a freshness stamp `[idx:xxxxxxxx]`. The stamp changes only when the index is rebuilt or a file is reindexed — it is stable across reads and JIT builds. If you see the same symbol with two different stamps, the second read is fresher; if you edited a file and the stamp did not change, the index may be stale (run `reliary reindex-file <path>` or `reliary trust .`).

**Do not call tools that are not in the tool list above.** The default menu exposes 6 tools (`search`, `find_references`, `call_graph`, `list_methods`, `find_dead_code`, `describe`) plus `verify`. `goto_def` and `similar` remain dispatchable but are hidden unless `RELIARY_FULL_MENU=1`. Specialist research variants exist as dispatch targets but are not listed — do not guess their names.

## Efficiency

- **Stop when answered.** If a tool result already answers the question, answer immediately. Do not call another tool to confirm what you were just told.
- **Prefer one call over several.** `find_references` alone answers most questions via its modes (`def_only`, `usage_only`, `methods`, `dead_only`). Reach for `describe`/`call_graph` only when the question needs their specific output.
- **Cite items, skip prose.** When the question asks for a list (callers, methods, fields, dead code), answer with `name at file:line` per item. Add descriptions only if the question asks what something does.
- **Do not re-query with a different spelling.** If a symbol lookup returns "no matches", use the closest-symbols suggestion from the result instead of searching again.

## When NOT to use reliary

- **Plain file reads** — use `read` directly. reliary reads happen via tool calls.
- **Recent file changes** — use `git diff` for that, not reliary.
- **Binary files** (images, compiled binaries) — reliary only indexes source text.

## Indexing

Before any `reliary_*` query, the repo must be indexed. Run `reliary trust <path>` once per project. Indexing takes seconds for most repos.

## Performance vs altbackend vs grep

Deterministic claim verification (F1 = how many model claims `symbol at file:line` verify against the index; 4 seeds 42/17/123/456 on a reliary corpus snapshot; repro in README):

| Backend | F1 | Precision | Billed | Dead-ends | Wall (median) |
|---------|----|-----------|--------|-----------|------|
| reliary8 | **0.782** | **0.780** | **24,143** | **0.0** | 43s |
| altbackend | 0.322 | 0.339 | 24,657 | 4.2 | 41s |
| grep | 0.383 | 0.423 | 35,180 | 0.0 | 43s |

Wall is provider-latency bound (~90% cache hit on all three conditions); the spread is within noise. reliary's edge is F1 (2× grep, 2.4× altbackend) at the lowest billed cost and zero dead-ends. A prompt-parity ablation (condition `M`, ~120-word minimal prompt vs A's ~300-word shipped prompt) scored F1 0.707 vs A's 0.816 — the tool contributes the majority of the gap.

Do not cite the keyword-score rubric (`/30`) as an accuracy measure: it is substring
matching and roughly doubles the real accuracy. Use F1 above.

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

## Available tool surface (6 tools in the default menu)

**Symbol queries** (`reliary_find_references` is the entry point for most of these; the others are aliases kept for convenience):
- `reliary_find_references` — def_only / usage_only / methods / dead_only / path_filter modes
- `reliary_call_graph` — callers/callees, direction in/out/both, depth
- `reliary_list_methods` — methods on a type
- `reliary_find_dead_code` — unused code, path-scoped
- `reliary_describe` — symbol overview; `methods`/`dead_only` route to the same handlers

**File queries**:
- `reliary_search` — BM25 file search, definition-first ranking (never returns empty)

**Verification**:
- `reliary_verify` — check a `symbol at file:line` claim against the index

Hidden but dispatchable (set `RELIARY_FULL_MENU=1` to expose): `reliary_goto_def` (deprecated — use `def_only=true`), `reliary_similar` (near-clone detection).

## Compressing tool output

For bash commands with verbose output (`cargo test`, `git diff`, `grep pattern .`), prefix with `reliary wrap`:

```
reliary wrap cargo test
reliary wrap git diff
reliary wrap grep "pattern" .
```

This pipes output through reliary's universal compressor before it reaches context.
Measured on the 20 non-trivial RTK comparison fixtures (`~/src/sift/scripts/bench_vs_rtk.py`): mean 31.6%, median 3.9% byte reduction, concentrated on repeated/ANSI-heavy output and zero on short dense output. Hard no-inflation guarantee: raw bytes are emitted whenever compression would be longer. Works on ANY command in ANY language.
No cache bust — the LLM builds reasoning on compressed text from the start (rtk pattern).
Content readers on source files (`cat`/`head`/`tail`/`less`/`bat <source.rs>`) pass through uncompressed.

For automatic interception (no manual prefix needed), set `RELIARY_SIFT_BASH=1` and install the appropriate hook:
- **Pi**: gate.js handles it automatically when `RELIARY_SIFT_BASH=1`
- **Claude Code**: `hooks/claude-pretooluse.sh` in `~/.claude/hooks/`
- **OpenCode**: `hooks/opencode-reliary-sift.js` in `~/.opencode/plugins/`

## License

MIT