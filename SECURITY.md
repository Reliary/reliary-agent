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

- LLM provider API keys (user-managed)
- Third-party dependencies (managed via cargo-deny and Dependabot)

## Branch Protection

The `master` branch has the following protection rules applied via GitHub UI:

- Require pull request reviews before merging (1 approval minimum)
- Dismiss stale reviews on new pushes
- Require status checks: CI (guardrails), Hardening
- Require signed commits
- Restrict direct pushes (only admins bypass)

These are configured at:
  https://github.com/Reliary/reliary-agent/settings/branches/master

## Security Practices

- **SAST**: CodeQL runs on every push/PR (`.github/workflows/codeql-analysis.yml`)
- **Dependency scanning**: Dependabot + Renovate
- **Supply chain**: `cargo-deny` advisories, sources, and bans
- **Signed releases**: Cosign-signed tarballs and checksums
- **Secrets scanning**: Gitleaks on every PR diff
- **Least privilege**: All workflow tokens scoped to minimum
