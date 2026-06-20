# Configuration

reliary-agent uses a cascade of configuration sources (highest priority first):

1. `RELIARY_MODE` and `RELIARY_FEATURES` environment variables
2. `./.relconf.json` — project config (can be checked into version control)
3. `./.reliary/config.json` — project config (included in `.reliary/` directory)
4. `~/.reliary/config.json` — global user config (applies to all projects)

## Example: `.relconf.json`

```json
{
  "mode": "reactive",
  "features": {
    "compress": true,
    "convWindow": true,
    "readEnrichment": true,
    "editMerge": false,
    "healEdit": true,
    "priorInjection": false
  }
}
```

## Modes

| Mode | Bash/write/grep | Safety escalation | Best for |
|------|----------------|-------------------|----------|
| `fast` | Pass through | None | Efficient models (Qwen, Nemotron) |
| `reactive` | Pass through until trigger | Escalates on unsafe behavior | Most models |
| `strict` (default) | Transparent redirect | Bash/write/grep redirected to sandbox tools, auto-deescalates after 5 redirects | High-variance models (DeepSeek) |

## Features

| Feature | Default | What it does |
|---------|---------|-------------|
| `compress` | true | IR reasoning compression (zero daemon needed) |
| `convWindow` | true | Drop old verbose tool results from conversation at 10+ messages |
| `readEnrichment` | true | Compress non-target file reads with zone truncation |
| `editMerge` | false | Combine sequential edits to same file into one operation (regresses on high-variance models) |
| `healEdit` | true | Self-healing: shadow-apply edits, run tests, revert on failure |
| `priorInjection` | false | Inject chronicle edit history into system prompt (adds prompt overhead) |

## Environment Variables

| Variable | Values | Description |
|----------|--------|-------------|
| `RELIARY_MODE` | `fast`, `reactive`, `strict` | Override safety mode |
| `RELIARY_FEATURES` | `+editMerge,-healEdit` | Enable/disable individual features |
| `RELIARY_REPLAY` | `record`, `replay` | Deterministic benchmark mode |
| `RELIARY_UPSTREAM_URL` | URL | Fallback upstream URL for unknown API keys |
| `RELIARY_PROXY_GUARD_DISABLE` | `1` | Disable guard (cross-file edit safety). On by default. |
| `RELIARY_PROXY_FEATURE_ANTI` | `1` | Enable anti-decision (sticky identifier failure memory). Off by default. |
| `RELIARY_PROXY_NOVEL_COMPRESS` | `0` | Disable first-appearance freeze + novel mechanisms (Maxwell, DSL, hoisting). On by default. |
| `RELIARY_PROXY_WC_RATIO` | `2` | Weighted-cost multiplier on output tokens (DeepSeek V4 Flash is 1:2, Claude/GPT-4 is 1:4). |

## Agent Setup Examples (Proxy Routing)

To route your agent's API traffic through the proxy for conversation compression, set these environment variables in your shell before launching your agent:

| Agent | Environment Variable | Value |
|---|---|---|
| **Pi / Cline / OpenCode** | `*_BASE_URL` (e.g. `OPENAI_BASE_URL`) | `http://localhost:9090/v1` |
| **Claude Code** | `ANTHROPIC_BASE_URL` | `http://localhost:9090/` |

## Feature Toggle Syntax

`RELIARY_FEATURES` uses `+name` to enable and `-name` to disable features relative
to config file defaults:

```bash
# Enable editMerge (default: off), disable healEdit (default: on)
RELIARY_FEATURES=+editMerge,-healEdit

# Disable read enrichment (default: on)
RELIARY_FEATURES=-readEnrichment
```

## Config File Discovery

The config cascade resolves at gate.js load time and at each daemon command.
File order:

1. `./.relconf.json` — checked first nearest to CWD
2. `./.reliary/config.json` — inside the project index directory
3. `~/.reliary/config.json` — user home directory

Values from higher-priority sources merge over lower-priority sources.
Environment variables always win.
