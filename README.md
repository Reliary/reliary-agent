# Reliary8

Grammar-free code intelligence + bash compression for AI coding agents.

A single Rust binary (~7 MB) that indexes any codebase via structural detection (no tree-sitter, no language-specific code) and exposes it to AI agents through the Model Context Protocol (MCP). Companion tool `reliary wrap` compresses verbose bash output (cargo test, git diff, pytest) before it reaches agent context — an open-source, grammar-free alternative to RTK.

## Quick start

```bash
# Index your project
reliary trust .

# Wire into an AI agent (Claude Code, OpenCode, Pi, anything that speaks MCP)
reliary mcp
```

### Agent integration

Add to your agent's MCP config:

**Claude Code** (`~/.claude/mcp.json`):
```json
{ "mcpServers": { "reliary": { "command": "/path/to/reliary", "args": ["mcp"] } } }
```

**Pi Agent**: `pi install --tool "reliary mcp"`

**OpenCode** (`~/.config/opencode/config.json`): same format.

### Bash compression (RTK parity)

For verbose shell output, prefix with `reliary wrap`:
```bash
reliary wrap cargo test    # collapses 12 Compiling lines → 1, keeps errors + summary
reliary wrap git diff      # drops context lines, keeps hunk headers + changes
reliary wrap pytest -v     # collapses 40 passed lines → [40 passed]
```

For automatic interception (no manual prefix), set `RELIARY_SIFT_BASH=1` and install the appropriate hook from `hooks/`.

## What it does

### Code intelligence (MCP tools)

| Tool | What it returns |
|------|----------------|
| `reliary_find_references_with_source` | file:line + source text per reference. Type-aware: distinguishes BufWriter::consume from Take::consume. |
| `reliary_goto_def` | Immediately jump to definition with anchor_file+anchor_line for follow-up queries. |
| `reliary_callgraph` | Callers + callees from function body scanning. |
| `reliary_search` | BM25 full-text search across the index. |
| `reliary_dead_symbols` | Unused functions with file:line, path-scoped. |
| `reliary_fix` | Pattern-based edit within a function body. |
| `reliary_risk` | Pre-edit risk score + dependent symbols. |

### Bash compression (reliary wrap)

Universal text compressor that works on ANY command output. No per-command filters needed. Achieves 30-80% compression on real shell output depending on content structure.

## Benchmarks

10-query multi-turn session on tokio, deepseek-v4-flash, 2 seeds:

| Metric | Reliary | Grep-only | Δ |
|--------|---------|-----------|---|
| Score | 23-24/30 | 23-25/30 | Tied |
| WC (input + 4× output) | ~130k | ~240k | -46% |
| Tokens in | ~124k | ~229k | -46% |
| Tool bytes | ~10k | ~31k | -68% |
| Dead-ends | 8 | 4 | +4 |

Reliary uses 46% fewer tokens than grep for the same score. Tool output is 68% smaller because structured results replace raw file dumps.

Full reproduction: `python3 bench/long_session_bench.py --conditions A,C --seeds 42 17`

## MCP tool menu

The default menu exposes 16 curated tools. Set `RELIARY_FULL_MENU=1` for all variants.

## CLI reference

```
reliary trust [PATH]          Index a project directory
reliary mcp                   Start MCP server on stdio
reliary wrap [CMD]            Run CMD and compress its output
reliary sift [--stdin]        Pipe text through compressor
reliary dead [PATH]           Find dead code (cross-file, carrion-style)
reliary search QUERY          BM25 search
reliary who-calls FILE IDENT  Find callers and callees
reliary risk FILE             Pre-edit risk analysis
reliary fix FILE [OLD] [NEW]  Apply pattern-based fix
reliary init                  Auto-install agent integrations
reliary doctor                Health check
reliary clean                 Clean caches and state
```

## Key properties

- **Grammar-free**: no tree-sitter, no AST, no per-language code. Structural detection on brace/indent/blank-line boundaries works on any text file including Markdown, TOML, configs.
- **Index-first**: once trusted, all queries use the SQLite index (`.reliary/index.sqlite` per project). ~10s for most repos, ~80s for Linux kernel.
- **Deterministic**: identical inputs produce identical outputs (verified: 9 determinism tests). Cache-safe for provider-side KV caching.
- **Standalone binary**: single Rust binary, no Python/Node runtime, no daemon process.

## License

MIT
