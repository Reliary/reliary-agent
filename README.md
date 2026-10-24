# Reliary

Grammar-free code intelligence and bash compression for AI coding agents.

A single Rust binary that indexes any codebase through structural detection —
no tree-sitter, no AST, no per-language code — and exposes the index to agents
over the Model Context Protocol (MCP). The companion command `reliary wrap`
compresses verbose shell output (`cargo test`, `git diff`, `pytest`) before it
reaches agent context.

**What it is for.** Answering "where is X defined", "who calls X", "what methods
does X have" in one small tool call instead of a grep-then-read cycle. On the
benchmark it answers at ~42% lower billed token cost than grep with about twice
the tool-output efficiency, and it verified more of its claims than either
alternative on that corpus.

## Install

Prebuilt binaries for Linux (x86_64, aarch64), macOS (Intel, Apple Silicon),
and Windows (x86_64, aarch64) are attached to every
[release](https://github.com/Reliary/reliary-agent/releases). (Windows aarch64
is best-effort: its release job may be absent on some tags.) Download the
archive for your platform from the latest release page, or:

```bash
# Linux x86_64. Substitute darwin/windows and aarch64 as needed.
VER=$(curl -s https://api.github.com/repos/Reliary/reliary-agent/releases/latest | grep -oP '"tag_name": "\K[^"]+')
curl -sSfL "https://github.com/Reliary/reliary-agent/releases/download/${VER}/reliary-agent-${VER}-linux-x86_64.tar.gz" | tar xz
./reliary-agent-${VER}-linux-x86_64 --version
```

Once installed, `reliary update` self-updates from the same releases with
SHA-256 verification, and detects an existing `cargo install` and points you at
the right upgrade command instead of clobbering it. Prompts fail closed on
non-TTY stdin (exit 2) unless `--yes` is given; output honors `NO_COLOR` and
disables color when piped.

From source (needs a C compiler — the bundled SQLite and rustls build it):

```bash
cargo install reliary-agent
```

## Quick start

```bash
# Index a project (first-time setup: creates .reliary/, writes its .gitignore, builds the index)
reliary trust .

# Rebuild after major changes (trust is for first run; index is rebuild)
reliary index .

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
`~/Library/Application Support/opencode/opencode.json`). OpenCode's local MCP
shape differs from Claude's — `command` is an array and `type` is required:
```json
{
  "mcp": {
    "reliary": { "type": "local", "command": ["/path/to/reliary", "mcp"] }
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

For automatic interception with no manual prefix, install the hook from
`crates/reliary-agent/hooks/` for your agent. Bash compression is on by
default; set `RELIARY_SIFT_BASH=0` to disable.

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

### Which project do the tools use?

The tools resolve the project index themselves, so they work even when the MCP
server was started from somewhere else. In order of precedence they use:

1. the `path` argument on the tool call, if it names a directory;
2. `RELIARY_ROOT`, if set;
3. the workspace root your client advertises over MCP (`roots/list` — OpenCode
   and other modern clients send this);
4. the server's working directory.

From that starting point they walk up to the nearest `.reliary/` index, so a
query from a subdirectory finds the project above it. Passing
`path="/path/to/project"` always selects that project explicitly.

## CLI

```
reliary trust [PATH]         First-time setup: create .reliary/, write its .gitignore, build index
reliary index [PATH]         Rebuild the index (existing project)
reliary reindex-file PATH    Re-index one file after an edit (used by hooks)
reliary search QUERY         BM25 search
reliary mcp                  Start the MCP server on stdio
reliary wrap [CMD]           Run CMD and compress its output
reliary sift [--stdin] [--llm] [--aggressive] [CMD] Pipe text through the compressor
reliary compress [--gentle] [TEXT] IR reasoning compression (arg text or stdin)
reliary pack PATH            Codebase symbol pack (signatures + surprise facts)
reliary dead [PATH]          Cross-file dead-code analysis
reliary risk FILE            Pre-edit risk analysis
reliary impact SYMBOL        Blast radius: callers, test files, risk verdict
reliary test-plan            Which tests exercise the changed files/symbols
reliary diff REV REV         Structural diff between two revisions
reliary warrant              Closure certificate: is a change finished? (exit 0 closed, 1 open, 3 indeterminate)
                             Caches indexes and scans by content in .reliary/warrant/ (--no-cache to bypass)
reliary map                  Render a self-contained SVG map of the codebase
reliary verify TEXT          Verify a claim about the codebase
reliary fix TASK             Deterministic bug-fix recipes, LLM fallback
reliary bench                Generate benchmark questions+GT and score results
reliary init                 Auto-install agent integrations
reliary uninstall            Remove agent integrations
reliary doctor               Health check (`--fix` rebuilds index + writes missing .gitignore)
reliary status --path DIR    Index and integration status (for DIR, default .)
reliary logs                 Tail the log output
reliary config               Show resolved configuration
reliary clean                Clean caches and state
reliary update               Self-update from GitHub releases (checksum-verified; exit 2 = available with --check)
reliary completions SHELL    Emit a shell completion script
reliary man                  Emit the man page
```

Run `reliary --help` for the complete, current list.

Exit codes: 0 success; 1 error (including a failed `trust`/`index` build);
2 means "needs a decision" — no index (run `trust`), or a prompt declined on
a non-TTY (re-run with `--yes`). `update --check` also uses 2 for "available".

`doctor --fix` must run from the project root (it fixes `./.reliary` only).

## Key properties

- **Grammar-free**: no tree-sitter, no AST, no per-language code. Structural
  detection on brace/indent/blank-line boundaries works on any text file.
- **Index-first**: once trusted, queries use the per-project SQLite index at
  `.reliary/index.sqlite`. That directory is local machine state and is
  git-ignored automatically (a `.reliary/.gitignore` holding `*` is written by
  `trust`), so `git add .` never stages the index or the command-output cache.
  The project config `.reliary/config.json` is user-local too; force-add it
  (`git add -f`) if a team wants to share settings.
- **Deterministic**: identical inputs produce identical output, so results are
  safe for provider-side KV caching.
- **Standalone**: one Rust binary, no Python or Node runtime, no daemon.

## Benchmarks

Measured with a deterministic claim-verification harness on a 10-question
corpus of this project's own source, 4 seeds, `deepseek-v4-flash`. reliary
answered 95% of verifiable claims correctly and every query produced a checkable
claim; grep and codebase-memory-mcp produced fewer verifiable claims and
verified fewer of them.

This corpus is *familiar* to the model. On a foreign repository (tokio) grep
led, and the familiarity experiment found no measurable familiarity effect, so
accuracy on unfamiliar code is **not established**. The corpus-independent wins
are cost, output size, and zero dead-ends.

| | F1 | Billed cost | Tool bytes | Dead-ends | Refusals |
|---|---|---|---|---|---|
| reliary | 0.937 | 20,605 | 11.7K | 0.0 | 0/40 |
| codebase-memory-mcp | 0.495 | 21,901 | 11.1K | 2.0 | 4/40 |
| grep | 0.596 | 35,456 | 23.7K | 0.0 | 1/40 |

Earlier comparisons against codebase-memory-mcp are **retracted**: the harness
called its tracer with the wrong parameter name, which silently disabled
caller/callee lookup on four of the ten questions.

Full methodology, the edit-outcome result (a tie — no demonstrated advantage),
the familiarity experiment, and a zero-cost cassette replay are in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## License

MIT
