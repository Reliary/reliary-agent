# reliary-agent

Grammar-free code intelligence daemon, CLI, MCP server, and Pi Agent shim.

One binary. All local. No server required.

## Install

```bash
cargo install reliary-agent
```

## Usage

### Human (default)

```bash
reliary-agent index ./project
reliary-agent search "bm25_idf" ./project
reliary-agent compress "Let me think about this bug carefully..."
reliary-agent risk ./src/main.rs
reliary-agent fix-dir ./project
reliary-agent dead ./project
reliary-agent memory "what did we fix yesterday"
reliary-agent daemon            # start TCP server on :9799
reliary-agent serve             # start MCP server (stdio) and API proxy
```

### API Proxy (for any agent framework)

The `serve` command starts an OpenAI-compatible compression proxy on `:9090`.
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

**Features controlled via config file or environment (see CONFIG.md):**
- `RELIARY_MODE=fast|reactive|strict` — safety level
- `RELIARY_FEATURES=+editMerge,-taskTargets` — toggle individual features
- `RELIARY_REPLAY=record|replay` — deterministic benchmark mode

### Agent (compact output)

```bash
reliary-agent -f compact search "merge_sort" ./project
# → 4.2294 ./src/sort.rs
# → 2.8196 ./tests/sort_test.rs
```

### CI (JSON output)

```bash
reliary-agent -f json dead ./project | jq '.[] | select(contains("HIGH"))'
```

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

## License

MIT
