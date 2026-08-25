// index.ts — Reliary OpenCode plugin entrypoint.
//
// Implements the `tool.execute.after` hook to keep the holographic pack fresh
// after file edits. Mirrors the `gate.js` v0.8.0 behavior for the Pi-agent flow
// but is wired into the OpenCode plugin SDK.
//
// Plugin is registered via `~/.config/opencode/opencode.json`:
//   { "plugin": ["./path/to/reliary-opencode-plugin"] }
//
// or globally: `npm install -g .` from this directory.
//
// Opt-in: regen-on-edit is gated by `RELIARY_PACK_REGEN_ON_EDIT=1`. The
// reindex step is always run (cheap, fast, and keeps queries honest); the
// full pack regen is opt-in because it's slow (5-10s for large repos).

import { onFileEdit, dispose } from './regen.js';

const WRITE_TOOLS = ['write', 'edit', 'create', 'replace', 'patch', 'str_replace_edit'];

import type { Hooks, Plugin, PluginModule } from '@opencode-ai/plugin';

const ReliaryOpencodePlugin: Plugin = async () => {
  // Register disposal on exit so pending reindex ops complete.
  process.on('exit', dispose);

  const hooks: Hooks = {
    'tool.execute.after': async (input, _output): Promise<void> => {
      const tool = (input.tool ?? '').toLowerCase();
      if (!WRITE_TOOLS.includes(tool)) return;
      const args = (input.args ?? {}) as { file?: unknown; path?: unknown; filePath?: unknown };
      const candidate = args.file ?? args.path ?? args.filePath;
      if (typeof candidate !== 'string') return;
      try {
        onFileEdit(candidate);
      } catch (e) {
        console.error('[reliary-opencode] onFileEdit failed:', e);
      }
    },
  };
  return hooks;
};

// Module shape opencode expects. The `id` is what shows up in logs.
export const PluginModule: PluginModule = {
  id: 'reliary-opencode',
  server: ReliaryOpencodePlugin,
};

export default ReliaryOpencodePlugin;
