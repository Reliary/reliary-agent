# Contributing to Reliary

Thanks for your interest in contributing. Reliary is a single-binary Rust tool that indexes codebases via grammar-free structural detection and exposes the index to AI coding agents through the Model Context Protocol (MCP).

## Development setup

Requires Rust 1.80+ (stable). The tree uses `std::sync::LazyLock` (stabilized in
1.80); edition 2021 throughout, no nightly features.

```bash
git clone https://github.com/Reliary/reliary-agent.git
cd reliary-agent
cargo build --release
```

The binary is built at `target/release/reliary`.

## Code style

- **Grammar-free** — no tree-sitter, no language-specific parsers in the core path. New modules must work on any text file.
- **No keyword matching in scoring** — the structural detector (`crates/reliary-search/src/classify_structural`) uses block-boundary detection and last-identifier-before-delimiter rules, not language keywords. If you need language detection, use the file extension as a signal, not the grammar.
- **Lazy tables** — derived tables (`occurrence`, `block`) populate on first query. Don't block the trust path on populating them.
- **Idiomatic Rust** — uses `crates/reliary-output` for `compress_unified` and `compress_sift`. Uses `crates/reliary-core` for shared types. Uses `reliary-search`'s `find_references_*` family for symbol queries (don't reinvent).

## Running tests

```bash
cargo test --workspace --release
```

**Test layers.** Unit tests live beside the code in each crate. Integration and
end-to-end tests live in `crates/*/tests/`:

| File | What it covers |
|------|----------------|
| `reliary-agent/tests/e2e_mcp.rs` | Full MCP stdio protocol: handshake, `tools/list` schema, every tool callable, error codes, malformed-input survivability (including the 10MB line cap), parallel calls |
| `reliary-agent/tests/e2e_cli.rs` | CLI subprocess behaviour: `trust`, `search`, `verify`, `status --path`, `doctor` (+`--fix`), `wrap`, `cache-retrieve` (DB + tee fallback), `init`/`uninstall` against a fake `HOME`, completions |
| `reliary-agent/tests/e2e_adversarial.rs` | Hostile input: invalid UTF-8, NUL bytes, unterminated strings/comments, 200 KB lines, deep nesting, path traversal, SQL-injection-shaped symbol names, index integrity, determinism |
| `reliary-agent/tests/s2_pack_query.rs`, `s3_warmup.rs`, `common/` | Pack-query behaviour, index warmup, shared fixtures and the live MCP client harness |

The e2e tests spawn the real `reliary` binary and require `git` on `PATH`
(they create throwaway repos). Skip them when iterating on a single module:

```bash
cargo test -p reliary-search            # one crate
cargo test -- --skip e2e_               # unit tests only
```

**Adding tests.** New functionality gets a unit test in the same module. New
*integrating* behaviour (a new MCP tool, a new CLI command, a new failure mode)
gets an e2e test in the matching file above — the e2e layer is what catches
protocol and process-boundary regressions that unit tests cannot see.

**Docker integration tests.** Two container layers exercise the shipped binary
and a real agent client end to end:

```bash
make e2e            # Layer 1 — offline, keyless: the binary in a clean container
make e2e-opencode   # Layer 2 — a real OpenCode agent driving reliary's MCP tools
```

- **Layer 1** (`Dockerfile.e2e`, `scripts/e2e_docker_checks.sh`) runs the
  installed binary with `--network none`: install → `trust` → `search` → MCP
  handshake/`tools/call` → `init`/`uninstall` round-trip. It gates every push in
  CI and needs no secret.
- **Layer 2** (`scripts/e2e_opencode.sh`, `.github/workflows/e2e-opencode.yml`)
  installs OpenCode, wires reliary via `reliary init`, and runs `opencode run`
  headlessly with a **keyless free model**. It asserts the agent actually called
  a `reliary_*` tool and answered correctly, plus a negative control proving an
  unwired project calls no reliary tools. It needs network + the free tier, so it
  runs nightly and on `workflow_dispatch`, not per push. No API key is used:
  with `OPENCODE_API_KEY` unset, OpenCode's free tier is anonymous.

Both layers assert real behaviour and carry negative controls; neither may skip
silently. If Layer 2 cannot run (no network/model), remove the job rather than
let it report green.

**The OpenCode plugin** has its own suite:

```bash
cd opencode-plugin && npm ci && npm test
```

## Bench scripts

Bench scripts live in `bench/`. They aren't part of the default `cargo test` run; invoke them directly:

```bash
python3 bench/long_session_bench.py --conditions A,B --seeds 42
```

Reproducing benchmark results requires a `DEEPSEEK_API_KEY` env var.

## Commit message style

We use conventional commits:

- `feat:` for new features
- `fix:` for bug fixes
- `perf:` for performance changes
- `refactor:` for code restructuring (no behavior change)
- `docs:` for documentation
- `chore:` for maintenance

One change per commit. Reference issue numbers at the end of the commit message if applicable.

## Pull request process

1. Create a feature branch from `master-rebuild`. (Note: a fresh clone checks
   out `master` by default — an older line. All current work lands on
   `master-rebuild`.)
2. Make focused commits with clear messages.
3. Run `cargo test --workspace --release` and `cargo clippy --workspace --all-targets --release -- -D warnings`.
4. Update `CHANGELOG.md` for any user-visible change.
5. Open a PR with a clear description of what the PR changes and why.

## Reporting bugs

Open a GitHub issue. Include:

- Reliary version (`reliary --version`)
- OS and architecture
- Reproduction steps
- Relevant `reliary doctor` output

## Feature requests

Open a GitHub issue with the label `enhancement`. Explain the use case and the proposed solution. We prioritize features that make the tool work better for the existing users, not features that add new agent integration paths.

## License

By contributing, you agree that your contributions will be licensed under the MIT License, matching the project's existing license.
