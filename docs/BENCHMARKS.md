# Benchmarks

All numbers below are from a deterministic claim-verification harness, not an
LLM judge. A model answer is scored by extracting every `symbol at file:line`
claim it makes and checking each one against the index. The result is
precision/recall/F1 over verifiable claims, plus billed token cost and wall
time. This rewards correct locations and penalizes invented ones; it does not
reward keyword overlap.

## Foreign corpus (tokio, auto-generated questions)

Corpus: tokio at a pinned commit, and the same tree with every identifier and
filename replaced by a deterministic pseudonym (`bench/obfuscate_corpus.py`).
Questions and ground truth are generated from the index by
`bench/auto_questions.py` and scored with the same deterministic verifier, so
no question was written to suit either condition. 4 questions, 2 seeds, 7-14
verified GT facts each.

| Corpus | Condition | F1 | Billed | Wall | Tool bytes |
|---|---|---|---|---|---|
| tokio | reliary | **0.783** | **4,262** | **7.9s** | **2,018** |
| tokio | grep | 0.642 | 10,666 | 12.6s | 11,154 |
| tokio (obfuscated) | reliary | **0.711** | **6,225** | **9.9s** | **2,105** |
| tokio (obfuscated) | grep | 0.526 | 18,228 | 18.5s | 10,206 |

Reliary leads on both twins, and the gap is slightly **larger** when the
identifiers carry no meaning (F1 +0.141 original, +0.185 obfuscated), so the
result is not explained by the model recognising tokio's names. Cost, output
size and wall time favour reliary in both cases by 2-3x.

This is a small sample: 4 auto-generated questions over 2 seeds. It is enough
to say the direction survives obfuscation, not enough to put a confidence
interval on the margin.

## Comprehension (10 questions, 4 seeds)

Corpus: a snapshot of this repository. Model: `deepseek-v4-flash`. Seeds:
42/17/123/456. Conditions: **A** = reliary MCP, **B** = `codebase-memory-mcp`, **C** = grep + read.

| Metric | A (reliary) | B (codebase-memory-mcp) | C (grep) |
|--------|-------------|----------------|----------|
| F1 (claim-weighted) | **0.948** | 0.495 | 0.596 |
| Precision | 0.985 | 0.876 | 0.830 |
| Recall | **0.913** | 0.348 | 0.467 |
| Coverage | **1.00** | 0.90 | 0.85 |
| Billed cost | **22,925** | 21,901 | 35,456 |
| Tool output bytes | 10.5K | 11.1K | 23.7K |
| Dead-ends (unanswered queries) | **0.0** | 2.0 | 0.0 |
| Refusals (explicit "cannot answer") | **0/40** | 4/40 | 1/40 |
| Wall, median | **33s** | 34s | 43s |
| Keyword rubric /30 | 28.0 | 27.5 | 27.3 |

Read F1, not the keyword rubric. The `/30` row is a substring matcher kept only
as a smoke test: it is gameable by keyword-stuffing and roughly doubles the
apparent accuracy (verified against ground-truth F1 on the same answers).

> **Retracted: every comparison against codebase-memory-mcp before 2026-10-23.**
> The harness called its `trace_path` tool with `name` where the server requires
> `function_name`. The CLI prints its complaint to stderr, exits 0, and writes
> nothing to stdout, so the wrapper raised and handed the model
> `(altbackend error: Expecting value: line 1 column 1)`. That silently disabled
> caller/callee tracing on the four questions that need it, and it affected
> every tape recorded since 2026-06-30 (35%, 42% and 25% of B answers in
> canonical-v1, v4 and v8 respectively). The previously published "F1 0.298"
> and "clearly ahead of codebase-memory-mcp" are void. This table is
> re-recorded with the fixed harness.

Billed cost includes the provider's cache discount (~92% cache hit on all three
conditions). Wall time is provider-latency bound and the spread is within noise;
reliary is inside the band, not the leader.

**On this corpus reliary answers more of the verifiable claims.** F1 0.948 vs
0.596 for grep and 0.495 for codebase-memory-mcp, with full coverage: every
query produced at least one checkable claim, and 94% of claims verified
correctly. Grep's precision is 0.83 — one claim in six was wrong.

It is also the only condition that never declined to answer: 0/40 refusals
against 4/40 and 1/40. A refusal is cheap, so a backend that declines can look
efficient while answering nothing; the refusal count is reported next to the
score for that reason.

That is a single corpus of this project's own source, so it is the *familiar*
case. On a foreign repository (tokio) grep led; the familiarity experiment
(`bench/FAMILIARITY_EXPERIMENT.md`) found no measurable familiarity effect, so
the honest reading is that accuracy on unfamiliar code is **not established**.
The corpus-independent wins remain cost (~42% lower billed than grep at these
numbers), tool-output size (~2× smaller), and zero dead-ends. Against
codebase-memory-mcp reliary is *more expensive* (21.3K vs 16.4K) and larger
(12.4K vs 10.4K bytes); its advantage there is accuracy and dead-ends, not cost.

**Prompt fairness.** Condition A receives the shipped routing prompt (~300
words); B and C receive ~100-word prompts. A same-tree ablation with a minimal
~120-word prompt for A (`M`, seeds 42/17/123/456, 2026-10-16) scored F1 0.885
vs A's 0.920 — the tool contributes most of the gap, the routing prompt the
remainder (~0.035).

Reproduce locally (~$0.05):

```sh
python3 bench/run_snapshot_bench.py --bin target/release/reliary \
  --corpus "<corpus-snapshot>" --conds A,B,C --seeds 42 17 123 456
python3 bench/deterministic_verify.py --input <out.jsonl> \
  --corpus "<corpus-snapshot>"
```

## Free reproduction (cassette)

A remote sampler cannot be forced deterministic from the client: the DeepSeek
API accepts `seed` but ignores it, and does not enforce `response_format`. What
can be fixed is the transcript. `bench/cassette.py` records every model response
keyed on the exact sampling-affecting request plus the corpus's
`meta.index_gen`, and replays it byte-for-byte with zero API calls.

The committed canonical tape is `bench/cassettes/canonical-v10/` (A/B/C × seeds
42/17/123/456). Replay it against a fresh corpus checkout:

```sh
bench/replay_canonical.sh     # $0, no key, no network
```

All three conditions replay byte-identically at the recorded corpus path —
every score, answer, billed cost and tool-byte figure matches; only wall-clock
timers differ (verified on the committed tape: A/B/C × seed 42 reproduced
exactly).

A and B also replay at a *different* corpus path. C does not: it runs
arbitrary shell commands, so its output embeds the checkout path.
`REPLAY_CONDS=A,B` is the byte-identical check anywhere else.

Condition B is best-effort at a different path: codebase-memory-mcp's own index
build is not reproducible, so its recorded hits are matched as a set.

Recording a new tape (live API):

```sh
RELIARY_CASSETTE=bench/cassettes/rel8 RELIARY_CASSETTE_MODE=record \
  python3 bench/long_session_bench.py --conditions A --seeds 42
RELIARY_CASSETTE=bench/cassettes/rel8 RELIARY_CASSETTE_MODE=replay-strict \
  python3 bench/long_session_bench.py --conditions A --seeds 42
```

Reindexing the corpus invalidates entries automatically (`index_gen` is part of
the key). `python3 bench/cassette.py summarize <dir>` lists near-tie decisions.

## Edit outcome (mutation bench)

Two pre-registered mutation benches asked whether the comprehension advantage
converts to *fixing bugs* (`bench/MUTATION_BENCH_V2_PREREG.md`). Both returned a
tie.

- **Run 1** (tests visible to the agent): all three conditions scored f2p 100%
  on 6 tasks × 2 seeds. The design was invalid — a runnable failing test is the
  answer key.
- **Run 2** (discriminating design: every test stripped from the agent's
  workspace, bare prose symptoms, hidden tests injected at scoring time):
  again all three conditions scored 100%, zero wrong-file edits.

The pre-registered kill criterion (reliary ≥ both by +15pp) failed both times.
Single-line defects in a ~500-file repo are greppable from symptom prose, so
there is no index advantage to demonstrate at this corpus size. **Reliary's
edit-outcome advantage is unproven.** Details:
`bench/MUTATION_BENCH_V2_RESULTS.md`.

## Familiarity experiment

To test whether the accuracy edge was an artifact of the model having memorized
a public corpus, every identifier in a snapshot of tokio was replaced with a
deterministic pseudonym (grammar-free token substitution) and the same questions
were translated onto both corpora (`bench/FAMILIARITY_EXPERIMENT.md`).

Result: **no flip.** The reliary-vs-grep delta was positive and similar on both
the original and the obfuscated corpus. The accuracy edge is not explained by
training priors — but it is also not established as superior to grep. Cost and
output-size advantages reproduced on both arms.

## Bash compression (`reliary wrap`)

Measured on the 20 non-trivial fixtures of the RTK comparison bench:
**mean 31.6%, median 3.9%** byte reduction. Compression is concentrated where it
matters — repeated lines (100%), ANSI noise (97%), `ss -tuln` (89%), chaos
output (83%), `git status` (72%), `ps aux` (46%) — and near-zero on short dense
output (compiler errors, `docker ps`, `ip a`).

Hard no-inflation guarantee: `wrap` emits the raw bytes whenever compression
would produce a longer result. Source-file readers (`cat`/`head`/`tail`/`less`/
`bat <source>`) pass through byte-identical so model-built edits never see
mangled code.
