# Mutation edit bench — results (v1 test-visible, v2 test-withheld)

Two pre-registered runs. Model: `deepseek/deepseek-v4-flash`, Pi agent.

## v1 — tests visible (design flaw: test is the oracle)

6 tasks × 3 conditions × 2 seeds = 36 runs. **All conditions f2p 100% (3.00/3)**
on every task and seed. Pre-registered kill criterion failed. Details below in
"v1 detail".

## v2 — test oracle withheld (the discriminating design)

The agent's workspace has **every test removed uniformly** (`#[cfg(test)]` →
`#[cfg(any())]`, `tests/` dirs deleted); the prompt is a bare prose symptom with
no test command; the agent is told to verify with `cargo check` only. Scoring
runs on a separate pristine tree with the agent's source patch re-applied and the
real tests restored. This measures localization, not "run the failing test".

| Condition | n | f2p | mean score | timeouts | wrong-file | median billed | median calls | median wall |
|-----------|---|-----|-----------|----------|-----------|---------------|--------------|-------------|
| A (reliary)    | 12 | **100%** | 3.00 | 1 | 0 | 25,886 | 11.5 | 24s |
| B (altbackend) | 12 | **100%** | 3.00 | 0 | 0 | 28,102 | 11.5 | 18s |
| C (grep)       | 12 | **100%** | 3.00 | 0 | 0 | 29,232 | 11.0 | 18s |

**Zero failures. Zero wrong-file edits. Patch apply succeeded for every run.**

## Verdict: TIE — the edit-outcome advantage is unproven. Scrap this line.

Removing the test oracle changed nothing: all three conditions still fix all six
bugs on both seeds. The mutation tasks are single-line defects in a ~500-file
repo, and the symptom prose still lexically overlaps the fix site (e.g. "member
locations land on the line below" → the buggy expression is a line-number
arithmetic). `grep` on the symptom's nouns reaches the primitive in a few calls,
so no index advantage exists to demonstrate. The corpus is simply too small and
the defects too local for retrieval quality to matter.

This is a **fact about the benchmark**, not proof that reliary adds nothing.
The conditions under which an index should win — a corpus large enough that
grep's candidate set explodes, or a symptom whose vocabulary diverges from the
code's — were never created. But we have now run the discriminating experiment
twice and it ties both times, so the honest position is:

- **Demonstrated:** reliary's comprehension advantage (F1 0.949 vs 0.622 grep,
  ~2× lower billed, 0 dead-ends) on the canonical benchmark.
- **Not demonstrated:** any edit-outcome or hard-task advantage. Two
  pre-registered attempts failed to show one.
- **Conclusion:** stop investing in an edit-outcome bench unless a genuinely
  large-corpus setup (kernel-scale, or a corpus where grep is overwhelmed) is
  built. Until then, claim only the retrieval advantage.

## Honest secondary observations (not a win claim)

- A was again **the slowest** condition in v1 and **not** cheaper in v2
  (median billed 25.9k vs 28.1k/29.2k — a wash within variance).
- One A timeout in v2 (m6/seed42) still produced a passing fix; it kept
  exploring past completion.
- No condition ever edited the wrong file: localization was never the
  bottleneck on any task.

## v1 detail

| Condition | n | f2p | mean score | timeouts | wrong-file | median billed | median calls | median wall |
|-----------|---|-----|-----------|----------|-----------|---------------|--------------|-------------|
| A (reliary)    | 12 | 100% | 3.00 | 2 | 0 | 41,848 | 13.5 | 295s |
| B (altbackend) | 12 | 100% | 3.00 | 1 | 0 | 33,780 | 12.5 | 240s |
| C (grep)       | 12 | 100% | 3.00 | 0 | 0 | 32,266 | 13.0 | 230s |

## Reproduction

```sh
# v2 (test-withheld): the harness strips tests from the agent workdir and
# scores on a pristine tree with the agent patch re-applied.
python3 bench/mutation_edit_bench.py --conds A,B,C --seed 42 --out bench/results/mut_v3_s42.json
python3 bench/mutation_edit_bench.py --conds A,B,C --seed 17 --out bench/results/mut_v3_s17.json
```
