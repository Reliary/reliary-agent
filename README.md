# Reliary

Grammar-free code intelligence and bash compression for AI coding agents.

A single Rust binary that indexes any codebase through structural detection —
no tree-sitter, no AST, no per-language code — and exposes the index to agents
over the Model Context Protocol (MCP). The companion command `reliary wrap`
compresses verbose shell output (`cargo test`, `git diff`, `pytest`) before it
reaches agent context.

**What it is for.** Answering "where is X defined", "who calls X", "what methods
does X have" in one small tool call instead of a grep-then-read cycle. On the
benchmark it answers at ~42% lower billed token cost than grep with about four
times smaller tool output, at accuracy comparable to grep and clearly ahead of
`codebase-memory-mcp` (the other MCP index tested).

## Quick start

```bash
# Index a project
reliary trust .

# Start the MCP server (Claude Code, OpenCode, Pi, Cline, anything MCP)
reliary mcp
```

### Agent integration

`reliary init` detects installed agents and offers to wire itself in. To do it
by hand:

**Claude Code** (`~/.claude.json`):
```json
{
  "mcpServers": {
    "reliary": { "command": "/path/to/reliary", "args": ["mcp"] }
  }
}
```

**OpenCode** (`~/.config/opencode/opencode.json`; on macOS
`~/Library/Application Support/opencode/opencode.json`):
```json
{
  "mcp": {
    "reliary": { "command": "/path/to/reliary", "args": ["mcp"] }
  }
}
```

**Cline** (`cline_mcp_settings.json`): same shape as the Claude entry.

**Pi Agent**: `reliary init` installs the Pi extension (`gate.js`), which routes
tool calls through reliary.

### Bash compression

Prefix a command with `reliary wrap` to compress its output:

```bash
reliary wrap cargo test    # collapses "Compiling" lines, keeps errors + summary
reliary wrap git diff      # drops context lines, keeps hunk headers + changes
reliary wrap pytest -v     # collapses passing lines to a summary
```

For automatic interception with no manual prefix, set `RELIARY_SIFT_BASH=1` and
install the hook from `hooks/` for your agent.

## Tools

The default MCP menu exposes six tools plus one verifier:

| Tool | What it returns |
|------|-----------------|
| `reliary_find_references` | Entry point for symbol questions. `def_only=true` → where X is defined; `usage_only=true` → who calls X; `methods=true` → methods on X; `dead_only=true` (+`path`) → unused code; `path_filter='io/util/'` → scope to a module; no flags → general references. One-line answer with raw code evidence. |
| `reliary_search` | BM25 file search with definition-first ranking; never returns empty. Use when you do not know the exact symbol name. |
| `reliary_call_graph` | Callers and callees with source. `direction=inbound/outbound/both`, `depth`. |
| `reliary_list_methods` | Methods declared on a type, with file:line. |
| `reliary_find_dead_code` | Unused functions, path-scoped. |
| `reliary_describe` | Symbol overview: purpose, signature, callers, methods. |
| `reliary_verify` | Check a `symbol at file:line` claim against the index. |

Every response ends with a freshness stamp `[idx:xxxxxxxx]` that changes only
when the index changes.

## CLI

```
reliary trust [PATH]      Index a project directory
reliary index [PATH]      Build or refresh the index
reliary search QUERY      BM25 search
reliary mcp               Start the MCP server on stdio
reliary wrap [CMD]        Run CMD and compress its output
reliary sift [--stdin]    Pipe text through the compressor
reliary dead [PATH]       Cross-file dead-code analysis
reliary verify CLAIM      Verify a claim about the codebase
reliary init              Auto-install agent integrations
reliary uninstall         Remove agent integrations
reliary doctor            Health check
reliary status            Index and integration status
reliary update            Self-update from GitHub releases (checksum-verified)
reliary completions SHELL Emit a shell completion script
reliary man               Emit the man page
```

Run `reliary --help` for the complete, current list.

## Key properties

- **Grammar-free**: no tree-sitter, no AST, no per-language code. Structural
  detection on brace/indent/blank-line boundaries works on any text file.
- **Index-first**: once trusted, queries use the per-project SQLite index at
  `.reliary/index.sqlite`.
- **Deterministic**: identical inputs produce identical output, so results are
  safe for provider-side KV caching.
- **Standalone**: one Rust binary, no Python or Node runtime, no daemon.

## Benchmarks

Measured with a deterministic claim-verification harness on a 10-question
corpus, 4 seeds, `deepseek-v4-flash`. reliary's F1 leads the alternative MCP
backend clearly and grep narrowly; the reliary-vs-grep gap is within noise on
this single small corpus, so accuracy is presented as comparable to grep. The
reproducible wins are cost, output size, and zero dead-ends.

| | F1 | Billed cost | Tool bytes | Dead-ends |
|---|---|---|---|---|
| reliary | 0.947 | 21,459 | 8.8K | 0.0 |
| codebase-memory-mcp | 0.461 | 23,064 | 13.8K | 3.5 |
| grep | 0.615 | 36,967 | 33.3K | 0.0 |

Full methodology, the edit-outcome result (a tie — no demonstrated advantage),
the familiarity experiment, and a zero-cost cassette replay are in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## License

MIT
