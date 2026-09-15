# V70 Plan — Telepathy (wall time) + Product Features

Status: approved 2026-09-10. One branch per item. Merge on success. ISTQB antagonism on every branch.

## Baseline (E5 canonical 4-way, 4 seeds, claim-weighted, corpus-matched)

| Cond | F1 | Precision | Recall | Billed | Turns | Calls | Dead-ends | Wall |
|------|----|-----------|--------|--------|-------|-------|-----------|------|
| A (reliary) | 0.765 | 0.762 | 0.767 | 24.3k | 27 | 17 | 0 | 33s |
| M (minimal prompt) | 0.609 | 0.651 | 0.576 | 21.3k | 24 | 14 | 0 | 31s |
| C (grep) | 0.428 | 0.460 | 0.401 | 34.8k | 26 | 16 | 0 | 43s |
| B (altbackend) | 0.299 | 0.373 | 0.250 | 25.1k | 30 | 20 | 4 | 34s |

Goals: wall <= 16s (telepathic), F1 >= 0.765 preserved, billed <= 24.3k, dead-ends 0.

## ISTQB antagonism protocol (mandatory per branch)

1. **Test plan** — scope, risks, equivalence partitions, boundaries, negatives.
2. **Equivalence partitioning** — valid/invalid input classes enumerated.
3. **Boundary value analysis** — 0/1/max/over-max; empty; unicode; huge inputs.
4. **Decision table** — flag/param combinations (at minimum: all-off, each-on, all-on).
5. **State transition** — cold/warm/stale cache; watcher on/off; index missing/present.
6. **Negative testing** — malformed JSON, missing args, nonexistent paths, permission errors, invalid revs.
7. **Regression** — full existing suite green (138 search + 60 agent + integration).
8. **Acceptance** — measurable criterion stated; merge only if green. No merge on "probably fine".

Branch policy:
- Branch from `feat/v59-accuracy`: `v70/<item>`.
- Implementation + tests + docs on the branch.
- Merge: `git checkout feat/v59-accuracy && git merge --no-ff v70/<item>` only when acceptance + regression pass.
- Failure: leave branch, write reason in commit/plan, do not merge.

## Wall-time levers (W-series)

### W1. Per-turn latency instrumentation — MERGED
Measured: api=32.1s (97%), tool=0.65s (2%), overhead=7ms. Wall is API round-trip bound.

### W2. HTTP keep-alive in bench client — MERGED
api/turn 1071ms -> 1060ms. Handshake was already amortized; kept for measurement hygiene.

### W3. Batched tool calls — FAILED (branch v70/w3-batch, unmerged)
Model rarely batches (1-2 turns/session); net turns/wall did not improve.

### W4. Wide responses — MERGED
Callee signatures in call_graph. turns 29->26, calls 19->16, wall 31.6s->27.7s,
F1 0.735->0.784, tool bytes -16%.

### W5. Sufficiency markers — FAILED (branch v70/w5-sufficiency, unmerged)
Model ignores [complete]/[+N more]; F1 0.784->0.746.

### W6. Output caps — FAILED (branch v70/w6-output-caps, unmerged)
Cap implemented but bench path already bounded; F1 0.784->0.780.

### W7. Menu trim — MERGED
8 -> 6 tools (goto_def, similar hidden; RELIARY_FULL_MENU=1 restores).
F1 0.784 -> 0.803 (best), score within variance, wall flat.

### W-series net (A condition, 2-seed runs)
- F1: 0.735 (pre-W4) -> 0.803 (post-W7)
- Wall: ~32s -> ~29s
- Menu: 8 -> 6 tools


## Feature phases (P-series)

### P1. `reliary verify` — MERGED
CLI + MCP `reliary_verify` (9th tool). Rust port of the verifier core:
6 claim forms, English-stop filtering, ±1 tolerance, phrase-stem lookup.
Acceptance: claim-set parity 10/10 (137 claims), verdict parity 10/10
vs the Python verifier on real bench answers. 5/5 unit tests.

### P2. `reliary impact <symbol|file>`
- CLI + appended to `describe` output.
- Data: direct callers, depth-2 callers, test files, churn (git log), risk verdict.
- ISTQB: 0/1/many callers; file input; nonexistent symbol; test-only symbol; unicode.
- Acceptance: impact of `classify_structural` matches `find_references` callers; < 200ms.
- Branch: `v70/p2-impact`.

### P3. `reliary test-plan [--diff R1..R2 | --files a,b]`
- Grammar-free test detection (path segments `test/tests/spec`, filename `test_*`, `*_test`, `*.test.*`, `*.spec.*`).
- Mapping: references to changed symbols + mirror paths + vocabulary overlap.
- Output: ordered test files + runnable commands.
- ISTQB: no tests; all tests; changed file with no tests; mixed; deleted file; unicode paths.
- Acceptance: on a known diff, includes tests that reference changed code; deterministic order.
- Branch: `v70/p3-test-plan`.

### P4. `reliary diff <rev1> <rev2>` — MERGED
Structural deltas from two indexed revisions. Fresh indexes now run
build_all_occurrence. HEAD~6..HEAD: 512 added / 406 removed; same-rev
empty; JSON byte-deterministic; invalid rev exit 2. 3/3 tests.

### P5. `reliary map --out map.html` — MERGED (SVG)
Pure-SVG grid: files ranked by total callers, symbols by call sites
(orange/bold hot, dimmed dead). 24 files/46KB, byte-identical across
runs, well-formed XML, all names escaped. 4/4 tests.

### P6. `reliary bench gen|verify` — MERGED
Rust port of auto_questions + deterministic scorer. No Python, no LLM.
gen: 4-6 questions from any index, byte-deterministic (seeded LCG).
verify: claim-weighted P/R/F1 per condition; matches Python (C exact,
A better via extractor fix). 12/12 tests.

### P7. Freshness — MERGED
watcher already default-ON (NO_RELIARY_WATCHER=1 disables); status now
reports index age + watcher state in human and JSON formats.

## Sequencing

1. W1 (instrument) -> W2 (keep-alive) -> re-measure baseline.
2. W3 (batching) -> re-measure.
3. P1 (verify) -> W5 -> W6 -> W4.
4. P2 -> P3 -> P4.
5. P5 -> P6 -> P7.
6. W7 last (client checks).
7. Final: canonical 4-way + deterministic verifier; update README/AGENTS with honest numbers.

## Non-negotiables

- Grammar-free: no tree-sitter, no per-language parsing.
- Deterministic outputs: same query + same index => byte-identical.
- Offline/private: no cloud dependency in the product.
- No cache-breaking changes to LLM conversations.
- Bench changes apply to all conditions (A/B/C/M).
- No F1 regression below 0.765 on the canonical 4-way when the change touches the A path.
- No savings/latency claims without the repeatable bench.
