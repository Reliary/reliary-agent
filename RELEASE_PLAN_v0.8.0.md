# Release Plan: Reliary v0.8.0 — Ship as new master

## Goal

Force-push reliary8 as the new master of `Reliary/reliary-agent`. Delete the
untested proxy. Ship a pure MCP + CLI tool. Update all documentation.

---

## Phase 1: Delete proxy + unused deps (~2 hours)

### 1a. Remove proxy source files
- Delete `crates/reliary-agent/src/proxy.rs` (965 lines)
- Delete `crates/reliary-agent/src/mcp_sse.rs` (242 lines)
- Remove `mod proxy;` and `mod mcp_sse;` from `main.rs` (lines 7, 15)

### 1b. Remove proxy-related CLI commands
Delete these `Commands` variants from the `Subcommand` enum in `main.rs`:
- `Serve { port }` — daemon + proxy (line 733)
- `Daemon` — deprecated alias for serve (line 1953)
- `Start` — spawns daemon in background (line 1590)
- `Stop` — kills daemon (line 1624)
- `ProxyStats` — proxy statistics (line 1711)

Delete their match arms in the `match commands` block.
Delete from `CLI_COMMANDS` list (line 693-695): `"serve"`, `"start"`, `"stop"`, `"proxy-stats"`.

Keep: `Sift`, `Wrap`, `CacheStore`, `CacheRetrieve`, `CacheStats`, `search`, `index`, `compress`, `risk`, `trust`, `mcp`, `init`, `uninstall`, `doctor`, `status`, `update`, `build-all`, `build-occurrences`, `vacuum`, `dead`, `fix`, `classify`, `mine-ops`, `parse-expr`, `completions`, `mangen`.

### 1c. Remove proxy deps from Cargo.toml
Remove from `crates/reliary-agent/Cargo.toml`:
- `axum = "0.7"`
- `tokio = { version = "1", features = ["full"] }`
- `tower-http = { version = "0.6", ... }`
- `futures-util = "0.3"`
- `tokio-stream = "0.1"`
- `bytes = "1"`

Keep:
- `reqwest` — used by `update` subcommand for GitHub release API (keep `blocking` feature, drop `stream`)
- `tracing` / `tracing-subscriber` — used by MCP server and general logging

Update reqwest to: `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "blocking"] }` (drop `stream`).

### 1d. Remove daemon/ux functions
From `crates/reliary-agent/src/ux.rs`, remove:
- `daemon_alive()`
- `daemon_pid()`
- `daemon_pid_path()`
- `wait_for_daemon()`
- `remove_pid_file()`
- `proxy_stats()` (the proxy stats display function)
- Any PID file references

Keep all other ux functions (colors, prompts, formatting).

### 1e. Update help text
- Remove "daemon + proxy on :9090" from help banner
- Update about string to: "Grammar-free symbol intelligence — CLI and MCP server"
- Remove proxy/daemon examples from quickstart
- Update CLI_COMMANDS list

### 1f. Build + test
- `cargo build --release` — verify clean
- `cargo test --workspace` — verify 290+ tests pass
- Measure binary size (expect ~5 MB, down from ~11 MB)

---

## Phase 2: Create root-level documentation (~2 hours)

Reliary8 is missing: README.md, CHANGELOG.md, LICENSE, CONTRIBUTING.md, CONFIG.md, SECURITY.md.

### 2a. Copy LICENSE from reliary-agent
- `cp /home/user/src/reliary-agent/LICENSE ./LICENSE`

### 2b. Write README.md (new, ~200 lines)
Sections:
- **Title + badges** (Crates.io, NPM, CI, License) — reuse from agent
- **Tagline**: "Grammar-free symbol intelligence for all coding agents."
- **What it does**: MCP server + CLI. Index any repo. Query symbols without AST/tree-sitter.
- **Key differentiator table** vs ALTBACKEND/tree-sitter tools:
  - Works on ANY language (Prolog, Nix, Erlang — not just 158 grammars)
  - 7 MB binary (vs ALTBACKEND 253 MB)
  - 1 call per query (vs ALTBACKEND's 11-call search_graph + get_code_snippet)
  - mAP 1.0 on find-references (tokio bench)
- **Quickstart**: `reliary trust .` then `reliary mcp` (or agent-specific setup)
- **Installation**: cargo install, npm, brew
- **CLI reference**: list subcommands
- **MCP tools**: list the 19 primary tools with one-liners
- **Performance**: bench results table (reliary vs ALTBACKEND vs grep)
- **Agent integration**: Pi, Claude Code, OpenCode setup
- **License**: MIT

### 2c. Write CHANGELOG.md (~80 lines)
```
# Changelog

## v0.8.0 — Symbol Intelligence Release

### Added
- Grammar-free symbol intelligence: find_references, goto_def, callgraph,
  dead_symbols, methods_on, scope, brace_graph, architecture, trace_path
- 19 primary MCP tools (was 6)
- Cross-language support: Rust, Python, JavaScript, Go, Java, Nix, Prolog,
  Haskell, Erlang (grammar-free, no per-language code)
- Type-flow disambiguation (mAP 1.0 on tokio, 0.928 cross-corpus on hyper)
- BM25 phrase search across all indexed files
- File watcher with incremental re-indexing
- Data-driven anchor selection (no per-language patterns)
- Cognitive summary output format
- `reliary wrap` for bash output compression
- Agent hooks: Claude Code, OpenCode, Pi

### Changed
- Binary: 7 MB → 5 MB (removed proxy/daemon stack)
- Index speed: lazy occurrence tables, trust in ~100ms for tokio
- DB size: 664 KB for tokio (was multi-MB in v0.6)

### Removed
- HTTP proxy server (`reliary serve`)
- LLM pass-through proxy (`/v1/chat/completions`, `/v1/messages`)
- IR reasoning compression (KV cache-busting, never fired on benchmarks)
- gate.js control layer (veto, cage, muzzle, redirect)
- heal_edit endpoint
- daemon start/stop commands
- MCP-over-SSE transport
- axum, hyper, tower, tokio full runtime dependencies

### Benchmark Results
- Tokio find-references: median mAP 1.000 (was 0.172 in v0.6)
- Cross-corpus (hyper): median mAP 0.928
- vs ALTBACKEND: 25/30 score (tied), 0.90x WC, 22% faster wall time
- vs ALTBACKEND on Nix: 61 hits vs 0 hits (ALTBACKEND can't parse Nix)
```

### 2d. Write CONFIG.md (~50 lines)
Document environment variables:
- `RELIARY_EAGER_INDEX=1` — build all occurrence tables at trust time
- `RELIARY_SIFT_BASH=1` — pipe bash output through reliary wrap
- `RELIARY_GATE=1` — enable tool adoption gate
- `RELIARY_FULL_MENU=1` — show all 62 MCP tools (research variants)
- `RELIARY_MCP_TIMEOUT` — MCP tool call timeout
- `NO_RELIARY_WATCHER=1` — disable file watcher

### 2e. Write CONTRIBUTING.md (~40 lines)
Copy from reliary-agent, update for new structure:
- How to build: `cargo build --release`
- How to test: `cargo test --workspace`
- How to benchmark: `cd bench && python3 long_session_bench.py`
- How to add a new symbol query algorithm
- Architecture overview (crates diagram)

### 2f. Write SECURITY.md (~30 lines)
Copy from reliary-agent, update for removed proxy:
- No network server (MCP over stdio only)
- No API key handling (no proxy)
- All processing local (no data leaves the machine)
- Report vulnerabilities via GitHub

---

## Phase 3: Update npm + CI (~1 hour)

### 3a. Copy npm/ directory from reliary-agent
- Copy `npm/package.json`, `npm/bin.js`, `npm/install.js`
- Update `package.json`: version → `0.8.0`, description → remove "proxy"
- Update `install.js`: repo stays `Reliary/reliary-agent`, version parsing unchanged

### 3b. Copy .github/workflows/ from reliary-agent
- Copy: ci.yml, release.yml, publish.yml, scorecard.yml, dependabot.yml, codeql-analysis.yml, pr-secret-scan.yml, hardening.yml, size.yml, fuzz.yml, bench.yml
- Update ci.yml: remove any proxy/daemon tests, update test commands
- Update release.yml: binary name stays `reliary`, update asset names

### 3c. Copy scripts/ from reliary-agent (bench scripts)
- Copy benchmark scripts that are still relevant
- Skip: bench_guard.py, bench_proxy.py (proxy-specific)

### 3d. Copy Dockerfile.e2e from reliary-agent
- Update to remove proxy/daemon references

---

## Phase 4: Force-push to GitHub (~30 min)

### 4a. Prepare the push
```bash
cd /home/user/src/reliary8
git remote add origin https://github.com/Reliary/reliary-agent.git
# Or update existing remote
git remote set-url origin https://github.com/Reliary/reliary-agent.git
```

### 4b. Merge arc66 branch into main
```bash
git checkout main  # or create main if it doesn't exist
git merge arc66-data-driven-anchor
```

### 4c. Force-push
```bash
git push --force origin main
git push origin --tags
```

### 4d. Create GitHub release
```bash
gh release create v0.8.0 --title "v0.8.0 — Symbol Intelligence" --notes-file CHANGELOG.md
```

### 4e. Publish to crates.io
```bash
cargo publish --dry-run  # verify
cargo publish
```

### 4f. Publish to npm
```bash
cd npm && npm publish
```

---

## Phase 5: Update AGENTS.md (~30 min)

The existing `AGENTS.md` in reliary8 is the agent usage guide. Update:
- Remove proxy/daemon references
- Remove "serve" from CLI examples
- Update MCP tool count (19 primary)
- Update performance table with arc63 results
- Update "When NOT to use reliary" section

---

## Execution order

1. Phase 1 (delete proxy) — verify build + tests pass
2. Phase 2 (docs) — write all root-level docs
3. Phase 3 (npm + CI) — copy and update release infrastructure
4. Phase 4 (force-push) — ship to GitHub
5. Phase 5 (AGENTS.md) — final polish

**Total estimated time: ~6 hours**
**Total LOC removed: ~1,500 (proxy.rs + mcp_sse.rs + daemon/ux functions)**
**Total LOC added: ~400 (docs)**
**Binary size: ~11 MB → ~5 MB**

---

## Risk assessment

| Risk | Mitigation |
|------|-----------|
| `update` subcommand breaks (reqwest dep change) | Keep reqwest with `blocking` feature |
| Tests reference removed commands | Search + fix all test references to `serve`/`daemon`/`start`/`stop` |
| npm install breaks | Test locally before publish |
| crates.io publish fails | `cargo publish --dry-run` first |
| GitHub Actions break | Copy working workflows from reliary-agent, update minimally |
| Existing `reliary serve` users break | No users — documented in plan |

## What this plan does NOT do

- Does NOT keep the LLM proxy (proven unnecessary in benchmarks)
- Does NOT keep gate.js/heal/veto (control layer, all failed vs 2.7x variance)
- Does NOT keep MCP-over-SSE (only stdio MCP, all tested agents use stdio)
- Does NOT keep the HTTP daemon endpoints (re-addable in an afternoon if needed)
- Does NOT do backward compatibility (no users)

If we later need HTTP/MCP-SSE for a hosted server, re-add axum (~3 deps, ~100 lines).
