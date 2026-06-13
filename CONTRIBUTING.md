# Contributing

## Build

```bash
cargo build --release
```

## Test

```bash
cargo test --release
```

## PR Workflow

1. Branch from master: `git checkout -b feature-name`
2. Make changes, keeping commits small and focused
3. Run `cargo test --release` — all tests must pass
4. Run `cargo clippy -- -D warnings` — no new warnings
5. Verify pre-commit hook passes: `.githooks/pre-commit` checks gitleaks, cargo audit, cargo deny
6. Open PR against master with a clear description

## Code Style

- Grammar-free over everything — zero AST, zero tree-sitter, zero language detection
- 4-space indentation in Rust, 2-space in JavaScript
- No `.unwrap()` in production code (use `?` or `.ok()`)
- Error messages should be actionable: "ERROR: cannot read file" not "Error 5"
- Dead code is deleted, not commented out

## Architecture

9 crates in `crates/`, one binary:

| Crate | Purpose |
|---|---|
| `reliary-search` | BM25 + FTS5 indexing |
| `reliary-compress` | Reasoning compression |
| `reliary-sift` | Structural compression |
| `reliary-risk` | Pre-edit risk analysis |
| `reliary-memory` | Cross-session memory |
| `reliary-fix` | Pattern extraction + editing |
| `reliary-dead` | Dead code analysis |
| `reliary-core` | Shared session types |
| `reliary-agent` | Binary — daemon, proxy, CLI, MCP |

## Release

Maintained by Reliary maintainers. Tag `v*` triggers CI release.

## Code of Conduct

Be excellent to each other. This is a small tool for developers.
