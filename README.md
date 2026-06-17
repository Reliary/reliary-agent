# reliary-agent

[![Crates.io](https://img.shields.io/crates/v/reliary-agent.svg)](https://crates.io/crates/reliary-agent)
[![NPM Version](https://img.shields.io/npm/v/@reliary/agent.svg)](https://www.npmjs.com/package/@reliary/agent)
[![CI (guardrails)](https://github.com/Reliary/reliary-agent/actions/workflows/ci.yml/badge.svg)](https://github.com/Reliary/reliary-agent/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Grammar-free code intelligence daemon, CLI, MCP server, and API proxy.

**One binary. All local. No server required.**

Save 16-84% on API tokens and eliminate debug spirals across any agent framework — Pi, Claude Code, Cline, OpenCode.

- [Installation](#installation)
- [Quickstart](#quickstart)
- [Usage by Agent](#usage-by-agent)
- [Features](#features)
- [CLI Reference](#cli-reference)
- [Configuration](#configuration)
- [Architecture](#architecture)
- [Development](#development)

## Installation

```bash
# NPM (Recommended for Node.js developers)
npm install -g @reliary/agent

# Cargo (Recommended for Rust developers)
cargo install reliary-agent

# Homebrew (macOS / Linux)
brew install Reliary/tap/reliary-agent
```

## Quickstart

```bash
# Auto-detect and configure your agents (Pi, Claude, Cline, OpenCode)
reliary-agent init

# Or start manually
reliary-agent serve &              # starts daemon + API proxy on port 9090
reliary-agent index ./project      # build local search index
```

After `init`, your agents have access to the daemon's MCP tools (search, risk, guard).
For conversation compression, configure your agent to route through the [API Proxy](#token-compression-api-proxy).

## Usage by Agent

Every agent gets proxy-level compression and safety simply by routing its API calls through `localhost:9090`.

### Pi (gate.js extension)

```bash
reliary-agent init       # installs gate.js, prompts for proxy routing
reliary-agent serve &    # starts daemon + proxy on :9090
export OPENAI_BASE_URL=http://localhost:9090/v1   # route API calls through proxy
pi --model gpt-4o --print "fix it"
```

Pi gets the full stack:
- Proxy compression + edit safety (via `*_BASE_URL` pointing at localhost:9090)
- Gate.js extension (compresses all tool outputs)
- Transparent strict mode (bash/write/grep are safely redirected to sandbox tools without errors)
- Self-healing edits (tests run before the LLM sees failures)
- Default mode: **reactive** (safety escalates on unsafe behavior, auto-deescalates after N turns)

### Claude Code

```bash
reliary-agent serve &
export ANTHROPIC_BASE_URL=http://localhost:9090/
```

Claude Code gets:
- Proxy compression + edit safety (via `:9090`, routed via env var)
- MCP tools (search, risk, guard, dead) — auto-injected by `init`
- No transparent redirect (Claude uses its own Bash tool)

### Cline / OpenCode

```bash
reliary-agent serve &
export OPENAI_BASE_URL=http://localhost:9090/v1   # match your provider's *_BASE_URL convention
```

Both get:
- Proxy compression + edit safety (via `:9090`)
- MCP tools — auto-injected by `init`
- No gate.js (Pi-only extension)

### Savings by Agent Stack

| Agent | Stack | Savings |
|---|---|---|
| **Pi** | Proxy + guard + gate.js | **16-84% weighted cost** |
| **Claude Code** | Proxy + guard + MCP | **16-60%** |
| **Cline / OpenCode** | Proxy + guard + MCP | **16-60%** |
| **Any agent** | Proxy only (passthrough) | **0%** (just routing) |

> Long multi-turn sessions (15+ turns) hit the highest savings. Short 3-turn fixes hit the lower end. The safety guards eliminate catastrophic debug spirals.

## Features

### Token Compression (API Proxy)

The `serve` command starts an OpenAI-compatible proxy on `localhost:9090`. Point your agent's `*_BASE_URL` here to route all API calls through the proxy. The proxy discovers your upstream from your agent's provider config, or you can set `RELIARY_UPSTREAM_URL` as a global fallback.

| Mechanism | Savings | How it works |
|---|---|---|
| **First-appearance freeze** | 16-84% | Modern LLMs heavily discount repeated conversation history via prefix caching. We compress messages once and lock them. The provider never sees the bloated original, ensuring your cache discount stays intact. |
| **Command Output Compression** | 10-20% | Collapses noisy terminal output (e.g., condensing 100 lines of `Compiling...` into a 1-line summary) while perfectly preserving actual compiler errors and stack traces. |
| **Response cache** | 0-100% | Repeated identical requests return cached results instantly at zero API cost. |

```mermaid
flowchart LR
    A[Agent Request] --> B{Auth Routing}
    B --> C[Compression]
    C --> D{Cache Hit?}
    D -->|Yes| E[Return cached]
    D -->|No| F[Forward to API]
    F --> G[Stream back to agent]
    G --> H[Cache response]
```

### Self-Healing Edits

When the LLM edits a file, `reliary` shadow-applies the change, runs your test suite, and reverts the file if the tests fail. The LLM never sees the failure spiral. Toggle with `features.healEdit` (on by default, disable via `RELIARY_FEATURES=-healEdit`).

```mermaid
flowchart LR
    A[LLM sends edit] --> B{Daemon intercepts}
    B --> C[Shadow-apply to temp]
    C --> D[Run tests]
    D --> E{Tests pass?}
    E -->|Yes| F[Write to real file]
    E -->|No| G[Revert temp file]
    G --> H[Return REVERTED to LLM]
    F --> I[Return OK to LLM]
```

### Safety & Guardrails

- **Cross-File Edit Guard (on by default):** Intercepts edits and checks them against the local search index. If an edit would orphan cross-file references (e.g., renaming a function without updating the places that call it), a warning is injected *before* the edit reaches the LLM.
- **Anti-Decision Memory (on by default):** A cross-session learning system. If the LLM repeatedly tries and fails to use a specific identifier across multiple sessions, the proxy injects a subtle warning the next time it tries to use it, conditioning the LLM to stop repeating the mistake.
- **Transparent Strict Mode (Pi only):** Instead of blocking risky commands (like blind `sed` replacements) with error messages that confuse the LLM, the agent transparently redirects them to safe sandbox tools.
- **Identifier Veto:** Blocks edits that reference completely hallucinated function or variable names.
- **Risk Gate:** Warns the agent before it edits files with a high blast radius.

### Code Intelligence (MCP tools)

Every underlying tool is available through standard MCP, working natively with Claude Code, Cline, and OpenCode.

```bash
reliary-agent search "bm25_idf" ./project           # Fast local search
reliary-agent risk ./src/main.rs                     # Pre-edit risk analysis
reliary-agent describe ./src/main.rs                 # Identifier summary
```

## CLI Reference

### Core

```bash
reliary-agent serve                              # Start daemon + proxy on :9090
reliary-agent start                              # Start daemon in background
reliary-agent stop                               # Stop background daemon
reliary-agent doctor                             # System health check
reliary-agent doctor --fix                       # Check + auto-fix issues
reliary-agent status                             # Project intelligence overview
reliary-agent init                               # Auto-configure agents (Pi, Claude, Cline)
reliary-agent uninstall                          # Remove all integrations
```

### Search & Index

```bash
reliary-agent index ./project                    # Build search index
reliary-agent search "query" ./path              # Search index
reliary-agent risk ./src/file.rs                 # Pre-edit risk analysis
reliary-agent describe ./src/file.rs             # Identifier summary
```

### Compression

```bash
reliary-agent compress < input.txt               # IR reasoning compression (stdin)
reliary-agent sift cargo test                    # Pipe command output through compression
```

### Configuration

```bash
reliary-agent config                             # Show current config + file paths
reliary-agent config mode fast                   # Set gate mode (fast/reactive/strict)
reliary-agent config --local mode strict         # Set in project .reliary/config.json
reliary-agent clean                              # Wipe project .reliary (with confirmation)
reliary-agent clean --global                     # Wipe ~/.reliary
reliary-agent clean --all                        # Wipe both
reliary-agent logs                               # Tail daemon logs
reliary-agent logs --tail                        # Follow in real-time
reliary-agent logs --level debug                 # Filter by level
```

### Output Format

```bash
reliary-agent --format json search "query" .     # JSON output for scripts/CI
reliary-agent --format compact search "query" .  # Minimal output for agents
reliary-agent --format default search "query" .  # Human-readable (default)
```

### Utilities

```bash
reliary-agent completions bash                   # Generate bash completions
reliary-agent completions zsh                    # Generate zsh completions
reliary-agent completions fish                   # Generate fish completions
reliary-agent completions powershell             # Generate PowerShell completions
reliary-agent completions elvish                 # Generate elvish completions
reliary-agent completions bash --outdir ./dir    # Write completions to directory
reliary-agent man                                # Generate man page (stdout)
reliary-agent man --outdir ./man/man1            # Write man page to directory
reliary-agent trust .                            # Quick project setup (create .reliary + index)
reliary-agent update --check                     # Check for updates without installing
reliary-agent update                             # Download and install latest release
```

### Verbosity & Color

```bash
reliary-agent -v search "query" .                # Verbose output
reliary-agent -vv search "query" .               # Very verbose
reliary-agent -q search "query" .                # Quiet (errors only)
NO_COLOR=1 reliary-agent status                  # Disable colored output
```

## Configuration

See [CONFIG.md](./CONFIG.md) for full documentation on the cascading configuration system.

### Quick Reference

| Env var | Effect |
|---|---|
| `RELIARY_MODE=fast` | Maximum compression (no safety rails) |
| `RELIARY_MODE=reactive` | Safety escalates on unsafe behavior (default) |
| `RELIARY_MODE=strict` | Full sandbox — transparently redirects risky commands |
| `RELIARY_FEATURES=+editMerge,-healEdit` | Toggle individual features |
| `RELIARY_UPSTREAM_URL=https://api.openai.com/v1` | Default upstream for unknown API keys |
| `RELIARY_PROXY_GUARD_DISABLE=1` | Disable cross-file edit safety |
| `RELIARY_PROXY_ANTI_DISABLE=1` | Disable Anti-decision memory |

### Features

| Feature | Default | Description |
|---|---|---|
| `compress` | on | Reasoning compression on assistant messages |
| `convWindow` | on | Conversation window collapsing for old messages |
| `readEnrichment` | on | Enrich file reads with structural summaries |
| `editMerge` | off | Merge consecutive edits (disabled due to regression) |
| `healEdit` | on | Self-healing: test edits before applying |
| `priorInjection` | off | Inject prior session knowledge (disabled due to overhead) |

## Architecture

This binary consolidates 9 crates into one extremely fast executable with a shared tokenizer and session state (zero IPC overhead).

```mermaid
graph TD
    A[CLI] --> D[Daemon Core]
    B[MCP Server] --> D
    C[API Proxy :9090] --> D
    D --> E[(Search Index)]
    D --> F[(Chronicle Database)]
    D --> G[(Co-occurrence Matrix)]

    C --> H[Upstream API]
    I[Pi Agent] --> C
    J[Claude Code] --> C
    K[Cline] --> C
```

- **search:** Fast local search using BM25 and stemming
- **compress:** Reasoning compression
- **sift:** Terminal output compression and noise reduction
- **risk:** Pre-edit risk scoring and blast radius calculation
- **memory:** Cross-session learning and recall
- **fix:** Pattern extraction and forgiving signature matching
- **dead:** Dead code detection via occurrence counting
- **agent:** The core binary serving the daemon, proxy, CLI, and MCP

## Development

```bash
cargo build --release
cargo test --release -- --test-threads=1
reliary-agent serve &    # start daemon + proxy
```

## Documentation

- **[CONFIG.md](./CONFIG.md)** — Mode system, feature flags, config cascade
- **[SECURITY.md](./SECURITY.md)** — Vulnerability disclosure and security policy
- **[CONTRIBUTING.md](./CONTRIBUTING.md)** — Build, test, PR workflow
- **[pi/GATE.md](./pi/GATE.md)** — Pi extension reference

## License

MIT
