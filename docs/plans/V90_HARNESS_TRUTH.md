# V90 — the harness was breaking B's tracer

## What was found

`bench/multi_turn_harness.py:375` sent `"name"` to `codebase-memory-mcp`'s
`trace_path`, which requires `function_name`. The CLI prints
`function_name is required` to **stderr** and nothing to stdout with exit 0, so
the wrapper's `json.loads(r.stdout)` raised and returned
`"(altbackend error: Expecting value: line 1 column 1)"`.

Present since `3b2f7b01` (2026-06-30). Every recorded tape is affected:

| tape | B answers blaming trace_path |
|---|---|
| canonical-v1 | 14/40 (35%) |
| canonical-v4 | 17/40 (42%) |
| canonical-v8 | 10/40 (25%) |

It lands on exactly the questions that need callers: q10 4/4, q5 3/4, q2 2/4,
q4 1/4 — while `search_graph` and `get_code_snippet`, whose argument names we
send correctly, succeed.

Handicap, same corpus, same query: `trace_path` returns 9,355 bytes for
`classify_structural` with the right key and a 62-byte error string with ours.

## Consequence

Every B figure published since June is void, including "F1 0.298" and the
README's "clearly ahead of codebase-memory-mcp".

## Plan

**P0 — harness correctness (blocking).**
1. `trace_path` sends `function_name`.
2. The B prompt advertises the real signature.
3. Audit every wrapper payload against the live server's `tools/list`
   `required` arrays.
4. Add a test that fails if any wrapper omits a required key (mutation-verified).
5. Retract the affected numbers in README / BENCHMARKS in the same change.

**P1 — metric correctness.**
6. Recall must not exceed 1.0: the caller-list exception matches on location
   alone, so one cited line satisfies several GT facts. Require a distinct
   claim per GT fact and assert `recall <= 1.0`.
7. Report `refusals`, `zero-tool answers`, `output share`, `prefix share` on
   every result.

**P2 — the two real product excesses (independent of the harness bug).**
8. `find_dead_code` / `describe dead_only`: present a capped list with
   `(+N more)` instead of dumping every item the caller's `limit` allows.
9. `describe` after `call_graph` re-states the definition; make it additive.

**P3 — re-record and report.** This repo, its obfuscated twin, and one foreign
repo. Show refusals. A-vs-B cost is unanswerable on a public copy of our own
source.

## Gates

- Wrapper/schema test fails when a required key is removed (negative control).
- `recall <= 1.0` asserted in the scorer.
- q10 and q4 billed each drop >= 30%, A F1 does not fall.
- A/B replay byte-identical at a different corpus path after re-recording.
- 664+ tests, clippy `-D warnings`, `cargo deny` clean.
