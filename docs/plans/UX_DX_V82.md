# UX/DX Plan — P0-P2 (antagonised)

Scope approved: Everything P0-P2. Keep `trust`/`index` separate. Non-TTY prompts fail closed.
Date: 2026-10-16. Branch: `feat/v59-accuracy`. Target: v0.8.11.

## Antagonism (what this plan refuses to do)

1. **No `trust`/`index` merge.** User chose keep-both. `trust` = first-time
   (create dir + `.reliary/.gitignore` + index + validate). `index` = rebuild.
   All hints converge on `trust .` for first run, `index` for rebuild.
2. **No silent scope widening.** `safe_module_filter` mapping an escaping
   filter to `None` (whole codebase, no message) is a lie. It becomes an
   explicit scoped-empty with the valid scope echoed + closest suggestion —
   never a silent whole-codebase dump (Claude Context pattern: successful
   content, actionable message, no retry spiral).
3. **No blind `dead_only` src-narrow removal.** `find_references(dead_only)`
   auto-narrows `path="."` to `src/` while `find_dead_code` treats `.` as whole
   tree. Removing the narrow may regress q10 (which relies on it). Measure q10
   before/after; align both entry points to ONE documented behavior, keep the
   one that scores.
4. **No anchor-convention refactor without fixture proof.** `anchor_file`/
   `anchor_line` indexing is mixed (`mcp.rs:2129,1299` do `al-1`; `2237-2241`,
   `2166-2171` pass raw). Each site gets a fixture assertion (known def at
   known line) BEFORE the change; no site is touched on prose reasoning alone.
   Same V26/V51 off-by-one class — guilty until proven innocent.
5. **No `path`/`path_filter` semantic split in one jump.** `path` is dual-use
   (project selector + module prefix) and `index_root.rs:97-109` falls back
   silently. Splitting semantics in one commit risks breaking every caller.
   Step 1 is document + message the fallback; split only if Step 1 proves
   insufficient.
6. **No exit-code breakage without CHANGELOG.** `update` always-exit-0,
   `search/pack/compress/verify` exit-0-on-no-index become:
   `0` ok / `1` error / `2` no-index-or-update-available(`--check`).
   Documented as breaking for scripts (`set -e`, Dependabot).
7. **No CACHEDIR.TAG, no relocation.** Killed in the `.reliary` antagonism:
   config is local-only, TAG would nuke it on restore; relocation breaks
   walk-up/`nearest_index_root`/`doctor`/roots. Deferred.
8. **No prompt auto-yes anywhere.** Non-TTY + no `--yes` → exit 2 with
   remediation. `echo | reliary clean` must exit 2, never delete.

## Phase 1 — Safety + non-TTY (fail-closed)

- P1.1 Single color gate. Unify `main.rs:30-56` + `ux.rs:25-30` +
  `init.rs:7,56` behind one `colors_enabled()` (`NO_COLOR` + `TERM=dumb` +
  `!stderr.is_terminal()`). Progress/startup to stderr. Verify:
  `NO_COLOR=1 doctor | cat -v` clean; piped clean.
- P1.2 `--yes` + fail-closed prompts. Global `--yes`/`--non-interactive`.
  `ask_yes_no` (`init.rs:38-52`), `open_index_or_prompt` (`main.rs:699-730`),
  `clean` confirms (`main.rs:2302-2321`): non-TTY && !`--yes` → exit 2 +
  remediation. Negative control: piped stdin exits 2, mutates nothing.
- P1.3 `update` exit codes. `do_update` (`main.rs:1867-2110`): 0 up-to-date,
  1 error, 2 available (with `--check`). Map every early `return`.
- P1.4 `trust` vs `index` clarify. Fix hints to agree (`ux.rs:594`,
  `ux.rs:472`, `init.rs:399-405` → `trust .` first, `index` rebuild).
  `status` gains `--path` (currently hardcodes `./.reliary`, `ux.rs:518`).
  Quick start (`README:41-49`) adds the `init` step.

## Phase 2 — Output contract + errors

- P2.1 Honor `-q/-v/-f json`. `-q` silences `✓ files indexed` (`main.rs:510`),
  `vacuuming` (`545,560`), `building` (`581,615,622`). Progress
  (`run_index:496`, `run_build_all:621-623`) to stderr, suppressed under
  `-q`/`-f json`. Spinner (`ux.rs:10-19`) TTY-gated. `-f xml` rejects with
  valid-values error (`value_parser`), no silent fallback (`1110-1116`).
- P2.2 No-index (exit 2 + `run reliary trust {path}`) vs empty (exit 0 +
  `No results`) distinct in `search/pack/compress/verify`
  (`2159-2181,2233-2294,2208,2897`). Negative control per command.
- P2.3 Flag sanity. `logs --tail conflicts_with --level`; `config --root`
  requires `--local` documented; `sift --stdin` vs command precedence
  documented; `bench --out` per-subcommand help; remove `---stdin---`
  sentinel leak; `man` writes `reliary.1` (`main.rs:3021`); unify `-o` naming.
- P2.4 Help + error text. Per-command `long_about` + examples
  (search/index/risk/config/clean/logs/bench/fix/verify/pack); `logs` mentions
  `RELIARY_LOG_FILE`; `reliary-agent config` → `reliary config` (`2441-2443`);
  `cargo uninstall reliary-agent` (`873`); `doctor --fix` help truthful.

## Phase 3 — MCP tool honesty (agent-facing)

- P3.1 Scoping truth. `safe_module_filter` explicit scoped-empty (never silent
  None). Align `dead_only` narrow across both entry points (measure q10).
  Document `path` (project selector) vs `path_filter` (module scope);
  message the `index_root.rs:97-109` fallback.
- P3.2 Schema truth. `required` matches code; `describe`+`list_methods` gain
  `path`; `verify` `tol` units; hidden `anchor_file/threshold/window` exposed
  or removed from reads (`1639-1644`); spec `examples` per tool; unknown-tool
  error lists valid names (`1519,2933`).
- P3.3 Anchor + messages. One convention per fixture proof (sites above).
  `call_graph(inbound)` hint → `def_only` (not `goto_def`). Unify
  `No dead code` ×3, `No definition` + `Did you mean`, `NO CLAIMS` format
  (`984`). All no-index variants: `run reliary trust {path}`, path filled.
  Output paths corpus-relative everywhere (fix basename-only `1847-1851`).
- P3.4 Menu count truth. Reconcile `6 vs 7 vs 9` (`W7` comment, test `3385`,
  `README:96`, `AGENTS:35-37`) to one number; aliases documented once.

## Phase 4 — Onboarding + doctor truth

- P4.1 `init` idempotent + dry-run honest. `configured_agents` increments in
  dry path (`250,269,313,324,338,350,369`); dry shows would-skip-existing.
  Backup user configs before `atomic_write`; half-install atomic-or-rollback.
- P4.2 `doctor` no false-green. Stale `>7d` warns; `mode` validates typo;
  `binary` checks PATH resolvability; `ignore` checks content + deeper `.git`
  walk + `--fix` fixes it; `claude` hooks-required fixed; `pi` checks
  binary/registration; `rel` shim verified.
- P4.3 Uninstall symmetric. Plugin file removed even if config gone; `.bak`
  removed; `rel` failure loud; upgrade hint respects install method.
- P4.4 Docs + hook parity. Fix macOS OpenCode path; `AGENTS:75-78` auto-index
  = `serve_stdio` git-only, non-git warns; resolve config-share contradiction
  (local-only wins); unify hook rewrite sets; fix `gate.js` default
  contradiction; `config set` scope documented; `clean --global` confirms.

## Verification (pre-registered, non-vacuous)

- `NO_COLOR=1 … | cat -v` clean; piped clean.
- `</dev/null reliary clean/search/init` → exit 2, no mutation.
- `trust fresh → git status clean`; remove guard → leaks (exists).
- MCP: escaping `path_filter` errors with valid scope echoed; `dead_only`
  scope identical across entry points (q10 measured, no regression);
  unknown tool lists valid names.
- `doctor` on stale/missing-guard/typo-mode warns; `--fix` fixes ignore.
- Full gates per commit: workspace tests, `clippy -D warnings`, `cargo deny`,
  Docker 15/15. OpenCode agent 6/6 on the final commit only (budget).

## Sequencing

P1 → P2 → P3 → P4, commit per phase (4-6 commits), gates per commit.
No index-format change. No new tools. No prompt tuning. No benchmaxxing:
q10 measured, never fitted.
