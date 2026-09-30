# reliary-opencode-plugin

OpenCode plugin that keeps the [Reliary](https://github.com/Reliary/reliary-agent)
index fresh after file edits.

## What it does

In OpenCode's `tool.execute.after` hook, this plugin:

1. **Reindexes** the edited file (`reliary reindex-file <path>`) — fast, always on
2. **Regenerates** the pack (`reliary pack ...`) — opt-in via `RELIARY_PACK_REGEN_ON_EDIT=1`

Mirrors `gate.js`, the Pi Agent extension at `crates/reliary-agent/pi/gate.js`.

## Install

### Option 1 — `reliary init` (preferred)

Run `reliary init` from any directory; you'll be prompted:

```
? Found OpenCode config. Add Reliary MCP server? Yes
? Install reliary-opencode plugin (auto-reindex after edits)? Yes
✓ Updated opencode.json
✓ Installed reliary-opencode plugin
```

`init` writes the built plugin to `~/.local/share/reliary/opencode-plugin.js`
and adds that path to the `"plugin"` array in your OpenCode config. No source
tree or npm install is needed.

### Option 2 — Manual (from a source checkout)

```bash
cd opencode-plugin
npm install                # one-time
npm run build              # tsup → dist/index.js
```

Then point your config at the built file:

```json
{
  "plugin": ["/absolute/path/to/opencode-plugin/dist/index.js"]
}
```

## Configuration

| Env var | Default | Effect |
|---|---|---|
| `RELIARY_BIN` | (uses `which reliary`) | Path to the `reliary` binary |
| `RELIARY_PACK_REGEN_ON_EDIT` | unset (OFF) | When `1`, regenerates the pack after each edit |

## How it works

The plugin subscribes to OpenCode's `tool.execute.after` hook. On `write` and
`edit` tool results:

```
1. Extract the file path from the tool args (.file / .path / .filePath)
2. Skip if the file is not source code
3. Walk up to find .reliary/index.sqlite (project root)
4. Run `reliary reindex-file <path>`
5. If RELIARY_PACK_REGEN_ON_EDIT=1, regenerate the pack
```

All failures are soft — the agent session is never blocked.

## Verification

```bash
python3 bench/test_s5_end_to_end.py
```

Expected: 4/4 steps pass.

## License

MIT, same as the parent project.
