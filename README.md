# reliary-agent

Grammar-free code intelligence daemon, CLI, MCP server, and API proxy.

**One binary. All local. No server required.**

Save 30-50% on API tokens across any agent framework — Pi, Claude Code, Cline, OpenCode.

## Quickstart

```bash
cargo install reliary-agent

# Auto-detect and configure your agents (Pi, Claude, Cline, OpenCode)
reliary-agent init

# Or start manually
reliary-agent serve &               # daemon + proxy on :9090
reliary-agent watch ./project       # continuously re-index
```

After `init`, your agents are configured with the daemon's MCP tools (search, risk, heal) and the proxy's conversation compression.

## Features

### Token Compression (works with any agent)

| Layer | Where | Savings | How |
|---|---|---|---|
| **Reasoning compression** | Gate.js (Pi) / proxy (all agents) | 30-50% | Strip LLM reasoning fluff ("Let me analyze...") before it reaches your bill |
| **Conversation window** | Proxy | 15-25% | Drop verbose tool results older than 8 turns |
| **Response cache** | Proxy | 0-100% | Repeated requests (same model, same messages) return cached results |
| **Tool schema stripping** | Proxy | ~150t/turn | Remove redundant tool descriptions the LLM already knows |

### Code Intelligence (MCP tools)

```bash
reliary-agent search "bm25_idf" ./project          # FTS5 search
reliary-agent risk ./src/main.rs                    # Pre-edit risk analysis
reliary-agent compress "Let me think..."            # Reasoning compression
reliary-agent dead ./project                        # Dead code detection
```

Every tool also available through MCP — works with Claude Code, Cline, OpenCode.

### Self-Healing Edits

When the LLM edits a file, `reliary` shadow-applies the change, runs tests, and reverts if tests fail. The LLM never sees the failure spiral.

```
edit → heal applies → cargo test → PASS → commit
edit → heal applies → cargo test → FAIL → revert → "REVERTED: assertion L42"
```

### Safety Features

- **Identifier veto:** blocks edits that reference hallucinated API names
- **Risk gate:** warns before editing files with high blast radius
- **Bash guard:** blocks destructive commands; routes `sed -i` through self-healing
- **Muzzle:** pauses background scavenger during active LLM sessions

## Documentation

- **[CONFIG.md](./CONFIG.md)** — Mode system, feature flags, config cascade
- **[SECURITY.md](./SECURITY.md)** — Vulnerability disclosure and security policy

## Install

```bash
cargo install reliary-agent
```

Or download a release tarball:

```bash
curl -sSfL https://github.com/Reliary/reliary-agent/releases/latest/download/reliary-$(uname -m)-unknown-linux-gnu.tar.gz | tar xz
cd reliary-* && ./install.sh
```

## Usage

### Usage by Agent

| Agent | What `rel init` does | Savings |
|---|---|---|
| **Pi** | Installs gate.js (tool-level compression) + sets proxy | 30-50% |
| **Claude Code** | Injects MCP config + sets `ANTHROPIC_BASE_URL=http://localhost:9090/v1/messages` | 15-25% |
| **Cline** | Injects MCP config + sets `OPENAI_BASE_URL=http://localhost:9090/v1` | 15-25% |
| **OpenCode** | Injectes MCP config + sets `OPENAI_BASE_URL=http://localhost:9090/v1` | 15-25% |

### CLI

```bash
# Explore
reliary-agent index ./project         # Build FTS5 search index
reliary-agent search "query" ./path   # Search index
reliary-agent risk ./src/file.rs      # Pre-edit risk analysis
reliary-agent dead ./project          # Dead code detection
reliary-agent memory "what we fixed"  # Cross-session memory

# Edit
reliary-agent fix-dir ./project       # Apply stored fix patterns
reliary-agent fix-file file old new   # Apply pattern to single file
reliary-agent apply-edit file tmp wd  # Self-healing edit

# Services
reliary-agent serve                   # Daemon + proxy (:9090)
reliary-agent init                    # Auto-configure agents
reliary-agent doctor                  # System health check
reliary-agent status                  # Project intelligence overview
reliary-agent logs                    # Tail daemon logs

# Config
reliary-agent config                  # Show current settings
reliary-agent config mode strict      # Set safety level (fast/reactive/strict)
```

### API Proxy (for any agent framework)

The `serve` command starts an OpenAI-compatible compression proxy on `localhost:9090`.
Point any agent at it to get conversation compression without installing gate.js:

```bash
# Start proxy
reliary-agent serve &

# Point any agent to it
export DEEPSEEK_BASE_URL=http://localhost:9090/v1
pi --model deepseek/deepseek-v4-flash --print "fix bug"
```

**What the proxy compresses:**
- **Conversation history:** old assistant reasoning messages are compressed before
  being sent to the API (~15-25% fewer billable tokens)
- **Response cache:** identical requests (same model, same messages) return cached
  responses — zero API cost on repeat edits
- **Tool schemas:** redundant description text is stripped from the tools array
  sent with each request (~150t saved per turn)
- **Context filter:** tool results older than 8 turns are dropped entirely,
  preventing unbounded context growth

### Output Formats

```bash
# Human (default)
reliary-agent search "merge_sort" ./project

# Agent (compact)
reliary-agent -f compact search "merge_sort" ./project
# → 4.2294 ./src/sort.rs

# CI (JSON)
reliary-agent -f json dead ./project | jq '.[] | select(contains("HIGH"))'
```

## Configuration

See [CONFIG.md](./CONFIG.md) for the full documentation.

### Quick Reference

| Env var | Effect |
|---|---|
| `RELIARY_MODE=fast` | Maximum compression (no safety rails) |
| `RELIARY_MODE=reactive` | Safety escalates on unsafe behavior (default) |
| `RELIARY_MODE=strict` | Full sandbox (bash blocked, edits always healed) |
| `RELIARY_FEATURES=+editMerge,-taskTargets` | Toggle individual features |
| `RELIARY_UPSTREAM_URL=https://api.openai.com/v1` | Set API upstream (default: auth-based routing) |
| `DEEPSEEK_BASE_URL=http://localhost:9090/v1` | Route through proxy |

## Built from

| Crate | Origin | What |
|---|---|---|
| `reliary-search` | stria | BM25 + FTS5, Porter stemming, phrase extraction |
| `reliary-compress` | gate | IR reasoning compression, conv-window |
| `reliary-sift` | sift + maxwell | Structural compression, entropy/diversity gates |
| `reliary-risk` | quale | Pre-edit risk scores, blast radius |
| `reliary-memory` | cortex-rs | HDC 10K-bit vectors, Hebbian learning |
| `reliary-fix` | cortex-rs + relay | Pattern extraction, content matching, signature matching |
| `reliary-dead` | carrion | Grammar-free dead code via occurrence counting |

## Synergies (one binary)

Search, risk, fix, dead, and memory share a tokenizer and session state.
Co-occurrence spans all operations. MCP server exposes all capabilities.

## Development

```bash
cargo build --release
cargo test --release
reliary-agent serve &    # start deamon + proxy
```

## License

MIT
