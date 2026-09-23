# Reliary8

Grammar-free code intelligence + bash compression for AI coding agents.

A single Rust binary (~9 MB) that indexes any codebase via structural detection (no tree-sitter, no language-specific code) and exposes it to AI agents through the Model Context Protocol (MCP). Companion tool `reliary wrap` compresses verbose bash output (cargo test, git diff, pytest) before it reaches agent context — a grammar-free compressor with a hard no-inflation guarantee.

**What it's for:** answering "where is X defined / who calls X / what methods does X have" in one small tool call instead of a grep-then-read cycle. On the benchmark below reliai is **roughly half the billed cost of grep and ~4× smaller in tool output, with comparable accuracy** (its accuracy edge is real-looking but within noise — see the honest note under Benchmarks).

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

Deterministic claim-verification bench (10 questions on a reliary corpus snapshot, deepseek-v4-flash, 4 seeds 42/17/123/456). Record: `bench/cassettes/canonical-v4/record.jsonl`.

**The demonstrated advantage is cost and speed at comparable accuracy.** reliai answers in fewer, smaller tool results at roughly half the billed cost of grep. Its accuracy is comparable to grep on this corpus (and ahead of altbackend); an accuracy *superiority* over grep is not established.

| Metric | Reliary (A) | Altbackend (B) | Grep (C) |
|--------|-------------|----------------|----------|
| F1 (claim-weighted) | **0.947** | 0.461 | 0.615 |
| Precision | **0.960** | 0.893 | 0.881 |
| Recall | **0.935** | 0.315 | 0.473 |
| Coverage | **1.00** | 0.93 | 0.88 |
| Keyword score /30 | 27.5 | 24.8 | **26.5** |
| Billed cost | **21,459** | 23,064 | 36,967 |
| Dead-ends | **0.0** | 3.5 | 0.0 |
| Tool bytes | **8.8K** | 13.8K | 33.3K |
| Wall (median) | **29s** | 32s | 39s |

Every model claim (`symbol at file:line`) is verified mechanically against the index — no LLM judge. Wall is provider-latency bound (~90% cache hit on all three). Repro (live, ~$0.05): `python3 bench/run_snapshot_bench.py --bin target/release/reliary --corpus "$HOME/src/v75-canon-replay" --conds A,B,C --seeds 42 17 123 456` (set `RELIARY_GT=1` for the corpus-matched questions) then `python3 bench/deterministic_verify.py --input <out.jsonl> --corpus "$HOME/src/v75-canon-replay"`.

**Free reproduction:** this exact run is committed as a cassette. `bench/replay_canonical.sh` builds a fresh checkout of the corpus at commit `5c8b6244` (a path of its own choosing), indexes it, and replays the sessions with **zero API calls, no key, $0**. All three conditions replay byte-identically on a fresh corpus at a different path — every score, answer, and cost figure matches (only wall-clock timers differ). Note for B: altbackend's index build is not reproducible even by itself (two fresh indexes of identical bytes gave 15,070 vs 14,937 edges), so its recorded hits are matched as a set with a sorted, rank-free projection. `REPLAY_CONDS=A,C` replays only the conditions that do not need altbackend installed.

**Read F1, not the keyword score.** The `Keyword score /30` row is a substring-matching
rubric kept only as a smoke test: it is gameable by keyword-stuffing and inflates
real accuracy roughly 2× (verified against ground-truth F1). The claim-verification
columns above are the meaningful comparison — every `symbol at file:line` the model
states is checked against the index, so invented locations are counted as errors.
Billed cost includes the provider's cache discount (≈94% cache hit on all three
conditions).

**Honest accuracy note.** reliai's F1 leads altbackend clearly and grep narrowly, but on
a single small corpus the F1 and an independent LLM judge both put the reliai-vs-grep
gap within noise (judge delta ≈1σ). Treat accuracy as **comparable to grep, ahead of
altbackend**; the reproducible, corpus-independent wins are cost (≈45% lower billed
than grep), tool-output size (≈4× smaller), wall time, and zero dead-ends. A test of
whether the accuracy edge was a training-prior artifact — obfuscating every identifier
in a memorized public repo — found **no flip** (`bench/FAMILIARITY_EXPERIMENT.md`).

Prompt-fairness disclosure: condition A receives the shipped routing prompt (~300 words); B/C receive ~100-word prompts. An ablation (`M`, minimal ~120-word prompt) scored F1 0.707 vs A's 0.816 — the tool contributes most of the gap, the routing prompt the remainder. See `docs/plans/V70_TELEPATHY.md`.

Full reproduction: `python3 bench/long_session_bench.py --conditions A,C --seeds 42 17`

**Edit-outcome benchmark: tie, twice — no demonstrated advantage.** Two
pre-registered mutation benches (`bench/MUTATION_BENCH_V2_PREREG.md`) ask whether
the comprehension advantage converts to *fixing bugs*. **Run 1** (tests visible):
all three conditions scored f2p 100% on 6 tasks × 2 seeds — but a runnable failing
test *is* the answer key, so the design was invalid. **Run 2** (the discriminating
design: every test stripped from the agent's workspace, bare prose symptoms, hidden
tests injected at scoring time): **again all three conditions scored 100%, zero
wrong-file edits.** Pre-registered kill criterion (reliai ≥ both by +15pp) failed
both times. Honest reading: single-line defects in a ~500-file repo are greppable
from symptom prose, so no index advantage exists to show. **reliai's edit-outcome
advantage is unproven; the demonstrated advantage is retrieval cost/precision.**
Details: `bench/MUTATION_BENCH_V2_RESULTS.md`.

### Deterministic transcripts (cassette)

A remote sampler cannot be forced deterministic from the client. Verified against
the live DeepSeek API: `seed` is accepted but ignored, `response_format:
json_object` is not enforced, and `json_schema` is unavailable. What *is* forced
is the transcript — `bench/cassette.py` records every model response keyed on the
exact sampling-affecting request plus the corpus's `meta.index_gen`, and replays
it byte-for-byte with **zero API calls**.

The committed canonical tape is `bench/cassettes/canonical-v4/` (A/B/C × seeds
42/17/123/456, 317 entries, 312 KB). Replay it against a fresh corpus checkout:

```sh
bench/replay_canonical.sh            # $0, no key, no network
```

```sh
# record a new tape (live API)
RELIARY_CASSETTE=bench/cassettes/rel8 RELIARY_CASSETTE_MODE=record \
  python3 bench/long_session_bench.py --conditions A --seeds 42

# replay forever (a miss is a hard error, never a live call)
RELIARY_CASSETTE=bench/cassettes/rel8 RELIARY_CASSETTE_MODE=replay-strict \
  python3 bench/long_session_bench.py --conditions A --seeds 42
```

Reindexing the corpus invalidates entries automatically (`index_gen` is part of
the key). `python3 bench/cassette.py summarize <dir>` lists near-tie decisions
(worst top1−top2 logprob margin) so residual score variance is attributable to
named coin-flips instead of hand-waved. Tests: `python3 -m pytest bench/tests/ -q`.

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
