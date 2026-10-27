# Reliary — Agent Usage Guide

This file tells an LLM coding agent when and how to use reliary's MCP tools.

## When to use reliary

Use `reliary_*` tools for code intelligence questions instead of grep/read
cycles. reliary is grammar-free (no AST, no tree-sitter), works on every
language, and answers most symbol questions in a single call.

Nix note: attribute paths are separate tokens, so ask for the leaf
(`def_only("git")`, not `"programs.git"`). Nix indented strings are NOT
modelled, so shell code inside a module doc still indexes as code.

Ruby note: `attr_reader :x` / `define_method(:x) do` do define `x`, but only
the first symbol on a multi-symbol `attr_accessor` line. `attr_*` with a
receiver (`self.attr_reader`) is not a declaration.

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
file and the stamp did not change, the index may be stale — run
`reliary reindex-file <path>` (one file) or `reliary index .` (whole project).

**Only call tools from the list above.** The default menu exposes these six plus
`reliary_verify`. Two more exist as dispatch targets but are not listed
(`reliary_goto_def`, `reliary_similar`); do not guess other names.

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

## Which project do the tools use?

The tools locate the index themselves; you do not need to start the server from
the project. Resolution order:

1. the `path` argument you pass on the call, if it names a directory;
2. `RELIARY_ROOT`;
3. the workspace root your client advertises (`roots/list`);
4. the server's working directory.

From there they walk up to the nearest `.reliary/` index. So a call from a
subdirectory finds the project above it, and `path="/abs/project"` selects that
project explicitly. If a lookup returns "no index", pass the project directory
as `path` rather than retrying. An empty result with a `path_filter` means
nothing matched inside that scope — not the whole codebase. Oversize MCP input
(over 10MB on one line) is rejected with `-32602`; split the request.

## Indexing

Before any query, the project must be indexed. Run `reliary trust <path>` once
per project (from that directory, or pass the path). Indexing takes seconds for
most repos. `reliary init` can auto-index the project the first time an MCP
client connects to it.

Scripted `init`, `clean`, and index-offer prompts fail closed: on a non-TTY
without `--yes` (or `RELIARY_YES=1`) they exit 2 instead of guessing. A bare
`search` with no index exits 2; an empty result with an index present exits 0.

`.reliary/` is local machine state and is git-ignored automatically; do not add
it to version control, and do not remove `.reliary/.gitignore`.

## Compressing verbose output

Prefix verbose shell commands with `reliary wrap`:

```bash
reliary wrap cargo test
reliary wrap git diff
reliary wrap grep "pattern" .
```

Measured mean 31.6% / median 3.9% byte reduction on the RTK comparison fixtures, concentrated
on repeated and ANSI-heavy output, with a hard no-inflation guarantee (raw bytes
are emitted whenever compression would be longer). Content readers on source
files (`cat`/`head`/`tail`/`less`/`bat <source.rs>`) pass through unchanged.

For automatic interception, `reliary init` installs the hook for your agent
(Pi: `crates/reliary-agent/pi/gate.js`; Claude Code:
`crates/reliary-agent/hooks/claude-pretooluse.sh`). Bash compression
is on by default; set `RELIARY_SIFT_BASH=0` to disable. For OpenCode, `init`
installs the plugin (`~/.local/share/reliary/opencode-plugin.js`) which reindexes files after edits.

## Benchmarks

Accuracy and cost numbers, the edit-outcome result, and the zero-cost cassette
replay are in [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## License

MIT
