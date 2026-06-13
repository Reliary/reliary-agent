# gate.js — Pi Agent Extension

gate.js is the Pi Agent extension that provides reasoning compression,
tool interception, and safety features. It lives at `pi/gate.js` in this repo
and is embedded in the binary at build time (via `include_str!` in `init.rs`).

## How It Works

gate.js registers three Pi event hooks:

| Hook | Purpose |
|---|---|
| `handleToolCall` | Intercepts bash/edit/write calls, routes through heal-apply |
| `handleToolResult` | Compresses tool output, extracts structured read summaries |
| `handleBeforeProviderRequest` | Compresses reasoning, collapses conversation window, injects prior |

## Features

### Reasoning Compression (`compressReasoning`)
Strips LLM fluff patterns ("Let me analyze...", "I need to check...") using
inline JS regex. Preserves code blocks, file paths, and error messages.
~42% output token reduction on average.

### Conversation Window (`applyConversationWindow`)
Drops verbose tool results from turns older than 8. Keeps assistant reasoning
verbatim. Prevents unbounded context growth.

### Self-Healing Edits
Intercepts `edit` tool calls, shadow-applies through the daemon, runs tests,
reverts on failure. Also intercepts `sed -i` commands and `write` on existing files.

### Identifier Veto
Checks newText identifiers against the FTS5 index. Blocks edits referencing
hallucinated API names.

### Bash Guard
Routes `sed -i` through heal-apply. Blocks destructive patterns (`rm -rf /`).
Test commands (`cargo test`, `pytest`) pass through.

## Mode System

| Mode | Bash/write | Safety escalation | Best for |
|---|---|---|---|
| `fast` | Pass through | None | Efficient models, trusted envs |
| `reactive` (default) | Monitor, escalate | Escalates on unsafe patterns | Most users |
| `strict` | Blocked | Always on | High-variance models |

Set via `RELIARY_MODE=fast` or `reliary-agent config mode fast`.

## Config Cascade

1. `RELIARY_FEATURES` env var (`+compress,-convWindow`)
2. `RELIARY_MODE` env var
3. `./.reliary/config.json` (project)
4. `~/.reliary/config.json` (user)
5. Built-in defaults

## Event Shapes (Pi SDK)

```js
// tool_call hook
event.toolName   // string: "read", "edit", "bash", "write", "grep"
event.input      // object: tool arguments
// Return { block: true, response: "message" } to intercept

// tool_result hook
event.toolName   // string
event.content    // array of { type: "text", text: "..." }
event.isError    // boolean
// Return modified event.content to transform output

// before_provider_request hook
event.payload    // { model, messages, tools, ... }
// Return modified event.payload
```

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| "daemon not reachable" | Daemon not running | Start `reliary-agent serve` |
| "unknown api key" | Proxy has no route for auth key | Run `reliary-agent init` |
| No compression | Wrong mode | Set `RELIARY_MODE=fast` |
| Edits not healing | Daemon not running | Check `reliary-agent doctor` |
