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

8 curated tools in the primary menu:

| Tool | What it returns |
|------|----------------|
| `reliary_find_references` | Single entry point for all symbol questions. Modes: `def_only=true` → "where is X defined"; `usage_only=true` → "who calls X"; `methods=true` → "list methods on X"; `dead_only=true` (+`path`) → "find dead code"; `path_filter='io/util/'` → scope to a module; no params → general references. One-line answer with raw code evidence. |
| `reliary_search` | BM25 file search: definition-first ranking, never-empty fallback (closest files by vocabulary similarity). |
| `reliary_goto_def` | Deprecated — use `reliary_find_references(name=X, def_only=true)`. Still works. |
| `reliary_call_graph` | Callers + callees with source. `direction=inbound/outbound/both`, `depth` (entry points auto-expand). |
| `reliary_list_methods` | Methods on a type with file:line. Alias of the same handler. |
| `reliary_find_dead_code` | Unused functions, path-scoped. Alias of the same handler. |
| `reliary_describe` | Symbol overview: purpose, signature, callers, methods. `methods`/`dead_only` route to the same handlers. |
| `reliary_similar` | Structurally similar functions (near-clone detection). |

### Bash compression (reliary wrap)

Universal text compressor that works on ANY command output. No per-command filters needed. 46.3% average compression across the 6 fixtures in the V14 benchmark (see `~/src/sift/scripts/bench_vs_rtk.py`). Content readers on source files (`cat`/`head`/`tail`/`less`/`bat <source.rs>`) pass through uncompressed so model-built edits never see mangled code.

## Benchmarks

Deterministic claim-verification bench (10 questions on a reliary corpus snapshot, deepseek-v4-flash, 4 seeds 42/17/123/456, result file `bench/results/v70_3way.jsonl`):

| Metric | Reliary (A) | Altbackend (B) | Grep (C) |
|--------|-------------|----------------|----------|
| F1 (claim-weighted) | **0.809** | 0.346 | 0.414 |
| Precision | **0.835** | 0.397 | 0.511 |
| Recall | **0.785** | 0.308 | 0.355 |
| Keyword score /30 | 26.8 | 25.5 | **27.2** |
| Billed cost | **24,143** | 24,657 | 35,180 |
| Dead-ends | **0.0** | 4.2 | 0.0 |
| Tool calls | 18.2 | 19.8 | **15.5** |
| Tool bytes | **8.1K** | 16.0K | 32.5K |
| Wall (median) | 43s | 41s | 43s |

Every model claim (`symbol at file:line`) is verified mechanically against the index — no LLM judge. Wall is provider-latency bound (~90% cache hit on all three); the three conditions are within noise of each other. Repro: `python3 bench/run_snapshot_bench.py --bin target/release/reliary --corpus /tmp/rel8-corpus --conds A,B,C --seeds 42 17 123 456` then `python3 bench/deterministic_verify.py --input bench/results/v70_3way.jsonl --corpus /tmp/rel8-corpus`.

Prompt-fairness disclosure: condition A receives the shipped routing prompt (~300 words); B/C receive ~100-word prompts. An ablation (`M`, minimal ~120-word prompt) scored F1 0.605 vs A's 0.795 — the tool contributes most of the gap, the routing prompt the remainder. See `docs/plans/V70_TELEPATHY.md`.

Full reproduction: `python3 bench/long_session_bench.py --conditions A,C --seeds 42 17`

## MCP tool menu

The default menu exposes 8 curated tools (`search`, `find_references`, `goto_def`, `call_graph`, `list_methods`, `find_dead_code`, `describe`, `similar`). Specialist research variants (`callgraph_v2`, `methods_on`, `find_references_type_flow`, `find_references_boltzmann`, `trace_path`, `query_ast`, `brace_graph`, `architecture`, `risk`, `fix`, `prior`, `compress`, `retrieve`, `stats`) still exist as dispatch targets but are not listed.

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
