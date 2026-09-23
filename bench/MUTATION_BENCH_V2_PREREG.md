# Mutation edit bench v2 — pre-registration

Committed **before** any API call for this run. Purpose: measure whether reliary's
code-intelligence accuracy converts into better *edits on tasks that require
localization*, not keyword lookups.

## Why a v2

The V71 pilot was invalid: 4 single-line boolean mutations whose symptom text
named the mechanism (`"files following the foo.test.js convention"`,
`"names that start with an uppercase letter"`) and whose target test lived in the
file to edit. All three conditions scored 3/3 — it could not discriminate.

## Results

v1 (test-visible design) **tied** — all conditions 100% f2p. See
`bench/MUTATION_BENCH_V2_RESULTS.md`.

v2 (this document, amended) **withholds the test oracle**: the agent's workspace
has every test stripped uniformly (`#[cfg(test)]` → `#[cfg(any())]`, `tests/`
dirs deleted), the prompt contains a bare prose symptom with no test command, and
scoring runs on a separate pristine tree with the agent's source patch re-applied
and the real tests restored. This is the design that actually measures
*localization* rather than *run-the-failing-test*. Kill criterion unchanged.

## Hard design rules (all six tasks)

1. **Symptom-only prompts.** The prompt describes a *user-visible wrong output*.
   It must not name the file, the symbol, the function, the convention, or the
   mechanism.
2. **No answer in the test name.** Target tests have neutral names; the fix
   location is not derivable from the prompt.
3. **Cross-cutting effect.** Each mutation breaks a shared primitive that many
   call paths depend on, so localization means finding the primitive, not the
   call site.
4. **Mechanical validation, pre-registered.** Every task must satisfy:
   - (a) target test fails on the mutated tree;
   - (b) target test passes after the reference fix (`git checkout` of the
     mutated file);
   - (c) the full crate test suite passes pre-mutation (no flaky baseline).
5. **Score = FAIL_TO_PASS outcome**, not claim extraction. `3` = target test
   passes AND crate tests pass AND compiles; `2` = target passes, crate regresses;
   `1` = compiles, target failing; `0` = no fix / broken build. Immune to format
   bias by construction.
6. **Cap prompt and per-task timeout** so a single timeout cannot consume the
   whole budget. Timeout is reported as a failure, not retried.

## Conditions

Same agent (Pi, `deepseek/deepseek-v4-flash`), same prompt shape; only the
code-intelligence layer differs:
- **A** — reliary MCP extension
- **B** — altbackend (codebase-memory-mcp) extension
- **C** — no extension (bash + grep + read + edit)

## Tasks

| id | primitive broken | location class | target test file |
|----|------------------|----------------|------------------|
| m1_test_convention | test-path classifier | call-graph/impact module | `crates/reliary-search/src/impact.rs` |
| m2_method_line | method line indexing | call-graph module | `crates/reliary-search/src/callgraph_v2.rs` |
| m3_field_dedup | struct-field extraction | call-graph module | `crates/reliary-search/src/callgraph_v2.rs` |
| m4_snake_stem | identifier normalization | core token module | `crates/reliary-search/src/lib.rs` |
| m5_graph_tail | brace-graph state machine (depth) | structural module | `crates/reliary-search/src/brace_graph.rs` |
| m6_graph_comment | brace-graph state machine (comment/string skip) | structural module | `crates/reliary-search/src/brace_graph.rs` |

Symptoms (verbatim, mechanism-blind) are defined in `mutation_edit_bench.py`.

## Run plan

- Seeds: **42, 17** (2 seeds × 6 tasks × 3 conditions = 36 runs).
- Per-task timeout: **600 s** (not 900 s).

> **Amendment (before the scored run, after 1 discarded smoke run).** The
> original cap was 300 s. A smoke run of `m3` showed the agent legitimately
> still working at 300 s — each `cargo test --release` on the 12-crate
> workspace rebuilds the mutated crate and its dependents. A cap that truncates
> every condition equally makes the bench uninformative rather than
> discriminating, so the cap was raised to 600 s. No task, prompt, test, or
> threshold was changed. The single smoke run's numbers are discarded.
- Score matrix reported per condition: mean f2p (0–3), wrong-file rate, median
  billed cost, dead-ends.

## Kill criterion (pre-registered)

**A must beat BOTH B and C by ≥ 15 percentage points** on mean f2p across the 12
seeded task instances. If it does not, this is reported as a **tie or loss** and
stated as such in the README — no re-cut, no question rewrite, no re-scoring with
a different rubric.

Secondary (report only, not a pass/fail gate): cost and wrong-file rate.

## Explicit non-goals

- No rewriting task prompts or tests after seeing model output.
- No changing the scoring rubric mid-run.
- No model-ceiling claim as an excuse, and no claim of a win the numbers do not show.
- Budget guard: abort the run if balance drops below $1.50.
