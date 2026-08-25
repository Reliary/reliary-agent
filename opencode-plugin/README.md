# reliary-opencode-plugin

OpenCode plugin that keeps the [Reliary](https://github.com/reliary) holographic pack fresh after file edits.

## What it does

In OpenCode's `tool.execute.after` hook, this plugin:

1. **Reindexes** the edited file (`reliary reindex-file <path>`) — fast, always on
2. **Regenerates** the holographic pack (`reliary pack ...`) — opt-in via `RELIARY_PACK_REGEN_ON_EDIT=1`

Mirrors the behavior of `gate.js` v0.8.0 (the Pi-agent extension that lives at `crates/reliary-agent/pi/gate.js` in the reliability repo).

## Install

### Option 1 — `reliary init` (preferred)

Run `reliary init` from any directory; you'll be prompted:

```
? Found OpenCode config. Add Reliary MCP server? Yes
? Install reliary-opencode plugin (auto-reindex after edits)? Yes
✓ Updated opencode.json
✓ Installed reliary-opencode plugin
```

The plugin path is appended to the `"plugin"` array in `~/.config/opencode/opencode.json`.

### Option 2 — Manual

```bash
cd /path/to/reliary8/opencode-plugin
npm install                # one-time
npm run build              # tsup → dist/
npm pack                   # → reliary-opencode-plugin-0.1.0.tgz
npm install -g ./reliary-opencode-plugin-0.1.0.tgz
```

Then in `~/.config/opencode/opencode.json`:
```json
{
  "plugin": ["./reliary-opencode-plugin"]
}
```

## Configuration

| Env var | Default | Effect |
|---|---|---|
| `RELIARY_BIN` | (uses `which reliary`) | Path to the `reliary` binary |
| `RELIARY_PACK_REGEN_ON_EDIT` | unset (OFF) | When `1`, regenerates the entire pack (~5s) after each edit |

## How it works

The plugin subscribes to OpenCode's `tool.execute.after` hook. On `write` and `edit` tool results:

```
1. Extract file path from tool input.args (.file / .path / .filePath)
2. Skip if file doesn't match source-code extensions
3. Walk up looking for .reliary/index.sqlite (project root)
4. Run `reliary reindex-file <path>` — populates lazy occurrence table
5. If RELIARY_PACK_REGEN_ON_EDIT=1, run `reliary pack ...` — fresh cache
```

All failures are **soft** — the user gets verbose stderr but the agent session is never blocked.

## Verification

Run the S5 end-to-end test from the parent `reliary8` repo:

```bash
cd $HOME/src/reliary8
python3 bench/test_s5_end_to_end.py
```

Expected: 4/4 steps PASSED.

## License

Same as parent project (Reliary).
