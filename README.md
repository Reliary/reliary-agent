# Reliary8

Grammar-free code intelligence + bash compression for AI coding agents.

A single Rust binary (~9 MB) that indexes any codebase via structural detection (no tree-sitter, no language-specific code) and exposes it to AI agents through the Model Context Protocol (MCP). Companion tool `reliary wrap` compresses verbose bash output (cargo test, git diff, pytest) before it reaches agent context — a grammar-free compressor with a hard no-inflation guarantee.

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

6 curated tools in the default menu (the specialist dispatch targets still
exist but are not listed; `goto_def` and `similar` are dispatchable but hidden
unless `RELIARY_FULL_MENU=1`):

| Tool | What it returns |
|------|----------------|
| `reliary_find_references` | Single entry point for all symbol questions. Modes: `def_only=true` → "where is X defined"; `usage_only=true` → "who calls X"; `methods=true` → "list methods on X"; `dead_only=true` (+`path`) → "find dead code"; `path_filter='io/util/'` → scope to a module; no params → general references. One-line answer with raw code evidence. |
| `reliary_search` | BM25 file search: definition-first ranking, never-empty fallback (closest files by vocabulary similarity). |
| `reliary_call_graph` | Callers + callees with source. `direction=inbound/outbound/both`, `depth` (entry points auto-expand). |
| `reliary_list_methods` | Methods on a type with file:line. Alias of the same handler. |
| `reliary_find_dead_code` | Unused functions, path-scoped. Alias of the same handler. |
| `reliary_describe` | Symbol overview: purpose, signature, callers, methods. `methods`/`dead_only` route to the same handlers. |
| `reliary_verify` | Check a `symbol at file:line` claim against the index. |

### Bash compression (reliary wrap)

Universal text compressor for command output — no per-command filters, works on
any language. Measured on the 20 non-trivial fixtures of the RTK comparison
bench (`$HOME/src/sift/scripts/bench_vs_rtk.py`):

- **mean 31.6%, median 3.9%** byte reduction. Compression is concentrated where
  it matters — repeated lines (100%), ANSI noise (97%), `ss -tuln` (89%),
  chaos output (83%), `git status` (72%), `ps aux` (46%) — and **zero or near-zero
  on short/dense output** (compiler errors, `docker ps`, `ip a`).
- **Hard no-inflation guarantee**: `wrap`/`sift` emit the raw bytes whenever
  compression would produce a longer result. The tool never costs you context.

Source-file readers (`cat`/`head`/`tail`/`less`/`bat <source.rs>`) pass through
byte-identical so model-built edits never see mangled code. Full raw output is
recoverable from a content-addressed tee file on the paths that compress.

## Benchmarks

Deterministic claim-verification bench (10 questions on a reliary corpus snapshot, deepseek-v4-flash, 4 seeds 42/17/123/456, result file `bench/results/v70_3way.jsonl`):

| Metric | Reliary (A) | Altbackend (B) | Grep (C) |
|--------|-------------|----------------|----------|
| F1 (claim-weighted) | **0.782** | 0.322 | 0.383 |
| Precision | **0.780** | 0.339 | 0.423 |
| Recall | **0.785** | 0.308 | 0.355 |
| Coverage | **0.93** | 0.85 | 0.82 |
| Keyword score /30 | 26.8 | 25.5 | **27.2** |
| Billed cost | **24,143** | 24,657 | 35,180 |
| Dead-ends | **0.0** | 4.2 | 0.0 |
| Tool calls | 18.2 | 19.8 | **15.5** |
| Tool bytes | **8.1K** | 16.0K | 32.5K |
| Wall (median) | 43s | 41s | 43s |

Every model claim (`symbol at file:line`) is verified mechanically against the index — no LLM judge. Wall is provider-latency bound (~90% cache hit on all three); the three conditions are within noise of each other. Repro: `python3 bench/run_snapshot_bench.py --bin target/release/reliary --corpus /tmp/rel8-corpus --conds A,B,C --seeds 42 17 123 456` then `python3 bench/deterministic_verify.py --input bench/results/v70_3way.jsonl --corpus /tmp/rel8-corpus`.

**Read F1, not the keyword score.** The `Keyword score /30` row is a substring-matching
rubric kept only as a smoke test: it is gameable by keyword-stuffing and inflates
real accuracy roughly 2× (verified against ground-truth F1). The claim-verification
columns above are the meaningful comparison — every `symbol at file:line` the model
states is checked against the index, so invented locations are counted as errors.
Billed cost includes the provider's cache discount (≈94% cache hit on all three
conditions).

Prompt-fairness disclosure: condition A receives the shipped routing prompt (~300 words); B/C receive ~100-word prompts. An ablation (`M`, minimal ~120-word prompt) scored F1 0.707 vs A's 0.816 — the tool contributes most of the gap, the routing prompt the remainder. See `docs/plans/V70_TELEPATHY.md`.

Full reproduction: `python3 bench/long_session_bench.py --conditions A,C --seeds 42 17`

## MCP tool menu

The default menu exposes 6 curated tools (`search`, `find_references`, `call_graph`, `list_methods`, `find_dead_code`, `describe`) plus `reliary_verify`. `goto_def` and `similar` remain dispatchable for backward compatibility but are hidden unless `RELIARY_FULL_MENU=1`. Specialist research variants (`callgraph_v2`, `methods_on`, `find_references_type_flow`, `trace_path`, `query_ast`, `brace_graph`, `architecture`, `risk`, `fix`, `prior`, `compress`, `retrieve`, `stats`) still exist as dispatch targets but are not listed.

## CLI reference

```
reliary trust [PATH]          Index a project directory
reliary index [PATH]          Build/refresh the index
reliary search QUERY          BM25 search
reliary mcp                   Start MCP server on stdio
reliary wrap [CMD]            Run CMD and compress its output
reliary sift [--stdin]        Pipe text through compressor
reliary compress [PATH]       Compress a file or directory
reliary dead [PATH]           Find dead code (cross-file, carrion-style)
reliary risk FILE             Pre-edit risk analysis
reliary fix TASK              Autonomous bug-fix agent (deterministic recipes, LLM fallback)
reliary verify CLAIM          Verify a claim about the codebase against the index
reliary impact SYMBOL         Pre-edit blast radius: callers, test files, risk verdict
reliary test-plan             Which tests exercise changed files/symbols
reliary diff REV REV          Structural diff between two revisions
reliary map                   Render a self-contained SVG map of the codebase
reliary bench                 Deterministic benchmark: generate questions+GT, score results
reliary init                  Auto-install agent integrations
reliary uninstall             Remove agent integrations
reliary doctor                Health check
reliary status                Show index + integration status
reliary update                Self-update from GitHub releases (checksum-verified)
reliary clean                 Clean caches and state
reliary logs                  Show recent log output
reliary config                Show resolved configuration
reliary completions SHELL     Emit shell completion script
reliary man                   Emit the man page
```

Run `reliary --help` for the complete, always-current command list.

## Key properties

- **Grammar-free**: no tree-sitter, no AST, no per-language code. Structural detection on brace/indent/blank-line boundaries works on any text file including Markdown, TOML, configs.
- **Index-first**: once trusted, all queries use the SQLite index (`.reliary/index.sqlite` per project). ~10s for most repos, ~80s for Linux kernel.
- **Deterministic**: identical inputs produce identical outputs (verified: 9 determinism tests). Cache-safe for provider-side KV caching.
- **Standalone binary**: single Rust binary, no Python/Node runtime, no daemon process.

## License

MIT
