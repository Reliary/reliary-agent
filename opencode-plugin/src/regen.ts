// regen.ts — File-edit regen hook for reliary-opencode plugin.
//
// Mirrors gate.js v0.8.0 (crates/reliary-agent/pi/gate.js:69-105): after
// a file-modifying tool writes a file, run `reliary reindex-file` to
// keep the lazy occurrence table current, and optionally regenerate
// the holographic pack so subsequent `pack_query` calls see fresh data.
//
// All failures are soft: the user gets verbose logs but the model
// session is never blocked by an indexing hiccup.

import { spawn } from 'child_process';
import { existsSync } from 'fs';
import { dirname, resolve, join } from 'path';

interface RegenOptions {
  /**
   * Reliary binary. Env var RELIARY_BIN takes priority, then which, then nothing.
   */
  bin?: string;

  /**
   * Hook only fires for files matching this regex.
   * Defaults to source-code extensions only (Rust, Python, TS, Go, etc.).
   */
  filePattern?: RegExp;
}

export const DEFAULT_FILE_PATTERN = /\.(rs|py|ts|tsx|go|c|h|cpp|hpp|js|jsx|java|rb|swift)$/i;

function discoverReliary(): string | null {
  if (process.env.RELIARY_BIN && process.env.RELIARY_BIN.trim()) {
    return process.env.RELIARY_BIN.trim();
  }
  // H13: walk PATH directly — no subprocess spawn. Synchronous existsSync
  // is fast (stat syscall) and safe even when PATH contains slow mounts.
  const pathDirs = (process.env.PATH || '').split(':');
  for (const name of ['reliary', 'reliary-agent']) {
    for (const dir of pathDirs) {
      const candidate = dir ? dir + '/' + name : name;
      if (existsSync(candidate)) return candidate;
    }
  }
  return null;
}

/**
 * Walk upward from a file path looking for `.reliary/index.sqlite`.
 * Mirrors gate.js v0.8.0 line 86-97: up to 20 levels of parent dirs.
 */
export function findProjectRoot(filePath: string): string | null {
  if (!filePath) return null;
  let dir = dirname(resolve(filePath));
  for (let i = 0; i < 20; i++) {
    if (existsSync(join(dir, '.reliary', 'index.sqlite'))) return dir;
    const parent = dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return null;
}

// H4: debounce queue. Rapid file edits (bulk find-and-replace) coalesce
// into a single reindex per debounce window (default 500ms).
let pendingReindex: NodeJS.Timeout | null = null;
let pendingFiles: Set<string> = new Set();
const DEBOUNCE_MS = 500;

function flushReindex(opts: RegenOptions): void {
  if (pendingFiles.size === 0) return;
  const files = Array.from(pendingFiles);
  pendingFiles.clear();
  pendingReindex = null;
  for (const f of files) {
    triggerReindex(f, opts);
  }
}

/**
 * Trigger `reliary reindex-file` after a write/edit. Best-effort, never blocks.
 *
 * Returns true if reindex was scheduled, false if it failed or was skipped.
 */
export function triggerReindex(filePath: string, opts: RegenOptions = {}): boolean {
  const bin = opts.bin ?? discoverReliary();
  if (!bin) return false;

  // HK4: skip if no .reliary/index.sqlite exists
  if (!findProjectRoot(filePath)) return false;

  try {
    const child = spawn(bin, ['reindex-file', '--', filePath], {
      stdio: 'ignore',
      detached: true,
    });
    // HK5: log spawn errors instead of silent swallow
    child.on('error', (err) => {
      console.error(`[reliary] reindex spawn failed for ${filePath}: ${err.message}`);
    });
    child.unref();
    return true;
  } catch (err) {
    console.error(`[reliary] reindex sync error for ${filePath}: ${err}`);
    return false;
  }
}

/**
 * Trigger full holographic pack regeneration. Heavy: 5-10s for large repos.
 * Opt-in via RELIARY_PACK_REGEN_ON_EDIT=1 (matches gate.js convention).
 *
 * Returns true if regen succeeded, false if skipped or failed.
 */
export function triggerPackRegen(
  filePath: string,
  opts: RegenOptions = {},
): boolean {
  if (process.env.RELIARY_PACK_REGEN_ON_EDIT !== '1') return false;

  const bin = opts.bin ?? discoverReliary();
  if (!bin) return false;

  const projectRoot = findProjectRoot(filePath);
  if (!projectRoot) return false;

  try {
    const child = spawn(
      bin,
      ['pack', projectRoot, '--format', 'l2l3', '--strategy', 'full'],
      { stdio: 'ignore', detached: true },
    );
    child.on('error', (err) => {
      console.error(`[reliary] pack-regen spawn failed for ${filePath}: ${err.message}`);
    });
    child.unref();
    return true;
  } catch (err) {
    console.error(`[reliary] pack-regen sync error for ${filePath}: ${err}`);
    return false;
  }
}

/**
 * Edit-hook entrypoint. Should run on file-edit tool calls (`write`, `edit`).
 *
 * Uses a debounced reindex queue: rapid edits coalesce into a single reindex
 * per debounce window. Returns object describing what was triggered.
 */
export function onFileEdit(
  filePath: string,
  opts: RegenOptions = {},
): { reindexed: boolean; regenerated: boolean } {
  if (!filePath || typeof filePath !== 'string') return { reindexed: false, regenerated: false };

  const pattern = opts.filePattern ?? DEFAULT_FILE_PATTERN;
  if (!pattern.test(filePath)) return { reindexed: false, regenerated: false };

  const bin = opts.bin ?? discoverReliary();
  if (!bin) return { reindexed: false, regenerated: false };
  const resolvedOpts = { ...opts, bin };

  // H4: debounce reindexes — coalesce rapid edits
  pendingFiles.add(filePath);
  if (pendingReindex) clearTimeout(pendingReindex);
  pendingReindex = setTimeout(() => flushReindex(resolvedOpts), DEBOUNCE_MS);

  // Pack regen is heavy — keep immediate fire for the first edit, ignore rest
  const regenerated = triggerPackRegen(filePath, resolvedOpts);
  return { reindexed: true, regenerated };
}

/**
 * Dispose hook: flushes pending reindexes and clears state.
 * Called on process exit / SIGINT / hot-reload.
 */
export function dispose(): void {
  if (pendingReindex) {
    clearTimeout(pendingReindex);
    pendingReindex = null;
  }
  // Flush remaining pending files synchronously (best-effort)
  if (pendingFiles.size > 0) {
    const files = Array.from(pendingFiles);
    pendingFiles.clear();
    for (const f of files) {
      try {
        const bin = discoverReliary();
        if (!bin) continue;
        spawn(bin, ['reindex-file', '--', f], { stdio: 'ignore', detached: true }).unref();
      } catch {}
    }
  }
}

// Default export for the opencode plugin loader
export default { onFileEdit, triggerReindex, triggerPackRegen, findProjectRoot };
