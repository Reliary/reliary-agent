# Changelog

All notable changes to Reliary Agent will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **MCP tool surface: 19 tools → 8** — `reliary_find_references` is now the single entry point for all symbol questions (`def_only` / `usage_only` / `methods` / `dead_only` / `path_filter` modes). `goto_def` is deprecated. `call_graph`, `list_methods`, `find_dead_code`, `describe` are aliases over the same handlers. New: `reliary_similar` (near-clone detection).
- **One-line tool answers with raw code evidence** — every symbol answer returns `file:line` plus the actual source line, copy-verbatim by the model.
- **Bash compression edit-safety** — `reliary wrap` passes content readers (`cat`/`head`/`tail`/`less`/`bat`) on source-like files through uncompressed.
- **Old bench plans and reports** moved to `docs/archive/`.

### Benchmarks

- Deterministic claim verification, 4 seeds on `bench/results/v64_final_3way.jsonl`: F1=A 0.642 / B 0.299 / C 0.686; precision=A 0.986; billed=A 9,574 vs B 39,938 vs C 61,322; dead-ends=A 0.0.
- Bash compression: 46.3% average on the 6 V14 fixtures.

## [0.8.0] - 2025-XX-XX

### Changed - BREAKING

- **Removed: HTTP proxy (`reliary serve`)** — The reverse-proxy compression layer (965 LOC, 6 deps: `axum`, `tokio`, `tower-http`, `futures-util`, `tokio-stream`, `tracing`) was untested in benchmarks and never matched proxy-compressed output to LLM input. The `serve` and `daemon` subcommands are removed. The MCP-over-stdio path is the sole transport.

- **Removed: SSE over HTTP transport** — `mcp_sse.rs` removed alongside the proxy.

- **Removed: 39 research-grade modules** — Pure-research math tools that proved less effective than type-flow in symbol disambiguation benchmarks (`mAP 1.0`). These were exposed only behind `RELIARY_FULL_MENU=1`. Removed crates from `reliary-search` Cargo.toml: `nalgebra`, `flate2`, `rand`. Removed CLI subcommands: `BuildLsa`, `MineOps`.

- **Removed: `tracing` dependency** — Replaced with `eprintln!` macros. Tracing infrastructure (~6 deps transitive) was overkill for stderr-based CLI output.

- **MCP tool surface changes**: 62 tools reduced to 19 primary. Specialized variants (`find_references_robust`, `find_references_holographic`, etc.) removed. The 19 primary tools cover every documented use case.

### Performance

- Binary size: ~11 MB → ~8.2 MB (release profile, stripped).
- Cold-start query: 0.001-1.5s (lazy mode keeps derived tables sparse).
- Trust time on small repos: ~100ms.
- Trust time on Linux kernel (72K files): ~60s.

### Improved

- **Symbol intelligence accuracy**: mAP 1.0 on tokio (homonym benchmark), 0.93 cross-corpus on hyper.
- **Cross-language support**: Validated on Rust, Python, JavaScript, Go, Java, Prolog, Nix, Erlang, Haskell.
- **Grammar-free guarantee**: zero keyword matching, zero tree-sitter, zero per-language code in the hot path.

### Fixed

- **Proxy/daemon removal was incomplete**: CHANGELOG claimed `serve` and SSE were removed in 0.8.0 but the code paths (`routes.rs`, `inject_opencode_proxy_routes`, `restore_opencode_proxy_routes`, `write_proxy_routes`, `install_pi_proxy_routes`, `inject_sse_mcp_server`, `install_daemon`, `uninstall_daemon`, `daemon_alive`, `daemon_pid`, `proxy_stats`, `proxy_routes_count`, `has_upstream`, `Commands::Start/Stop/ProxyStats`, 9090/PID files/proxy-routes.json references) were still in the binary and the `reliary init` flow still rewrote OpenCode provider `baseURL`s to `http://127.0.0.1:9090/v1`. All proxy/daemon code, CLI subcommands, init prompts, doctor/status/logs/`has_upstream` plumbing, systemd/launchd registry installers, and tests have now been removed. Cargo.toml description no longer mentions "API proxy". Verified: 51/51 agent tests pass, zero `9090`/`proxy-routes`/`reliary-agent serve` hits in `crates/`.

### Maintained

- All 19 primary MCP tools: `search`, `find_references`, `find_references_with_source`, `find_references_type_flow`, `find_references_boltzmann`, `goto_def`, `callgraph`, `callgraph_v2`, `methods_on`, `scope`, `dead_symbols`, `brace_graph`, `call_graph`, `brace_debug`, `architecture`, `trace_path`, `query_ast`, `compress`, `risk`.

### Added

- **29 primary CLI subcommands** (down from 31 — `serve` and `daemon` removed).
- **`reliary build-occurrences`** — populate the lazy `occurrence` table for offline analysis.
- **`reliary build-all`** — populate all lazy tables eagerly.
- **`RELIARY_FULL_MENU=1` env var** — exposes the full MCP tool set when set.

### Removed

- `gate.js` (safety control layer) was already removed in the prior arc. The v0.8 arc focused on language universality and tool surface reduction.
- `proxy.rs`, `mcp_sse.rs` (transport layer)
- 39 research modules (math variants that underperformed type-flow)
- 9 Cargo dependencies (`axum`, `tokio`, `tower-http`, `futures-util`, `tokio-stream`, `tracing`, `tracing-subscriber`, `nalgebra`, `flate2`, `rand`)
- `Serve` and `Daemon` subcommands

### Migration from v0.6.x

If you previously used `reliary serve` to front your agent with IR compression:

**Before (v0.6.x):**
```bash
reliary serve --port 9090
# Configure agent proxy: http://127.0.0.1:9090
```

**After (v0.8):**
```bash
reliary mcp
# Configure agent MCP server (stdio transport)
```

The `mcp` path is more direct — no HTTP transport, no proxy cache, no second-process to monitor. The MCP server implements the same sanitization and rate-limiting logic as the proxy did, but over stdio.

If you relied on the HTTP API endpoints (`/search`, `/risk`, etc.) for non-MCP integrations, those are removed. Wrap MCP-over-stdio in your own server (~50 lines of Python/Node) to expose them.

## [0.6.13] - 2024-XX-XX

Initial public release. Added in CHANGELOG after first stable API.

---

For the complete history of the v0.6.x series, see the git history prior to the v0.8.0 rewrite.
