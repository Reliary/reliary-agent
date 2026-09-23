# Benchmarks

All numbers below are from a deterministic claim-verification harness, not an
LLM judge. A model answer is scored by extracting every `symbol at file:line`
claim it makes and checking each one against the index. The result is
precision/recall/F1 over verifiable claims, plus billed token cost and wall
time. This rewards correct locations and penalizes invented ones; it does not
reward keyword overlap.

## Comprehension (10 questions, 4 seeds)

Corpus: a snapshot of this repository. Model: `deepseek-v4-flash`. Seeds:
42/17/123/456. Conditions: **A** = reliary MCP, **B** = `codebase-memory-mcp`, **C** = grep + read.

| Metric | A (reliary) | B (codebase-memory-mcp) | C (grep) |
|--------|-------------|----------------|----------|
| F1 (claim-weighted) | **0.947** | 0.461 | 0.615 |
| Precision | **0.960** | 0.893 | 0.881 |
| Recall | **0.935** | 0.315 | 0.473 |
| Coverage | **1.00** | 0.93 | 0.88 |
| Billed cost | **21,459** | 23,064 | 36,967 |
| Tool output bytes | **8.8K** | 13.8K | 33.3K |
| Dead-ends (unanswered queries) | **0.0** | 3.5 | 0.0 |
| Wall, median | **29s** | 32s | 39s |
| Keyword rubric /30 | 27.5 | 24.8 | **26.5** |

Read F1, not the keyword rubric. The `/30` row is a substring matcher kept only
as a smoke test: it is gameable by keyword-stuffing and roughly doubles the
apparent accuracy (verified against ground-truth F1 on the same answers).

Billed cost includes the provider's cache discount (~94% cache hit on all three
conditions). Wall time is provider-latency bound and the spread is within noise.

**Accuracy is comparable to grep, not established as superior.** The reliary-vs-grep
F1 gap and an independent LLM judge both fall within ~1σ on this single small
corpus. The reproducible, corpus-independent wins are cost (~42% lower billed
than grep), tool-output size (~4× smaller), and zero dead-ends.

**Prompt fairness.** Condition A receives the shipped routing prompt (~300
words); B and C receive ~100-word prompts. An ablation with a minimal ~120-word
prompt for A (`M`) scored F1 0.707 vs A's 0.816 — the tool contributes most of
the gap, the routing prompt the remainder.

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

The committed canonical tape is `bench/cassettes/canonical-v4/` (A/B/C × seeds
42/17/123/456). Replay it against a fresh corpus checkout:

```sh
bench/replay_canonical.sh     # $0, no key, no network
```

All three conditions replay byte-identically on a fresh corpus at a different
path — every score, answer, and cost figure matches; only wall-clock timers
differ. Condition B is best-effort: codebase-memory-mcp's own index build is not
reproducible, so its recorded hits are matched as a set. Set `REPLAY_CONDS=A,C`
to replay only the conditions that do not need codebase-memory-mcp installed.

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
