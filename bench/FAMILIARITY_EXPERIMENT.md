# Familiarity experiment — does reliary's advantage survive identifier obfuscation?

**Question.** Is reliary's comprehension advantage a *familiarity* effect — the
model has memorized public repos like tokio, so on those it overrides tool
output with training priors, while on unseen/private code the tool's precise
answers are the only signal? If so, obfuscating a public corpus should *flip*
the comparison in reliary's favour.

**Method.** Take tokio at a pinned commit, rename every user identifier to a
deterministic pseudonym (`bench/obfuscate_corpus.py`), translate the exact same
questions+GT onto the obfuscated twin (`bench/translate_questions.py`), and run
the identical A (reliary) vs C (grep) bench on both. Grammar-free: the
obfuscator preserves case class, underscore segmentation, language keywords and
directory names, so the index and generator see the same structure. File stems
are seeded by full relative path (fixing a 25-way `mod.rs` collision).

## Result: **no flip**

| Arm | Condition | claim-F1 | LLM judge (mean /3) | billed (median) | wall | tool calls | tool bytes |
|-----|-----------|----------|---------------------|-----------------|------|-----------|------------|
| Original tokio | A reliary | **0.727** | **1.78** | **6,478** | **9s** | **4** | **3,866** |
| Original tokio | C grep | 0.668 | 1.53 | 11,632 | 17s | 6 | 12,544 |
| Obfuscated tokio | A reliary | **0.693** | **1.88** | **11,798** | **12s** | **4** | **4,292** |
| Obfuscated tokio | C grep | 0.647 | 1.69 | 16,026 | 17s | 6 | 10,078 |

(A−C delta: claim-F1 +0.059 original / +0.046 obfuscated; judge +0.25 / +0.19.)

The hypothesis predicted a *negative* original delta and a positive obfuscated
one. Observed: both positive and of similar size. **Obfuscation did not flip the
result, so this experiment finds no support for the familiarity hypothesis.**

## Honest limitations

- **Small and not significant.** 4 questions × 8 seeds = 32 samples. The judge
  delta is 1.1–1.4σ — inside noise. This cannot *confirm* a general advantage
  either; it only fails to confirm the familiarity mechanism.
- The obfuscated arm's judge was slightly *higher* for A than the original
  (1.88 vs 1.78), which is not a directional prediction and is within noise.
- Only two backends, one foreign corpus, one question set.
- Absolute numbers here are **not** comparable to the README's canonical bench
  (different corpus and questions).

## Real bugs found and fixed by building this

1. **Deep-nested definitions were silently dropped** (`structural.rs`,
   commit `58223512`). The blanket `block_depth > 2` filter discarded any real
   definition nested 3+ levels deep — including every function generated inside
   a `macro_rules!` body and every method inside `impl` inside 2+ `mod` blocks.
   tokio had 265 `fn <name>` declaration lines indexed `is_def=0`, so
   `find_references`/`def_only`, `list_methods`, `call_graph` and qualified names
   all missed them. A keyword-confirmed declaration is now a definition at any
   depth, while nested control flow is still rejected. 265 → 191 (the remainder
   are multi-line signatures, tracked separately).
2. **Caller-list GT matching** (`deterministic_verify.py`). A "who calls X"
   ground truth puts the *queried* symbol on every fact, but a correct answer
   cites the *enclosing caller* at that site. The verifier scored correct caller
   lists as 0 recall on both arms until it matched on location when the GT
   symbol is the question's subject.
3. **File-name collisions** (`obfuscate_corpus.py`). Stem-only pseudonym seeding
   collapsed every `mod.rs`/`lib.rs` onto one base name (25-way collision in
   tokio); now seeded by full relative path.

## What this means for the private-repo claim

The earlier reasoning was: reliary wins on our corpus, loses on tokio (memorized),
therefore the win is familiarity. This experiment tested that mechanism directly
and **it did not hold** — obfuscation left the advantage roughly unchanged.

So the honest current position is narrower than "we win on private repos":

- **Demonstrated, corpus-independent:** lower cost (2–4× lower billed), smaller
  tool output, fewer dead-ends. These reproduce on every arm.
- **Observed but small and not significant:** reliary's accuracy edge survives
  obfuscation, so it is *not explained by familiarity* — but at 1.1–1.4σ it is
  not established as a general accuracy advantage either.
- The familiarity mechanism is **not supported** by this test.

Reproduce: index each corpus, then
`python3 bench/public_bench.py --index <idx> --corpus <root> --bin target/release/reliary \
  --conds A,C --seeds 42 17 123 456 7 99 2024 555 --questions <q.json> --gt <q.json> --out <out.jsonl>`
followed by `python3 bench/deterministic_verify.py --input <out.jsonl> --corpus <root> --gt <q.json>`.
