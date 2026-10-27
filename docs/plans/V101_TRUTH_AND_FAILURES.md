# V101 — trust the instrument, then work the measured failures

Status: executing.
Pre-registered before any run. Numbers below are filled in as they are measured;
the gates were written first.

## Premise correction

We already win on cost, output size and dead-ends, and we do not need to
re-prove it. `grep` is a **quarterly floor**, never a headline. The question is
now "what is our own ceiling and which lever moves it", not "are we
better than X".

## What the antagonism established (and this plan accepts)

1. `bench/search_bench.py` measured known-item lookup of uniquely-named
   definitions — the easy case, where grep also wins. It cannot show a
   difference that matters.
2. Its answer key was broken: `zod.tags` and `requests.tags` were generated
   with cwd inside a subdirectory, so 85% (zod) and 54% (requests) of gold
   paths do not exist under the corpus root. The scorer's `endswith` fallback
   made those metrics pass by accident. **requests was the least trustworthy
   number and I listed it as one of two I trusted.**
3. "Run twice, require identical scores" proves determinism, not validity.
4. Resolving gold against the index would be circular.
5. The indent ledger has no cheap gate: a rank-diff requires the mechanism it
   is meant to gate. **Dropped permanently** below.
6. The plan had no value hypothesis: every item was measurement or coverage.
7. The three failures our own forensics measured were **tool routing (q9),
   callgraph extraction precision (q4) and dead-code ordering (q10)**. None of
   them was "search could not find the file". Retrieval recall is not the
   demonstrated bottleneck, so it is not this round's work.
8. Any classifier change moves `index_gen` and invalidates the committed
   `canonical-v4` tape. If a classifier change ships, the tape must be
   re-recorded or the README's "$0 reproduction" claim is false.

## Results

### P0 — instrument (done)

Two tagfiles were regenerated from the corpus root; the key now resolves
against the filesystem and requires a unique basename.

| corpus | valid golds | dropped | recall@8 | recall@1 | mrr |
|---|---:|---:|---:|---:|---:|
| tokio (Rust) | 2,137 | 2,363 | 1.000 | 0.983 | 0.989 |
| requests (Python) | 91 | 46 | 0.989 | 0.967 | 0.970 |
| zod (TypeScript) | 570 | 962 | 0.983 | 0.925 | 0.954 |
| sinatra (Ruby) | 331 | 172 | 0.992 | 0.842 | 0.904 |

Pre-registered gate: no corpus below 0.60 recall@1. **Passed.** No retrieval
work is scheduled.

The previous published figures (zod 0.792, sinatra 0.758, requests 0.868) were
measured against a partly-broken key; they are superseded.

### P1 — the three measured failures

| failure | verdict |
|---|---|
| q9 trait impls | **already fixed** — scoped `find_references(Default)` returns the impl types |
| q4 callgraph precision | **already fixed** — callees carry def sites and no prose |
| q10 dead code | **already fixed** — returns real functions, ordered sensibly |
| (new) search on a symbol | **was broken** — fixed below |

### P2 — `search` answers exact identifier queries (done)

Three defects surfaced while implementing it, each with a regression test:

1. **`exact_definition` was blind after a bare `trust`** — `phrases` is
   populated at trust time but `occurrence` is built lazily, so the hint was
   absent until someone ran `build-all`. Same class as the V77 default-path
   bug. Now the rows for that one phrase are built on demand.
2. **Definitions were ordered by line number across files** — `classify_structural`
   resolved to `test_plan.rs:80` because 80 < 328. Rank by the name's use count
   in the defining file instead.
3. **The hint used a bare basename** because `short_path` shortens within an
   answer set of one. Added `project_relative`.

Gate G2.2 (≤20% more bytes) initially **failed** at +25% on tokio: the hint
repeated the top-ranked path. The hint now *replaces* the duplicate entry,
which brings tokio to 223 B against a 212 B baseline (+5%).

## P0 — make the instrument honest (free, no LLM)

Rewrite the gold-path resolution in `bench/search_bench.py`:

- resolve every gold path **against the filesystem**, never against the index;
- require the **basename to be unique** within the corpus (a bare
  `__init__.py` gold is ambiguous and is dropped);
- **assert 100%** of the surviving golds resolve on disk before scoring;
- no `endswith` fallback in scoring — exact normalized path only;
- report how many entries were dropped and why.

Gate:
- G0.1 every retained gold resolves (assertion, not a percentage);
- G0.2 the drop count is reported per corpus;
- G0.3 the corrected numbers are published even if worse.

Pre-registration: if corrected `recall@1` on any corpus is **below 0.60**, the
conclusion is "our retrieval is weaker than believed" and no retrieval work is
scheduled until that is explained. It is not a reason to change the bench.

## P1 — verify the three measured failures mechanically (free, no LLM)

Against the current binary and a real corpus, check each claimed failure class
directly. No model in the loop.

| failure | mechanical check | pass condition |
|---|---|---|
| q9 trait impls | `find_references(Default, path_filter=crates/x)` | returns the in-scope impl types |
| q4 callgraph precision | `call_graph(<known symbol>, direction=outbound)` | returns only callees that have definitions |
| q10 dead code | `find_dead_code(path=...)` | the known-dead function appears |

Gate G1: each class is either **still broken** (fix it this round) or
**already fixed** (recorded, and the residual gap is model tool choice, which
is not a tool defect).

## P2 — make `search` answer exact identifier queries (only if P1 shows routing)

`search("Default")` returns files. A model that picks `search` for a symbol
question therefore sees no definition, which is the q9 failure. Additive fix:
when the query is a single token that exactly matches a defined name, prepend
one line naming the definition, then the normal file list.

Gate:
- G2.1 `find_references` output byte-identical (this must not touch it);
- G2.2 mean answer bytes rise by no more than 20%;
- G2.3 a test asserts the exact-match line appears and that a non-symbol query
  is unchanged;
- G2.4 the test is proven non-vacuous by reverting the change.

## P3 — documentation of what we are not doing

- **Indent ledger: dropped permanently.** Recorded in `AGENTS.md` with the
  reason (three proposals, no cheap gate, blank-line blocks are only used for
  ranking). Not to be raised again without new measurement.
- **OCaml 0.352 / Haskell 0.647: documented, not chased.** The ctags oracle
  labels `open`/`include` as modules and expression-lets as functions, so part
  of that gap is us being right. No user has asked an OCaml question.
- `docs/research/` notes keep the two "predicts this is worthless" findings so
  they are not rediscovered.

## Non-goals

- q-log IDF (its own ablation says identifier-aware tokenization removes it).
- Embeddings / semantic retrieval (non-deterministic, cache-hostile).
- Cross-session memory.
- Re-benchmarking against grep.
- Chasing ctags recall on layout languages this round.

## Gates for the round

- 706+ tests, `clippy --all-targets -D warnings`, `cargo deny`.
- No classifier change, so the committed tape keeps replaying. If any change
  does move `index_gen`, re-record before the round closes.
- Every new test proven non-vacuous by reverting the fix.
