# Contributing to Reliary

Thanks for your interest in contributing. Reliary is a single-binary Rust tool that indexes codebases via grammar-free structural detection and exposes the index to AI coding agents through the Model Context Protocol (MCP).

## Development setup

Requires Rust 1.74+ (stable).

```bash
git clone https://github.com/Reliary/reliary-agent.git
cd reliary-agent
cargo build --release
```

The binary is built at `target/release/reliary`.

## Code style

- **Grammar-free** — no tree-sitter, no language-specific parsers in the core path. New modules must work on any text file.
- **No keyword matching in scoring** — the structural detector (`crates/reliary-search/src/classify_structural`) uses block-boundary detection and last-identifier-before-delimiter rules, not language keywords. If you need language detection, use the file extension as a signal, not the grammar.
- **Lazy tables** — derived tables (`occurrence`, `block`, `scope_binding`, `method_occurrence`) populate on first query. Don't block the trust path on populating them.
- **Idiomatic Rust** — uses `crates/reliary-output` for `compress_unified` and `compress_sift`. Uses `crates/reliary-core` for shared types. Uses `reliary-search`'s `find_references_*` family for symbol queries (don't reinvent).

## Running tests

```bash
cargo test --workspace
```

All 295+ tests must pass. Add tests for new functionality in the same module as the code (not in `tests/`).

## Bench scripts

Bench scripts live in `bench/`. They aren't part of the default `cargo test` run; invoke them directly:

```bash
python3 bench/long_session_bench.py --conditions A,B --seeds 42
```

Reproducing benchmark results requires a `DEEP_SEEK_API_KEY` env var.

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

1. Create a feature branch from `main`.
2. Make focused commits with clear messages.
3. Run `cargo test --workspace` and `cargo clippy --workspace -- -D warnings`.
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

By contributing, you agree that your contributions will be dual-licensed under MIT OR Apache-2.0 (at the maintainer's option), matching the project's existing license.
