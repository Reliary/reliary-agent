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
| `reactive` (default) | Pass through until trigger | Escalates on unsafe behavior | Most users |
| `strict` | Blocked | Always on | High-variance models (DeepSeek) |

## Features

| Feature | Default | What it does |
|---------|---------|-------------|
| `compress` | true | IR reasoning compression (~40% token savings, zero daemon needed) |
| `convWindow` | true | Drop old verbose tool results from conversation at 10+ messages |
| `readEnrichment` | true | Compress non-target file reads with zone truncation |
| `editMerge` | false | Combine sequential edits to same file into one operation |
| `taskTargets` | false | Preserve full content of task-targeted files (skip compression) |
| `priorInjection` | false | Inject chronicle edit history into system prompt |

## Environment Variables

| Variable | Values | Description |
|----------|--------|-------------|
| `RELIARY_MODE` | `fast`, `reactive`, `strict` | Override safety mode |
| `RELIARY_FEATURES` | `+editMerge,-taskTargets` | Enable/disable individual features |
| `RELIARY_REPLAY` | `record`, `replay` | Deterministic benchmark mode |
| `DEEPSEEK_BASE_URL` | URL (default: DeepInfra) | When using proxy, point at `http://localhost:9090/v1` |
| `PI_SESSION_FILE` | path | Pi session file path (used for session state) |

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
