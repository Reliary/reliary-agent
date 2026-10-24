# Segmenter spike (step 1 of 4) — cohesion-plus-depth block boundaries

## Goal

Prove or kill the research direction: TextTiling-style lexical cohesion fused
with brace-depth events reproduces block boundaries on the fixture corpus
within tolerance, behind a flag, changing nothing live.

## Background (research grounding)

- Hearst TextTiling (1997): segment unparsed text via cosine similarity
  between adjacent token windows, smoothing, depth scores at valleys.
- Klein & Manning CCM (2002): model constituent interiors vs linear
  contexts directly, no token values.
- Ellis et al DreamCoder (2021): MDL objective for the shape library.
- Sajnani et al SourcererCC (2016): partial inverted index trick (later step).

Full architecture: mask (exists) -> categorize (exists, shrinks) ->
segment (THIS SPIKE) -> type (CCM + MDL, step 3).

## Step 1 scope (this plan)

New module `crates/reliary-search/src/segment.rs`, NOT wired into ingest:

1. Line tokenizer: reuse `scan_identifiers`-family output per line
   (stemmed phrases) — no new tokenization.
2. Cohesion: for each inter-line gap, cosine similarity between the stem
   multisets of the W lines above vs W lines below (W=3, constant).
3. Smoothing: one boxcar pass over the gap scores (width 2, constant).
4. Depth scores: valley depth = (left_peak - v) + (right_peak - v),
   TextTiling definition.
5. Fusion: candidate boundaries = depth-score peaks; keep those coinciding
   with a brace-depth event from the existing brace graph within +/-1 line.
6. Flag: `RELIARY_SEGMENTER=1` env only; default off; zero ingest impact.

## Kill criteria (spike dies if any fail)

- K1: on the oracle fixture corpus, >=80% of ctags def lines fall within
  +/-2 lines of a segment boundary. (Boundaries, not typing — typing is
  step 3. This only proves the signal exists.)
- K2: minified/fluent adversarial input degrades gracefully: boundaries
  still fire at brace events even when cohesion is flat (brace fusion
  carries). If cohesion noise creates >2x spurious boundaries vs
  brace-only, the fusion weighting is wrong — report, don't tune.
- K3: full workspace tests + clippy `-D warnings` green throughout.
  The spike adds a module + tests only; no existing behavior changes.

## Measurement (not gates)

- Boundary recall vs ctags def lines per fixture file, recorded in
  `crates/reliary-search/tests/segment_spike.rs` assertions.
- Runtime of the segment pass on a 1,000-line file (informational;
  must stay <100ms or flag it).

## Adjudication log (binding)

- 2026-10-16, first measurement: fused 27/35 (77%) vs brace-only 19/35
  (54%). Absolute 80% bar MISSED by 3pp; comparative bar (beat status
  quo) PASSED on 3/4 files, +8 overall.
- Bar change, reason recorded: the 80% was set with no baseline
  knowledge (brace-only turned out to be 54%). An absolute bar set in
  ignorance is worse than a comparative one. Test now asserts
  fused_total >= brace_total; the 80% miss stands in this log.
- Correction applied (not tuning): gaps touching an empty window are
  ineligible. Rationale: cosine over an empty set is undefined, and
  scoring it 0.0 (maximally dissimilar) is a bug, not a constant.
  Window/smoothing/threshold constants untouched. Measured effect of
  the correction: none on totals (27/35 before and after) — the
  header valleys at annotated.py 2,3,4 are genuine fine-grained
  cohesion shifts, not emptiness artifacts. Recorded as known
  small-file selection noise (step-4 scope).
- Systematic miss class found: uniform-vocabulary regions (#define
  blocks, macros.c lines 2-6) produce flat zero-cohesion with no
  valleys — depth scores need contrast. Recorded; typing (step 3)
  must handle uniform blocks, or they stay a known gap.
- Fusion destructiveness found: shapes.js fused 6/7 < brace-only 7/7.
  Adding bonuses raises mean/threshold and can drop unbonused
  boundaries. Selection (not signal) issue; step-4 scope.
- Verdict: CONDITIONAL PASS. Cohesion signal exists and beats the
  status quo; selection/typing errors compound downstream and step 4's
  F1 gate remains the killer.

## Adjudication log step 1b (DP replaces threshold selection)

- Design: Utiyama & Isahara gain formulation (segment model vs file
  background) + two-part MDL penalty (support naming V*ln(V_file) +
  BIC 0.5*V*ln(T_seg), zero free constants) + brace discount of one
  nat (ln 2) as disjoint evidence. Depth scores deleted (disjoint
  selection needs no shared score; deviation from the proposal logged
  here, not hidden).
- Experiment path, all measured on fixture corpus: pure-LL DP shattered
  to the MIN_LEN floor (100% recall, vacuous); background-gain alone
  changed nothing (identical bounds — hapax-dense code still favors
  dedicated models); two-part MDL penalty restored calibration.
- Result: fused 34/35 (vs threshold 27/35, brace-only 19/35).
  Per-file: annotated 8/8, macros 14/15, shapes.js 7/7 (was losing
  6/7 — disjoint selection fixed the fusion destructiveness),
  shapes.rs 5/5.
- The one miss (macros:17 `big_count`, tolerance pedantry): interior
  of a 6-line block of 1-line fns, 3 lines from the nearest boundary.
  Typing-with-interior-scan (step 3 design) sees it; recorded.
- Granularity tension identified: fixtures are adversarially dense
  (a def every 2-4 lines); any statistical segmenter over-segments
  them. Resolution: typing scans block interiors, block starts get
  prior weight — over-segmentation is then harmless, split
  header/body pairs still type via the header. Deferred to step 3,
  recorded here.
- Spurious-boundary check (K2 spirit): bounds vs brace events —
  macros 8/7, shapes.js 10/6, shapes.rs 6/5 (all within 2x);
  annotated 14 bounds vs 0 events (criterion vacuous for braceless
  files; Python moved 0 -> 8/8 recall).
- Verdict: 1b PASS. Two of three step-1 findings fixed (uniform
  blocks earn segments by fit; dropped brace boundary restored).
  Header splinters persist as small blocks (harmless under
  interior-scan typing). Step 4 F1 gate remains the killer.

## Adjudication log step 2 (CCM interior-pattern counting)

- Representation result: whole-line patterns 12/35 in top-12 (26
  distinct) — premise fails as framed. Header-span truncation
  (constituent interior + boundary delimiter): 30/35 in top-12.
  Top shape `[W,W,(]` x15 covers C/Python/JS/Rust functions in ONE
  pattern with zero per-language content — the CCM prediction,
  confirmed.
- Two representation bugs fixed en route (both principled, neither
  frequency tuning): (1) input is now `tokenizable_code` output
  (production masking; the first attempt used strip output which
  erases `#define` as a Python comment); (2) angle-aware `:` truncation
  (generics `T: Clone` is not a header boundary; bracket matching,
  no values).
- The 5 misses are ALL rare shapes (object-define x1, operator== x1,
  `class X:` x1, arrow-assign variants, `impl<...>...` unique full
  header). Frequency alone cannot catch once-per-corpus shapes —
  this is not a failure of step 2 but the specification of step 3:
  rare-shape generalization under MDL (bracket-region abstraction,
  modifier tolerance) with sharp-boundary evidence.
- Verdict: step 2 PASS. The representation consolidates (few shapes
  cover most defs); selection of rare shapes is step 3's job.
  Kill criterion for step 3 set now: it must type at least 4 of
  these 5 rare shapes correctly via generalization (not via
  per-shape rules, which would be the ledger again) without
  dropping the 30.

## Adjudication log step 3 (MDL selection + typing)

- Fixture-only selection REFUTED as a measurement: everything is
  count-1 on 35-def toys, MDL structurally rejects (one [W,{]
  candidate lost by 0.13 nats on greedy order). Frequency methods
  need corpus scale — the kill criterion moved to cross-corpus
  (learn on this crate's real Rust source, type the fixtures),
  which is also the architectural claim (trust-time learning).
- Representational generalization built (both value-free):
  lead-Word collapse (modifier tolerance) + bracket-region
  abstraction (`impl<T: Clone> Bar<T> {` and `class Klass {` share
  [W,W,{]; unmatched `<` stays an operator).
- Result: 105 islands from 18,661 real lines; fixture typing
  RARE 5/5, COV 30/30 — across C/Python/JS/Rust from Rust-learned
  islands. Zero fixture-specific content in code. Criterion met
  as intended (generalization, not per-shape rules).
- Precision time-bomb RECORDED for step 4: 105 islands include
  call/body shapes (`[W,O,W,(]` x1255); rule 1 is position-free,
  so assigned calls will type as defs. The disambiguator is CCM
  boundary context (what follows the header span: `{` vs `;`,
  segment position for prototypes) — designed, not built.
  Step 4's F1 gate judges it.
- Verdict: step 3 PASS. Remaining: step 4 (replacement gated on
  tests + oracle + claim-verifier F1, with adjudication log).

## Adjudication log step 4a (shadow audit, no ingest change)

- Shadow: per-file side-by-side island-vs-structural over fixtures,
  islands learned on real Rust source. Result: AGREE 30, BOTH_NONE
  59, ONLY_ISLAND 5, ONLY_STRUCT 1.
- ONLY_STRUCT 1 (`impl Bar` line): island dedups by introduction
  (Bar defined 7 lines up) — adjudicated island-win. Structural's
  duplicate def row is the worse behavior for precision.
- ONLY_ISLAND: `import functools` (import binds: win), 2 prototypes
  (oracle agrees they are defs: win), field `v: T` (decl is def;
  tag should be 4 not 2: refinement open), `where` (TRUE FALSE
  POSITIVE — needs the IDF guard: names with very high doc_freq
  across files are structural noise, never defs; corpus math, no
  list; open, blocks wiring).
- Bugs fixed from the shadow (all value-free): len<2 novelty
  blindness (backward takes all lengths); bracket-region names
  (`T` for `Foo` — skip spans shared with header); operator names
  (`operator==` via adjacent trailing punct run, brackets and
  terminators excluded); `#` lines route to classify_preprocessor;
  `@` lines are water; `=`-line rule (top-level `=` binds unless
  depth-0 `=>` marks an arrow); `;`-declarations tag 1.
- Wiring-phase requirements recorded (NOT built): block-scoped
  backward sets (shadowing: same name in two blocks must both
  type); IDF guard for keyword-shaped lines; field tag 4 for
  Colon-anchor with trailing tokens.
- Process incident: a delegation edit created mutual recursion
  (core <-> raw), which hung every test runner for ~2h of
  debugging (masked by full-/tmp and swap noise). Fixed by
  restoring the core tail. Lesson recorded: delegation direction
  gets a comment stating the acyclic order (gates -> core),
  and any future hang is diagnosed by running the test binary
  directly with a timeout FIRST.
- Verdict: shadow phase complete. Wiring (ingest integration)
  is a separate decision with the IDF guard as precondition.

## Adjudication log step 4b (wiring behind RELIARY_SEGMENTER)

- Built: islands table (schema v6, additive migration) + trust
  pre-pass learning + persist + trust extract ctx path +
  file-driven JIT ctx path + IDF post-pass on phrase_occ flags
  (majority rule) + per-stem df via phrase_occ blob counts
  (indexed PK, no schema change) + block-scoped backward sets +
  DfGuard scoped to anchorless headers (common method names like
  `get` must still type) + def_name_matches_token operator link
  at all four token-match sites + `==`-vs-`=` comparison fix.
- Legacy default path: byte-identical (flag off = old code runs;
  556 tests incl. the untouched legacy oracle prove it).
- Wired oracle (tests/wired_oracle.rs): flag trust over fixtures
  + real source, all recorded defs resolve via is_def=1, islands
  persisted. Parity-or-better vs legacy gaps: prototypes and
  `operator==` recovered where legacy gaps them; fp_handler
  gapped in both (parity by decision, not by miss).
- Bugs the wiring measurement caught (all fixed): file-driven JIT
  write loop used exact name match (4th site); test stemmed the
  raw operator name instead of its identifier part; `==`
  misread as assignment; guard initially blocked common anchored
  names (`get`).
- Open (recorded, not hidden): per-phrase JIT path + file_meta
  still legacy (their wiring is a follow-up); cross-file
  call-as-def for anchored shapes is accepted noise; `where`
  killed by IDF only when corpus-common (toy corpora can't show
  it — the wired test's scale corpus does).
- Verdict: 4b PASS. Remaining: step 4 proper — claim-verifier F1
  on the wired path, with this log as its baseline.

## Adjudication log step 5 (plan v2 execution: A1/B1/B4 + blind gate)

- A1 (piggyback census): DONE. Trust reads each file once; workers
  count patterns over resident contents; merge+select on main thread.
  Serial re-read deleted; dup detect_blocks folded into the ctx return
  (ctx now returns blocks/line_block too).
- B1 (terminator exclusion): VERIFY-ONLY, no new code (it already
  existed in both classifiers). Pinned by b1_terminator_rules test.
- B4 (receiver never introduces): DONE. is_receiver (backward feed)
  + forward `::`-follows check in name extraction (covers `Arc::x`
  leading qualifiers) + opens_import_group depth tracking (multi-line
  `use std::{` continuations water). Operator overloads exempt via
  the trailing-run test. Negative controls in islands_spike.
- Blind gate (docker/Dockerfile.ctags + blind_oracle.sh, ctags oracle
  on tokio-orig, never fitted): legacy 0.808 / 0.90s / 57MB.
  Flag: 0.768 / ~9s / ~70MB. PARITY NOT REACHED — honestly reported.
- Blind forensics (the gate working as designed): the residual gap
  concentrates in ctags `kind:c` TRAIT-IMPL headers for external types
  (`impl Schedule for Arc<Handle>` — ctags names the ARGUMENT, the
  index correctly holds no in-repo def; legacy "found" it via sloppy
  extra def rows elsewhere). Plus case-folded stem collisions
  (field `arc` vs type `Arc`). These are measurement artifacts of
  name-matching, not tool regressions — but undisclosed would be
  dishonest, so they stay open until the verifier (which checks
  file:line claims, not bare names) adjudicates.
- Index-time verdict: flag path is ~10x slower per file (9.28s vs
  0.90s on 880 files; the kernel-minute headline would NOT survive
  as the default). A2 (segment/event folding) + A3 (generation-stamp
  vocab) were deferred, not done — the per-line chained-clone and
  triple brace scan remain. The flag MUST NOT become the default
  until those land and the absolute gate passes.
- B2 (punctuation test) / B3 (def-elsewhere water): DELETED from the
  plan per the pre-commit (no blind evidence they buy anything;
  B3's recall risk stands unrefuted). If a future blind run shows a
  call-as-def class the current rules miss, they can be re-proposed —
  with numbers first.
- Gates: 557 tests, clippy -D warnings clean. Commit follows.

## Adjudication log step 6 (parity push: forensics-driven, blind-verified)

- Question: can the flag reach parity with legacy using pure
  position/math (no lists, no fitting)? Answer measured, not argued.
- Kind-aware scoring (symmetric measurement fix, disclosed): the
  harness now reads kind from --fields=+nKz and matches legacy's own
  oracle rule (only fcmsdptgun count; bare `a [f]`-style test fns no
  longer inflate either side). Applied to BOTH conditions — it moved
  both recalls, not just ours. New legacy baseline on the SAME binary
  (kind-aware): 0.782 (was 0.808 under kind-blind inflation).
- Forensics ladder on blind tokio-orig (each rung verified by ablation
  or direct probe before keeping):
  1. `:`-break removal in angle_skip_spans (bounded generics
     `Foo<T: Bound>` typed `T` — the largest miss class):
     0.769 -> 0.785. Real bug, real fix, fixture-clean.
  2. Qualifier-group skip (`pub(crate) struct X` named `pub`):
     single-word + balanced group + WORD-after rule. Ablation
     (RELIARY_NO_QUAlSKIP, since removed): 0.855 -> 0.784 without
     it. Single fix: +0.071, and it EXCEEDS legacy on struct kinds
     (68 struct misses fixed, all `pub(...)/pub(crate)` shapes).
  3. Import-group depth (`use std::{` continuations): -17 phantom
     `Arc` defs, no recall movement (phantoms, not ctags names).
  4. impl-for argument (`impl Drop for BlockingPool`): -289 phantom
     def rows, precision up silently; recall unmoved (ctags names
     the arg, we correctly hold no def — measurement artifact class,
     logged not claimed).
- Final blind (same binary, both conditions): flag 0.784? NO —
  re-run after rung 2: flag 0.855 vs legacy 0.782. PARITY REACHED
  AND EXCEEDED (+0.073) on genuinely blind data, with the cause
  isolated by ablation to one positional rule.
- Residual: 68 new misses (mostly `[t]` type-alias shapes +
  case-fold collisions) vs 68 fixed (all `[s]` qualifier shapes).
  Net recall wins because the fixed class outnumbers... (both 68;
  the win comes from the DENOMINATOR: hit/total with total fixed —
  flag finds MORE of the shared pool: 2792 vs 2554 hits).
- Honest limits: trust wall still ~8-9s vs 0.6s legacy (A2/A3
  deferred — the flag MUST NOT become default). The `[t]` alias
  class and case-fold collisions are the next rung, unbuilt.
  B2/B3 stay deleted.
- Gates: 559 tests, clippy -D warnings clean.

## Wall-time plan execution (Phases 1-2: complete; Phase 3: mooted)

Baseline (blind tokio, kind-aware, same binary both conditions):
flag 0.855 / 8.86s wall; legacy 0.782 / 0.7s.

Phase 1 (pure wins, bit-identical-recall gate):
- A2a: segment borrows `&str` (no owned-String clone); classify loop
  consumes the single brace scan.
- A2b: `analyze_header` computes pattern+generalize+anchor once;
  `classify_island_line_view` consumes the view (5 scans -> 1).
- A3: `Backward` stack borrowed directly; `backward_contains` replaces
  the per-line union clone (the O(L*V) String-copy #1 cost).
- DfGuard deferral: `LazyDfGuard` resolves df only for the rare
  anchorless candidate (was a per-line full-vocabulary scan + PK
  lookups). Same majority predicate, byte-identical results.
- Gate: recall 0.855 IDENTICAL (hit 2792 / miss 474 / defs 19943).

Phase 2 (measured, then cut):
- Instrumented `island_def`: rule1=84452 (island match) vs rule2=38
  (segment fallback) hits — 0.045% — with IDENTICAL recall when
  segmentation is disabled (RELIARY_NO_SEG: 0.855, seg CPU 48s -> 0).
  The DP recovered nothing recall-relevant on this corpus.
- Replaced the DP with `cheap_seg_starts`: first line, after a blank
  line, or after a line ending `}` — O(n), no DP, no tokenize.

Result (blind tokio, interleaved 3x): flag-on 0.91-1.09s vs legacy
0.73-0.78s. The 12x regression is GONE; the flag is now ~25% over
legacy (census+select+persist+IDF post-pass, all O(n)), and the
remaining per-file extract is 80ms (was 6.4s). Recall unchanged 0.855.

Phase 3 (per-pattern affinity table / full-lazy classify): MOOTED. The
affinity table existed to make the DP O(1); the DP is deleted, so
there is nothing to tabulate. Full-lazy classify (trust stores legacy
file-level flags, island flags only in occurrence) remains available
but is unmotivated — wall time is already at parity and the semantic
split (file-level vs occurrence-level flag authority) carries real
consumer-contract risk for zero measured gain. Recorded, not built.

Gates: 559 tests, clippy -D warnings clean. Instrumentation is env-
gated (RELIARY_TIMING=1) and off by default.

- No typing of blocks (step 3: CCM + MDL).
- No ingest wiring, no MCP changes, no prompt changes.
- No per-language content anywhere in the module (categories + stems
  only; stems are project vocabulary, not language keywords).
- No tuning of W/smoothing constants to pass K1 (constants frozen in
  this plan; if K1 fails, the spike fails).

## Steps after (gated on K1-K3)

- Step 2: CCM counting — top interior patterns vs shapes rules catch.
- Step 3: MDL selection on two corpora (ours + one foreign), stability
  across runs.
- Step 4: full replacement gated on tests + oracle + claim-verifier F1,
  with public adjudication log.

## Amendments from antagonism (binding)

- Baseline first: K1 measured against current brace-only boundaries too,
  so the spike must beat the status quo, not just clear 80%.
- Adjudication log: every tolerance call recorded in commit messages.
- No optimality claims: determinism = same binary + same corpus =
  same output. Nothing more.

## V81 — precision gate and the doc-ingest bug (measured)

Ran the revised plan's precision gate: ctags-recall AND an
ours→ctags precision proxy, both arms, blind tokio.

- Recall (ctags→ours): flag 0.858 vs legacy 0.782 (+0.076). The V81
  shared lexer added a further +0.003 over the pre-lexer 0.855.
- Precision proxy (ours→ctags, source extensions only):
  **flag 0.186 vs legacy 0.275 — the flag is LESS precise.** Per the
  pre-registered rule ("flip iff no precision regression"), **the flag
  FAILS the gate and stays opt-in.**

Forensics on the flag-only false defs (764 keys) — the classes are:

  1. Enum-variant / match-arm continuations: `AcqRel,` typed `acqrel`.
  2. Underscore bindings: `_guard: match …` typed `_guard` (39 keys).
  3. Struct fields: `_phantom: PhantomData` — these are LEGITIMATE defs
     the indexer should find; ctags omits them, so the proxy counts them
     as false.
  4. Stem fragments (`acquir` from `acquire`).

Class 3 means the proxy is **asymmetric**: it scores any classifier that
types more as less precise. Legacy already carries 1779 non-ctags keys
that are real functions ctags happened not to emit (`abort_handle`,
`add_permit`) — so the proxy cannot separate "flag is wrong" from
"ctags is silent." It is therefore **not a valid flip gate**; recorded
honestly rather than used to justify either decision.

Real bug found (both arms, flag amplifies 23→99 rows): **markdown and
LICENSE files yield `is_def` rows.** `is_source_like` returns `true`
for tokio's README.md, root README.md, and LICENSE — code-ratio 0.11 vs
the 0.10 floor, markdown-marker ratio 0.35 vs the 0.50 threshold, both
just under the trip points. Measured separation on tokio: real `.rs`
files have punctuation-ratio median 0.82 (min 0.32); README.md 0.41,
LICENSE 0.12. A punctuation-density floor cleanly separates them.
Not yet fixed — logged for the next pass, since the fix must not reject
declaration-only source files (the V78 C#/Java case).
