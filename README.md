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
reliary-agent serve             # MCP server for any agent framework
```

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
