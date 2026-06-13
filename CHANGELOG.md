# Changelog

## v0.2.0 (unreleased)

### Major Features

- **Unified port architecture** — daemon and proxy now on a single HTTP server (:9090). Removed the separate TCP daemon on :9799. One port, one process, one protocol.
- **Provider-agnostic proxy** — routes by Authorization header, not model name. No hardcoded providers, no model lists, no per-provider configuration.
- **Self-healing edits for bash+sed** — intercepts `sed -i` commands and routes them through heal-apply. Zero-distraction failure recovery.
- **Grammar-free design throughout** — zero AST, zero tree-sitter, zero language detection. All analysis uses identifier scanning, Porter stemming, byte DFA, and indentation-based boundary detection.

### Compression

- **Gate.js at -42% reasoning compression** (proven on standard benchmark)
- **Proxy conversation compression** — feed-forward compression of old assistant messages (~15-25% savings)
- **Response cache** — repeated edit requests return cached results (zero API cost)
- **Tool schema stripping** — removes redundant tool descriptions (~150t saved per turn)
- **Context filter** — drops verbose tool results after 8 turns, capping unbounded conversation growth

### Crates

- **reliary-search** (port of stria) — BM25 + FTS5, Porter stemming, grammar-free phrase extraction
- **reliary-compress** (port of gate.js) — inline reasoning compression, dict-based symbol shortening
- **reliary-sift** (port of sift + maxwell) — zone truncation, entropy gate, structural compression
- **reliary-risk** (port of quale) — pre-edit risk scores, blast radius, chronicle failure tracking
- **reliary-memory** (port of cortex-rs) — HDC 10K-bit vectors, Hebbian learning, cross-session recall
- **reliary-fix** (port of cortex-rs + relay) — pattern extraction, content matching, forgiving signature matching
- **reliary-dead** (port of carrion) — grammar-free dead code via occurrence counting

### Safety

- **Identifier veto** — blocks edits referencing hallucinated API names (checks against FTS5 index)
- **Self-healing edits** — shadow-applies edits, runs tests, reverts on failure. LLM never sees the failure spiral.
- **Bash guard** — blocks destructive patterns (rm -rf /) while allowing build/test commands
- **Muzzle** — pauses background scavenger during active LLM sessions (auto-expires after 30 min)
- **Secrets scanning** — pre-commit hook with gitleaks + cargo audit + cargo deny

### Security

- **Supply chain hardening** — GitHub Actions pinned by SHA (not version tags), deny.toml with license allowlist and crate bans
- **MSRV policy** — minimum Rust 1.75
- **Release integrity** — SHA256SUMS in release artifacts
- **Vulnerability monitoring** — cargo audit in CI, weekly dependency updates via Dependabot
- **Binary hardening** — LTO, panic=abort, strip, all crate roots #[forbid(unsafe_code)]

### Developer Experience

- **Unified CLI** — 15 subcommands under one binary
- **MCP server** — all tools exposed for any agent framework
- **Agent auto-detection** — `rel init` detects Pi, Claude Code, Cline, OpenCode
- **Platform support** — Linux (x64 + ARM), macOS (x64 + ARM), Windows (x64)
- **Config cascade** — env var > project config > user config > built-in defaults
- **Feature toggles** — per-mechanism enable/disable via config file or env var
- **Benchmark guard** — automated regression detection against known baselines

### Testing

- **57 unit tests** across 9 crates
- **Integration test** covering all 11 daemon endpoints
- **18 feature branches**, 24 merged to master, 20 experimental preserved

### Performance

- **mimalloc** global allocator
- **rayon** parallel indexing and dead code scanning
- **FxHashMap** and **AHashMap** for fast hashing in hot paths
- **LTO** via release profile
- **Binary size**: 6.9MB stripped

## v0.1.0

- Initial release
- 9 crate workspace with BM25 search, IR compression, risk, memory, fix, dead code
- TCP daemon on :9799
- MCP server for agent integration
- Gate.js Pi extension at -42% savings
