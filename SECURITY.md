# Security Policy

## Reporting a vulnerability

Open a private security advisory at
<https://github.com/Reliary/reliary-agent/security/advisories/new>, or email
`security@reliary.dev`. We aim to acknowledge within 72 hours. Please do not
open a public issue for an unpatched vulnerability.

Supported versions: the latest minor release receives fixes. Older minors are
not maintained.

## Threat model

Reliary is a local code-intelligence tool. It reads source files and answers
queries over a local SQLite index. It has no server component and listens on no
network port.

We assume:

- Your source directory and your home directory are trusted.
- The AI agent consuming the MCP tools is trusted (stdio transport only; the
  agent that can spawn the binary can call the tools — this is intentional).
- Indexed repository content is **untrusted** — the parser must not be
  exploitable by hostile source files.

## Network egress

The binary makes outbound HTTPS requests in exactly two places:

| Command | Destination | Purpose |
|---------|-------------|---------|
| `reliary update` | `api.github.com`, `github.com` | Fetch the latest release metadata and download the release tarball. The download is verified against the SHA-256 digest GitHub publishes for the asset before extraction; a mismatch aborts the install. |
| `reliary fix` (LLM fallback) | `api.deepseek.com` (hardcoded; auth via `DEEPSEEK_API_KEY`) | Send the task description, indexed code excerpts, and tool results to the LLM endpoint. This runs **automatically** whenever the task does not match a built-in deterministic recipe (doc comment, rename, unused removal, import addition, compiler-error fix). There is no flag that gates it — do not run `fix` on code you are not permitted to transmit. |

No other subcommand performs network I/O. `reqwest` is configured with
`default-features = false, features = ["rustls-tls", "blocking"]` — no
native-tls, no async runtime in the request path.

## Filesystem writes

Outside the project's `.reliary/` directory, the binary writes only when you
run `reliary init` (or explicitly install hooks). The full list:

| Path | Written by | Removed by |
|------|-----------|------------|
| `<project>/.reliary/index.sqlite` (+ `-wal`, `-shm`) | `trust`, `reindex-file`, watcher | `clean` |
| `~/.claude.json` | `init` (adds an MCP server entry) | `uninstall` |
| `~/.claude/settings.json` | `init` (adds a PreToolUse hook entry) | `uninstall` |
| `~/.claude/hooks/reliary-*.sh` | `init` | `uninstall` |
| `~/.config/opencode/opencode.json` | `init` (adds an MCP entry + plugin) | `uninstall` |
| `~/.config/Code/User/globalStorage/rooveterinery.cline/cline_mcp_settings.json` | `init` | `uninstall` |
| `~/.local/share/reliary/gate.js` | `init` (Pi Agent extension) | `uninstall` |
| `/tmp/reliary-tee/<hash>` | sift (`reliary wrap`) — full uncompressed command output, for recovery | `reliary clean --global` |
| private staging dir under the system temp dir | `update` (removed on all exit paths) | `update` |

`uninstall` removes every entry it added. All config writes are atomic
(temp file + rename) and preserve unrelated keys.

## What Reliary does NOT do

- Does not listen on any network port.
- Does not execute code from indexed files.
- Does not eval, run, or interpret indexed content.
- Does not send telemetry.
- Does not write secrets into the index. (API keys are read from the
  environment or the agent's auth file and never persisted by reliary.)

## Memory safety

Every first-party crate root carries `#![forbid(unsafe_code)]`, so the
compiler rejects any `unsafe` block in reliary's own code. CI asserts the
attribute is present on every crate root. This is a stronger and permanently
enforced guarantee than a `cargo geiger` snapshot. Third-party dependencies
(such as `rusqlite`'s bundled SQLite) may contain unsafe code; dependency
advisories are checked in CI with `cargo-deny` on every push and weekly.

## Hardening measures

- SQLite is opened read-only with a 256 MiB mmap window for queries; the
  bundled feature avoids a system SQLite dependency.
- MCP tool handlers validate caller-supplied paths and reject traversal
  outside the project root.
- The `wrap`/sift path bounds its read of child-process output and passes
  source-file readers (`cat`/`head`/`tail`) through uncompressed.
- `init` validates the environment-supplied binary path against a
  metacharacter allowlist before any hook that references it is installed.
- Release binaries are built from tagged commits in CI and published with
  SHA-256 digests (verified by `reliary update`).

## Known limitations

- The stdio MCP transport has no authentication. Any local process that can
  spawn or attach to the agent's stdio can call the tools. Running the agent
  on a multi-tenant host is out of scope.
- `reliary fix` sends code excerpts to a third-party API (DeepSeek) whenever
  the task needs the LLM fallback. The endpoint is not configurable. Do not
  use it on code you are not permitted to transmit.
- `reliary wrap` (sift) writes full uncompressed command output to
  `/tmp/reliary-tee/<hash>`. On a shared host other local users may be able to
  read those files depending on the temp directory's permissions; run
  `reliary clean --global` to remove them.
- Indexed content is treated as untrusted input, but the grammar-free parser
  has not been fuzzed as extensively as a full AST parser would warrant;
  robustness is covered by the adversarial e2e suite (`crates/reliary-agent/
  tests/e2e_adversarial.rs`).
