# Reliary — Agent Usage Guide

This file tells an LLM coding agent when and how to use reliary's MCP tools.

## When to use reliary

Use `reliary_*` tools for code intelligence questions instead of grep/read
cycles. reliary is grammar-free (no AST, no tree-sitter), works on every
language, and answers most symbol questions in a single call.

## Tool selection guide

| Question | Tool |
|----------|------|
| "find references to X" | `reliary_find_references(name="X")` |
| "where is X defined?" | `reliary_find_references(name="X", def_only=true)` |
| "who calls X?" / "callers of X" | `reliary_find_references(name="X", usage_only=true)` |
| "what does X call?" / "helpers used by X" | `reliary_call_graph(name="X", direction="outbound")` |
| "find files about topic Y" | `reliary_search(query="Y")` |
| "find unused code" | `reliary_find_dead_code(path="...")` |
| "what methods does Type X have?" | `reliary_list_methods(name="X")` |
| "which types implement trait T?" / "what derives T?" | `reliary_find_references(name="T")` |
| "explain X" | `reliary_describe(name="X")` |
| "is `sym at file:line` correct?" | `reliary_verify(text="sym at file:line")` |

Use `search` when you do not know the exact symbol name. Use
`reliary_find_references(dead_only=true, path="...")` as an alias for
`reliary_find_dead_code`, and `methods=true` as an alias for `list_methods`.

Every tool response ends with a freshness stamp `[idx:xxxxxxxx]`. The stamp
changes only when the index is rebuilt or a file is reindexed. If you edited a
file and the stamp did not change, the index may be stale — run `reliary trust .`
or `reliary reindex-file <path>`.

**Only call tools from the list above.** The default menu exposes these six plus
`reliary_verify`. Other research variants exist as dispatch targets but are not
listed; do not guess their names.

## Efficiency

- **Stop when answered.** If a result already answers the question, answer
  immediately; do not call another tool to confirm it.
- **Prefer one call over several.** `find_references` alone answers most
  questions through its modes. Reach for `describe` or `call_graph` only when
  you need their specific output.
- **Cite items, skip prose.** For list questions (callers, methods, fields,
  dead code), answer with `name at file:line` per item. Add descriptions only
  when asked what something does.
- **Do not re-query with a different spelling.** If a lookup returns "no
  matches", use the closest-symbols suggestion from the result.

## When NOT to use reliary

- **Plain file reads** — use `read` directly.
- **Recent file changes** — use `git diff`.
- **Binary files** — reliary indexes source text only.

## Indexing

Before any query, the project must be indexed. Run `reliary trust <path>` once
per project. Indexing takes seconds for most repos.

## Compressing verbose output

Prefix verbose shell commands with `reliary wrap`:

```bash
reliary wrap cargo test
reliary wrap git diff
reliary wrap grep "pattern" .
```

Measured mean 31.6% byte reduction on the RTK comparison fixtures, concentrated
on repeated and ANSI-heavy output, with a hard no-inflation guarantee (raw bytes
are emitted whenever compression would be longer). Content readers on source
files (`cat`/`head`/`tail`/`less`/`bat <source.rs>`) pass through unchanged.

For automatic interception, set `RELIARY_SIFT_BASH=1` and install the hook from
`hooks/` for your agent (Pi: `gate.js`; Claude Code:
`hooks/claude-pretooluse.sh`; OpenCode: `hooks/opencode-reliary-sift.js`).

## Benchmarks

Accuracy and cost numbers, the edit-outcome result, and the zero-cost cassette
replay are in [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## License

MIT
