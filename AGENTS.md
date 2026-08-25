# reliary8 — Agent Usage Guide

This file tells an LLM coding agent when and how to use reliary8's MCP tools.

## When to use reliary

Use `reliary_*` tools for code intelligence questions instead of grep/read cycles. reliary is grammar-free (no AST, no tree-sitter), works on every language, and is faster than grep on indexed repos.

## Tool selection guide

| Question | Tool | Why |
|----------|------|-----|
| "find references to X" | `reliary_find_references_with_source` | Returns file:line + actual source text per hit. Type-aware — separates BufWriter::consume from Take::consume. |
| "where is X defined?" | `reliary_goto_def` | Jumps from a usage line to the definition site. |
| "who calls X?" / "what does X call?" | `reliary_callgraph` | Bidirectional call graph rooted at the anchor. |
| "find files about topic Y" | `reliary_search` | BM25 full-text search. Use when you don't know the exact symbol name. |
| "find unused code" | `reliary_dead_symbols` or `reliary_dead` | Two variants: ranked summary (dead) vs structured list (dead_symbols). |
| "show structure of file F" | `reliary_brace_graph` | Tree of fn/method/block nesting with role tags. |
| "match expression patterns" | `reliary_query_ast` | REFAL-like patterns: `Call(_, _)`, `BinaryOp(?op, ?a, ?b)`. |
| "before editing file F, what's affected?" | `reliary_risk` | Risk score + dependent symbols. |
| "edit a specific function/block" | `reliary_fix` | Pattern-based edit, survives formatting changes. |
| "what did we do last time on this repo?" | `reliary_prior` | Cross-session memory. |
| "compress my reasoning text" | `reliary_compress` | Strips filler, merges redundant thinking. |

## When NOT to use reliary

- **Plain file reads** — use `read` directly. reliary reads happen via tool calls.
- **Recent file changes** — use `git diff` for that, not reliary.
- **Binary files** (images, compiled binaries) — reliary only indexes source text.

## Indexing

Before any `reliary_*` query, the repo must be indexed. Run `reliary trust <path>` once per project. Indexing takes seconds for most repos.

## Performance vs altbackend vs grep

Apples-to-apples benchmark on tokio (5 questions, deepseek-chat):

| Backend | Jaccard (median) | Notes |
|---------|------------------|-------|
| reliary8 | **0.299** | type-flow + inline source |
| altbackend | 0.000 | LLM doesn't know to call get_code_snippet without hints |
| grep | 0.154 | lowest prompt cost, no type awareness |

altbackend's "99.2% reduction" claim depends on giving the LLM a per-question playbook hint. Without that, altbackend's tools alone don't help the LLM find references.

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

### Find references to a method
```
reliary_find_references_with_source(
  name="consume",
  anchor_file="io/util/take.rs",
  anchor_line=121,
  path="/tmp/tokio-corpus/tokio/src"
)
```
Returns hits with `(file, line, similarity, source)`. Each hit shows the actual line of code, so you can verify the role match without follow-up calls.

### Get definition of a symbol
```
reliary_goto_def(
  name="Waker",
  anchor_file="runtime/task/waker.rs",
  anchor_line=42,
  path="/tmp/tokio-corpus/tokio/src"
)
```

### Call graph
```
reliary_callgraph(
  name="spawn",
  anchor_file="runtime/handle.rs",
  anchor_line=15,
  path="/tmp/tokio-corpus/tokio/src"
)
```

### Match expression patterns
```
reliary_query_ast(
  pattern="BinaryOp(?op, ?a, ?b)",
  file="io/util/buffer.rs",
  max_results=20
)
```

## Available tool surface (19 tools in primary menu)

**Symbol queries**:
- `reliary_find_references_with_source` — find-references with inline source
- `reliary_find_references` — find-references (file:line only)
- `reliary_find_references_type_flow` — type-flow variant
- `reliary_find_references_boltzmann` — with probability scores
- `reliary_goto_def` — definition lookup
- `reliary_callgraph` — call graph
- `reliary_scope` — symbol scope
- `reliary_dead_symbols` — unused symbols

**File queries**:
- `reliary_search` — BM25 file search
- `reliary_brace_graph` — structural tree
- `reliary_call_graph` — file-local call graph
- `reliary_query_ast` — expression pattern matching

**Editing**:
- `reliary_risk` — pre-edit risk
- `reliary_fix` — pattern-based edit

**Memory / compression**:
- `reliary_compress` — text compression
- `reliary_prior` — cross-session memory
- `reliary_retrieve` — content cache lookup
- `reliary_stats` — statistics
- `reliary_dead` — dead code summary

To see all 62 specialist tools (research variants), set `RELIARY_FULL_MENU=1` before starting the MCP server.

## Compressing tool output

For bash commands with verbose output (`cargo test`, `git diff`, `grep pattern .`), prefix with `reliary wrap`:

```
reliary wrap cargo test
reliary wrap git diff
reliary wrap grep "pattern" .
```

This pipes output through reliary's universal compressor before it reaches context.
Saves 30-60% of tokens on tool output. Works on ANY command in ANY language.
No cache bust — the LLM builds reasoning on compressed text from the start (rtk pattern).

For automatic interception (no manual prefix needed), set `RELIARY_SIFT_BASH=1` and install the appropriate hook:
- **Pi**: gate.js handles it automatically when `RELIARY_SIFT_BASH=1`
- **Claude Code**: `hooks/claude-pretooluse.sh` in `~/.claude/hooks/`
- **OpenCode**: `hooks/opencode-reliary-sift.js` in `~/.opencode/plugins/`

## License

MIT