// Integration test: triggerReindex must actually spawn `reliary reindex-file`
// against a real index and change index state. The pure-function tests in
// regen.test.ts cannot catch a broken spawn (wrong flag, detached process that
// never runs, binary not found). This one drives the real binary.
//
// Requires RELIARY_TEST_BIN to point at a built `reliary` binary. The test
// skips (with a loud message) when it is unset, so `npm test` still works for
// a developer who has not built the workspace — but CI sets it, and the CI
// step asserts the suite actually ran (an all-skipped run fails).

import { describe, it, expect } from 'vitest';
import { spawnSync } from 'child_process';
import { mkdtempSync, writeFileSync, mkdirSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { triggerReindex, findProjectRoot } from '../src/regen.js';

const BIN = process.env.RELIARY_TEST_BIN;

function run(bin: string, args: string[], cwd?: string) {
  return spawnSync(bin, args, { encoding: 'utf-8', cwd });
}

describe.runIf(BIN)('triggerReindex (real binary)', () => {
  it('finds the project root and reindexes an edited file', async () => {
    const root = mkdtempSync(join(tmpdir(), 'reliary-regen-'));
    mkdirSync(join(root, 'src'), { recursive: true });
    writeFileSync(join(root, 'src', 'lib.rs'), 'pub fn alpha() -> i32 { 1 }\n');
    writeFileSync(join(root, 'src', 'beta.rs'), 'pub fn beta() -> i32 { alpha() }\n');

    const trust = run(BIN!, ['trust', root], root);
    expect(trust.status, `trust failed: ${trust.stderr}`).toBe(0);
    expect(findProjectRoot(join(root, 'src', 'lib.rs'))).toBe(root);

    // Add a new symbol, then let the hook reindex that one file.
    writeFileSync(
      join(root, 'src', 'lib.rs'),
      'pub fn alpha() -> i32 { 1 }\npub fn gamma_delta() -> i32 { 7 }\n',
    );

    const before = run(BIN!, ['search', 'gamma_delta'], root);
    // The fresh symbol is not in the index yet, so search finds no file.
    expect(before.stdout).toContain('No results found');

    const scheduled = triggerReindex(join(root, 'src', 'lib.rs'), { bin: BIN });
    expect(scheduled).toBe(true);

    // The spawn is detached; poll briefly for the index to update. `search`
    // returns matching *files*, so the symbol being indexed shows up as the
    // file reappearing in the result set.
    let found = false;
    for (let i = 0; i < 40; i++) {
      const after = run(BIN!, ['search', 'gamma_delta'], root);
      if (after.stdout.includes('lib.rs')) { found = true; break; }
      await new Promise((r) => setTimeout(r, 250));
    }
    expect(found, 'reindex-file did not make the new symbol searchable').toBe(true);
  }, 30000);

  it('returns false when no index exists and does not spawn', () => {
    const root = mkdtempSync(join(tmpdir(), 'reliary-noindex-'));
    writeFileSync(join(root, 'lib.rs'), 'pub fn alpha() {}\n');
    // No `.reliary/index.sqlite` anywhere up the tree from a temp dir (the
    // system temp root has none), so the hook must decline.
    expect(findProjectRoot(join(root, 'lib.rs'))).toBeNull();
    expect(triggerReindex(join(root, 'lib.rs'), { bin: BIN })).toBe(false);
  });
});

describe.runIf(!BIN)('triggerReindex (skipped)', () => {
  it('RELIARY_TEST_BIN not set', () => {
    // Loud skip: CI sets RELIARY_TEST_BIN and asserts this suite did not skip.
    console.warn(
      'SKIPPED: set RELIARY_TEST_BIN=/path/to/reliary to run the real-reindex test',
    );
    expect(typeof triggerReindex).toBe('function');
  });
});
