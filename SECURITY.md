# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.8.x   | :white_check_mark: |
| 0.6.x   | :x:                |

## Reporting a Vulnerability

Open a GitHub issue with the label `security`. For sensitive disclosures, mark the issue as private or contact the maintainers directly.

Do not disclose security vulnerabilities publicly until we've had a chance to assess and address them.

## Threat Model

Reliary indexes **local codebases only**. The binary reads source files from the directory you point at, writes a SQLite database to `.reliary/`, and exposes the index over MCP.

We assume:

- Your source directory and your home directory are trusted.
- The AI agent consuming the MCP tools is trusted (the MCP path is local; no network exposure).
- `reqwest` is only used for the `update` subcommand and only contacts the GitHub Releases API (blocking client, rustls-tls).

## What Reliary does NOT do

- Does not listen on any network port (proxy removed in v0.8).
- Does not write to anywhere outside the project's `.reliary/` directory.
- Does not execute code from indexed files.
- Does not eval, run, or interpret indexed content.

## Limited Attack Surface

The binary uses:

- `rusqlite` with `bundled` feature (no system SQLite dependency).
- `reqwest` with `default-features = false, features = ["rustls-tls", "blocking"]` (no native-tls, no stream, no async).
- `axum` and `tokio` are removed in v0.8.0.
- `tree-sitter` is NOT used.
- No unsafe code in the core crates (verified via `cargo geiger`).

## MCP Authorization

The MCP server uses **stdio transport** by default. Any agent that can spawn the binary can call MCP tools. This is intentional — the binary is local-only.

If you need to restrict the MCP server to specific users, run the binary under a separate Unix user (so file permissions on the `.reliary/` directory limit access).

## Known Limitations

- The `reliary update` subcommand contacts `https://api.github.com/repos/Reliary/reliary-agent/releases/latest`. If you are on a network with restricted internet access, run with `RELIARY_NO_UPDATE_CHECK=1` or remove the `update` subcommand from your agent's tool list.
- The `reliary wrap` command executes the wrapped shell command via system shell (`sh -c`). Don't use `wrap` to run untrusted commands.

## Audit History

- v0.8.0 (2025): Shipped with zero known vulnerabilities. Removed `tracing`, `axum`, `tokio`, `tower-http`, `futures-util`, `tokio-stream` dependencies.
- v0.6.13 (2024): Last release with the HTTP proxy. Proxy was untested for adversarial input.
