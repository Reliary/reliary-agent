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
    "taskTargets": false,
    "priorInjection": false
  }
}
```

## Modes

| Mode | Bash/write/grep | Safety escalation | Best for |
|------|----------------|-------------------|----------|
| `fast` | Pass through | None | Efficient models (Qwen, Nemotron) |
| `reactive` | Pass through until trigger | Escalates on unsafe behavior | Lower-variance models |
| `strict` (default) | Transparent redirect | Redirects to sandbox tools (auto-deescalates) | High-variance models (DeepSeek) |

## Features

| Feature | Default | What it does |
|---------|---------|-------------|
| `compress` | true | IR reasoning compression (~40% token savings, zero daemon needed) |
| `convWindow` | true | Drop old verbose tool results from conversation at 10+ messages |
| `readEnrichment` | true | Compress non-target file reads with zone truncation |
| `editMerge` | false | Combine sequential edits to same file into one operation (regresses on high-variance models) |
| `taskTargets` | false | Preserve full content of task-targeted files |
| `priorInjection` | false | Inject chronicle edit history into system prompt (adds prompt overhead) |

## Environment Variables

| Variable | Values | Description |
|----------|--------|-------------|
| `RELIARY_MODE` | `fast`, `reactive`, `strict` | Override safety mode |
| `RELIARY_FEATURES` | `+editMerge,-taskTargets` | Enable/disable individual features |
| `RELIARY_REPLAY` | `record`, `replay` | Deterministic benchmark mode |
| `RELIARY_UPSTREAM_URL` | URL | Override auth-based routing. Default: resolved from `Authorization` header |
| `RELIARY_PROXY_GUARD_DISABLE` | `1` | Disable guard (cross-file edit safety). On by default. |
| `RELIARY_PROXY_ANTI_DISABLE` | `1` | Disable anti-decision (sticky identifier failure memory) in proxy. On by default. |
| `RELIARY_PROXY_OUTPUT_COMPRESS` | `1` | Enable first-appearance freeze compression in proxy. On by default. |
| `DEEPSEEK_BASE_URL` | URL (default: DeepInfra) | Route Pi/Cline/OpenCode through proxy: `http://localhost:9090/v1` |
| `ANTHROPIC_BASE_URL` | URL (default: Anthropic) | Route Claude Code through proxy: `http://localhost:9090/` |

## Feature Toggle Syntax

`RELIARY_FEATURES` uses `+name` to enable and `-name` to disable features relative
to config file defaults:

```bash
# Enable editMerge (default: off), disable priorInjection (default: off)
RELIARY_FEATURES=+editMerge,-priorInjection

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
