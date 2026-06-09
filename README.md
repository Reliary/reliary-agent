# reliary-agent

Grammar-free code intelligence daemon, CLI, MCP server, and Pi Agent shim.

One binary. All local. No server required.

## Install

```bash
cargo install reliary-agent
```

## Usage

```text
reliary-agent search "bm25_idf"          BM25 code search (from stria)
reliary-agent compress "long text..."    IR reasoning compression (from gate)
reliary-agent risk src/main.rs           Pre-edit risk analysis (from quale)
reliary-agent fix-file file.rs old new   Pattern-based file fix (from cortex)
reliary-agent fix-dir ./project          Apply known fix patterns
reliary-agent dead ./project             Grammar-free dead code detection
reliary-agent memory "query"             Cross-session HDC memory
reliary-agent serve                      MCP stdio server (works with any agent)
```

All commands accept `--format` flag: `default` (human), `compact` (agent), `json` (CI).

## Built from

| Crate | Origin | What |
|---|---|---|
| `reliary-search` | stria | BM25, Porter stemming, phrase extraction |
| `reliary-compress` | gate | IR reasoning compression, conv-window |
| `reliary-sift` | sift + maxwell | Structural compression, entropy/ratio/diversity gates |
| `reliary-risk` | quale | Pre-edit risk scores, blast radius |
| `reliary-memory` | cortex-rs | HDC 10K-bit vectors, Hebbian learning |
| `reliary-fix` | cortex-rs + relay | Pattern extraction, content matching, signature matching |
| `reliary-dead` | carrion | Grammar-free dead code via occurrence counting |

## Synergies (one binary, many capabilities)

- Search, risk, fix, dead, and memory share a common tokenizer and session state
- Co-occurrence spans all operations: search queries enrich memory, memory feeds fix patterns
- MCP server exposes all 7 capabilities under one protocol

## License

MIT
