# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in reliary-agent, please report it privately.

**Do not open a public GitHub issue.**

Contact: security@reliary.dev

We will acknowledge receipt within 48 hours and provide an estimated timeline for a fix.

## Scope

- The `reliary-agent` binary (daemon, proxy, CLI, MCP server)
- `pi/gate.js` extension
- Build and release infrastructure

## Out of Scope

- Dependencies with known CVEs (tracked via `cargo audit`)
- LLM provider API key security (user responsibility)

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.x     | ✅ |
