# Changelog

## v0.6.4

### Scorecard Security (June 2026)
- **CodeQL analysis:** Runs on every push/PR with Rust, JavaScript/TypeScript, Python (SAST: 0→10)
- **Per-tarball cosign signing:** Each release artifact signed individually (Signed-Releases: 0→10)
- **Repository Rulesets:** Branch protection migrated from classic rules to Repository Rules (Branch-Protection: -1→10)
- **SCORECARD_TOKEN wired:** Fine-grained PAT support for Branch-Protection check (fallback to GITHUB_TOKEN)
- **Docker digest pin:** `FROM ubuntu:24.04@sha256:...` (Pinned-Dependencies: 9→10)
- **SECURITY.md:** Updated with branch protection, SAST, and security practices documentation

### Quality of Life
- **Doctor multi-install detection:** Scans PATH + cargo + brew + npm installations. Warns on stale or duplicate copies
- **Daemon lifecycle polish:** `start` waits for health check, writes PID file, confirms "started on :9090". `stop` uses PID file → graceful SIGTERM → wait → `pkill` fallback. `status` shows daemon PID
- **Update per-method hints:** `update --check` shows upgrade commands for each install method (`cargo`, `brew`, `npm`)

### CI & Release
- **NPM trusted publishing:** OIDC provenance via `npm publish --provenance`
- **brew formula auto-push:** Fixed `mkdir -p` bug, PAT-based push to `Reliary/homebrew-tap`
- **Release YAML cleanup:** Removed YAML parser corruption from repeated edits

## v0.6.0

### UX Polish (June 2026)
- **Shell completions:** `reliary-agent completions {bash,zsh,fish,powershell,elvish}` via clap_complete. Optionally write to file with `--outdir`.
- **Man page generation:** `reliary-agent man [--outdir ./man/man1]` via clap_mangen.
- **Pager integration:** Long output from `search`, `dead`, and `status` pipes through `$PAGER` when stdout is a TTY.
- **NO_COLOR support:** All ANSI color helpers respect the no-color.org standard. Also respects `TERM=dumb`.
- **Verbosity flags:** `-v`/`-vv`/`-vvv` and `-q` available on every command.
- **Progress spinner:** `index` and `dead` now show a progress indicator while working.

### New Commands
- **`reliary-agent trust .`:** One-shot project setup -- creates `.reliary/` and builds the search index.
- **`reliary-agent update [--check]`:** Downloads the latest release from GitHub and replaces the current binary.
- **`reliary-agent completions`:** Shell completion generator for bash/zsh/fish/powershell/elvish.
- **`reliary-agent man`:** Man page generator.

### Config Validation
- **Unknown key warnings:** If you type `reliary-agent config mode strict` and misspell (`mod`), it warns.
- **Invalid mode detection:** Values other than `fast`/`reactive`/`strict` trigger a warning.
- **Invalid JSON detection:** Malformed config.json prints a clear warning instead of silently parsing as empty.

### Init Wizard
- **Setup wizard UI:** Fancy ASCII art banner, welcome message, and summary box at the end showing how many agents were configured plus next steps.

### README Overhaul
- **Crystal-clear agent wiring:** Usage by Agent section rewritten with exact, copy-paste steps for every agent. Each agent's section lists what you get, what you don't get, and how to verify it's working.
- **Hidden commands documented:** `apply-edit`, `fix-dir`, `fix-file`, `mcp`, `memory`, `session-state`, `veto` now listed (with explanation of what they do internally).
- **MCP tools section fixed:** Lists all 7 tools (search, compress, risk, fix, dead, heal, prior) instead of stale subset.
- **Default mode corrected:** Every reference says `strict` (not `reactive`).
- **Troubleshooting section:** Common failure modes and their fixes.

### Internal
- 70 unit tests passing (was 57 at v0.5.0). Zero compiler warnings.
- Clean build with `-D warnings` enforced in CI.

### Documentation fixes
- **Pi agent setup:** README now includes the `export OPENAI_BASE_URL=...` step (was missing — Pi would bypass the proxy). Removed false "routed automatically" claim.
- **Stale provider detection text:** Replaced with accurate description of upstream discovery via agent configs or `RELIARY_UPSTREAM_URL`.
- **Env var table:** Clarified `RELIARY_UPSTREAM_URL` example is just an example, replace with your provider's URL.

## v0.5.2

### Provider-agnostic (June 2026)

- **Removed `scan_env_vars()`:** The proxy no longer hardcodes mappings like `DEEPSEEK_API_KEY` → `api.deepseek.com`. Auto-discovery now uses agent configs only (OpenCode, Pi, Claude, Cline). Unknown API keys fall through to `RELIARY_UPSTREAM_URL`.
- **Fixed `normalize_url()` for generic upstreams:** URLs without a known path suffix now get `/v1/chat/completions` appended instead of bare `/chat/completions`. Fixes routing for LiteLLM and other non-standard endpoints.
- **Cleaner `init` prompts:** No provider names in Pi proxy routing or fallback messages. Documents `RELIARY_UPSTREAM_URL` as the generic fallback.
- **Docs:** README/CONFIG.md examples use neutral provider references. `RELIARY_UPSTREAM_URL` documented.
- **Test data:** All `DEEPSEEK_API_KEY` references replaced with `OPENAI_API_KEY` in test fixtures.

## v0.5.1

### Bugfix
- **cargo install from crates.io:** Fixed `include_str!` path for `gate.js`. The old `../../../pi/gate.js` path resolved outside the crate directory and failed when installing via crates.io. Moved `gate.js` into the crate (`crates/reliary-agent/pi/gate.js`) and CI guard added to keep workspace-root and crate copies in sync.

## v0.5.0

### Pi Readiness & Transport (June 2026)
- **SSE MCP Transport:** MCP server now available via SSE on the same port as the proxy (:9090). No subprocess per agent — tools share memory with the proxy for anti-decision, session hashes, and response cache. Stdio fallback remains for agents without SSE support.
- **Structured Logging:** New `log.rs` module with `tracing` + `tracing-subscriber`. `RELIARY_LOG` env var controls verbosity (error/warn/info/debug/trace). `logs --tail` and `logs --level` for live log watching. `RELIARY_LOG_FILE` for persistent file logging with 10MB rotation.
- **Gate.js Log Levels:** RELEASE_LOG env var filters gate.js output. Default `info` — quiet until something breaks. `debug` shows compression ratios, tool redirects, heal events.
- **Binary Discovery:** Gate.js now checks `RELIARY_BIN_PATH` → `which reliary-agent` → hardcoded fallbacks. No more silent degradation on PATH-only installs.
- **Pi Proxy Routing:** `init` prompts to configure proxy routing after gate.js install. Scans Pi settings.json + env vars for API keys, writes proxy-routes.json automatically.
- **Daemon Service Verification:** After systemctl/launchctl install, verifies the service is actually active. Prints manual recovery command on silent failure.

### Testing
- **ISTQB Tests:** 10 new Rust unit tests (log rotation, boundary conditions, Pi proxy routing from env/settings, MCP config injection, SSE config, removal). 20 gate.js JavaScript tests (log levels, binary discovery priority, feature flag parsing, syntax validation).
- **CI:** Gate.js test suite added to CI workflow. Test count threshold raised to 96.
- **MCP Dispatch Fix:** `e2e_heal` test corrected from non-standard `tools/fix` to standard `tools/call` dispatch. Full MCP round-trip verified.

### Internal
- All operational `eprintln!` replaced with `tracing::{info, warn, error, debug}` macros. Tracing writes to stderr (never stdout) — MCP JSON protocol on stdout stays clean.
- 966 lines changed across 17 files.

## v0.4.1

### Polish & Stability
- **Massive Integrity Pass:** Fixed 131 internal issues across the codebase.
- **SQL Hardening:** Added `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL` to all 15+ SQLite connections.
- **I/O Safety:** Switched to atomic file writes (tmp + fsync + rename) for config and service files. Replaced 20+ silent `.ok()` error swallows with proper logging.
- **Regex Performance:** Compiled all hot-path regexes once using `LazyLock` (16 instances).
- **Security:** Added 10MB file size guard to proxy/MCP endpoints to prevent memory exhaustion attacks.
- **Documentation Overhaul:** Rewrote documentation to improve flow, remove legacy academic jargon, and clearly explain agent configuration setups.
- **CI Guardrails:** Added rigorous CI checks for silent error swallows, SQL PRAGMAs, regex compilation, atomic writes, and CLI documentation coverage.

## v0.4.0

### Safety & Guardrails
- **Anti-Decision Memory:** Added a cross-session learning system using chronicle (SQLite). The LLM is subtly warned when reusing identifiers that failed repeatedly in the past.
- **Transparent Strict Mode:** Pi Agent's strict mode now transparently redirects blocked commands (`bash`, `write`, `grep`) to safe sandbox tools without returning confusing error messages. 100% pass rate on benchmarks.
- **Guard on by default:** The proxy intercepts edits to check against the search index, warning the LLM if an edit orphans cross-file references.

### Compression
- **First-appearance freeze:** Proxy compresses messages on first occurrence and freezes them in cache.
- **Sift Everywhere:** Sift (structural terminal output collapse) now compresses all tool results over 300 characters, not just `bash`.

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

- **reliary-search** — BM25 + FTS5, Porter stemming, grammar-free phrase extraction
- **reliary-compress** — IR reasoning compression, format coercion
- **reliary-sift** — zone truncation, entropy gate, structural compression
- **reliary-risk** — pre-edit risk scores, blast radius, chronicle failure tracking
- **reliary-memory** — HDC 10K-bit vectors, Hebbian learning, cross-session recall
- **reliary-fix** — pattern extraction, content matching, forgiving signature matching
- **reliary-dead** — grammar-free dead code via occurrence counting

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
