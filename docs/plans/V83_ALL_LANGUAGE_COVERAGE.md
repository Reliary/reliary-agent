# V83 — All-Language Coverage (antagonised plan)

Status: P0 + B1-B12 landed (818f896..6bdb552).
Per-language blind oracle (site-level, file-aware ctags truth):

| Corpus | legacy | island | notes |
|---|---|---|---|
| tokio (Rust) | 0.711 | **0.853** | 0.912 stem-tolerant; 11,106 sites |
| requests (Python) | 0.668 | **0.698** | above legacy parity |
| rack (Ruby) | 0.385 | **0.618** | 0.794 tolerant |
| aeson (Haskell) | 0.163 | **0.502** | 0.557 tolerant; was ZERO at V82 |
| ocaml-lsp (OCaml) | 0.282 | **0.389** | 0.419 tolerant; layout weakest |

Fixes: B1/B2 decl-assign, B3 keyword-name defs, B4 imports must not
vaccinate, B5 enclosing-frame def insertion, B6 control-flow gate,
B7 repeat-def evidence, B8 declaration-line names + type-syntax guard,
B9 equation rule, B10 binder-led relaxed equations + delimiter-free
island rejection, B11 source-likeness accepts binding/declaration shapes
(62 dead .ml/.mli files recovered), B12 keyword-shaped equation names
after value binders (`let raise t = ...`).

Remaining measured classes: OCaml ppx/let% macro sites (619, name `%`
untypeable), Haskell record fields/constructors (excluded kinds),
OCaml implicit-module sites (313, ctags names the file's module after the
basename), operator methods (`()`, `*` — no identifier to bind).
Constraint (user): all languages covered; no regression in speed or accuracy.
Trust wall 0.54-0.92 s on tokio; 583 tests; clippy clean.

## 0. What the first half of this arc got wrong (retractions first)

1. **The ruler is broken twice over, not once.**
   - `docker/blind_oracle.sh` piped ctags through
     `awk '{print $1"\t"$(NF-1)"\t"$NF}'`. For any ctags line carrying a
     trailing scope/signature field (methods carry `implementation:`, fields
     carry `struct:`, variants carry `enum:`), the "last two fields" are
     `line:N` plus the scope — **kind and line are both lost**. Measured on
     the committed oracle file: 6,851 of 13,098 lines (52%) mangled; all
     4,120 methods and 1,366 struct fields dropped.
   - The inline report then matched `kind:(\w)` — **one character** — against
     the letter set `fcmsdptgun`, silently dropping `enum`, `enumerator`,
     `interface`, `variable` even when the kind survived the awk.
   - Consequence: every recall number reported so far (0.782 / 0.855 / 0.858 /
     0.865) was computed against ~40% of the truth and is void. The earlier
     "kind-filter correction" re-filtered the same corrupt file and is also
     void. The commit message and SEGMENTER_SPIKE.md entries quoting them
     must be corrected when P0 lands.
2. **"Regex heuristic is strictly better than today" was false.** It wins in
   value position (after `( = , : [`), stays blind after identifiers
   (`return /re/`) and after `)`, and adds a small false-fire surface. Better
   on average; not strictly.
3. **A tuned heredoc cap (N lines) would contradict our own no-magic-numbers
   rule.** Replaced by a derived bound (P2).
4. **Hand-authored stress fixtures would be authoring to our strengths.**
   Replaced by pinned real corpora per language, graded mechanically.
5. **The identifier scan is ASCII-only** (`scan_identifiers`:
   `is_ascii_alphabetic`), so Unicode identifiers (`café`) truncate and
   operator definitions (`>>=` in Haskell) are invisible. Not on the original
   list; recorded as a named gap.

## 1. The information-theoretic ceiling, stated honestly

- Shannon: two constructs with identical positional signatures are
  **indistinguishable** to a position-only classifier. `foo(:bar)` and
  `define_method(:foo)` are the same shape at every byte; no positional rule
  splits them. This is a theorem, not laziness.
- Rice: "is this line a definition" is semantic; no static method is total.
- Therefore "all languages covered" is defined **operationally**:
  per-language recall >= legacy on a pinned real corpus per language,
  published as a table, with the residual gap named per language.
  The escape route past the ceiling is not more rules but more *information*:
  corpus statistics (§2) and consistency invariants.

## 2. Science-grounded design (replacing per-language rules)

- **L — the layout rule is a positional algorithm.** The Haskell layout rule
  (Haskell 2010 report §10.3) is a formal function of columns; layout
  languages need no heuristics. `detect_blocks` already tracks indentation;
  extend it to emit layout blocks. The equation rule then reads: a depth-0
  `=` line at the layout column opens a binding of its first identifier
  (skipping one leading declaration word). Covers Haskell `foo x = ...`,
  OCaml `let f x = ...`, point-free `let f = m`, Python/JS top-level
  constants — while `let x = foo()` inside a body stays a local. Applies to
  both classifiers (legacy + island); shadow audit must agree.
- **Empirical priors (Bayes), never keyword lists.** `boltzmann.rs` already
  turns scores into calibrated probabilities; extend the same discipline to
  def classification: P(def | shape, depth, novelty, indent ratio) estimated
  from the corpus's own statistics. Train tree A, evaluate tree B; never the
  same tree. This converts "ship a Ruby DSL heuristic?" into a posterior.
- **Good-Turing** for shapes the corpus has never shown: reserve probability
  mass for unseen shapes instead of silently defaulting to not-a-def.
- **Consistency repair (Codd/Chase):** within-scope uniqueness — two defs of
  one name in one block means at least one is wrong (or reassignment). A
  deterministic post-pass demotes implausible duplicates. Language-neutral
  precision backstop.
- **Import-aware disambiguation** for name collisions (TS barrel re-exports,
  same-named methods): `import { X } from './x'` is positional; resolve a
  caller to the def set of the module it imports before centrality ranking.
- **Rust-isms:** const transition table for lexer states; memchr/bitset
  delimiter scans already landed; additions stay branch-light in the hot
  loop (`count_braces` runs per file across several passes).

## 3. Phases with gates and falsifiers

### P0 — fix the ruler (this turn)
- Delete the awk transform; append raw ctags lines.
- Extract the report to `docker/oracle_report.py`: full-word kinds,
  multi-field scan, per-kind hit/miss table in the report.
- Golden-fixture test + negative controls (control must fail on the old awk
  transform and on the old one-char kind regex).
- Re-derive both arms on the blind tokio corpus; correct the record.

### P1 — corpora
Pinned real repos, one per language family: Rust, C, Python, Go, JS/TS, C#,
Ruby, Haskell, OCaml, shell. Small (<= a few hundred files each). Graded by
the fixed oracle. Published coverage table.
Gate: no tuning against these numbers except via the P2/P3 protocols
(fix -> measure -> keep or revert).

### P2 — lexer shapes (phantom-structure only; cannot hide real braces)
- Heredocs: opener `<<` + all-caps or quoted word; terminate at the word;
  **unterminated -> blank to EOF**, the same precedent as unterminated
  strings. No cap constant. Falsifier: zero files across the ten corpora may
  lose more than 1% of lines to a misfired heredoc; else restrict (value
  position) or revert.
- Regex literals (JS/Ruby): previous-char-class heuristic; measure FP/TP on
  the corpora; revert if net-negative anywhere.

### P3 — classifier shapes
- Layout/equation rule (§2). Per-corpus parity gate on recall AND precision.
- `macro_rules!` multi-line signature remainder (~191 tokio fns, tracked
  since V80).
- Ruby/DSL symbols (`define_method(:foo)`, `attr_accessor :x`): ship only if
  P1 shows the shape matters and the rule adds zero false defs on the ten
  corpora; otherwise a documented limit with numbers, not silence.

### P4 — collisions and invariants (only if P1 shows mis-ranking)
- Import-aware resolution; consistency repair pass.

### P5 — published table
`docs/BENCHMARKS.md` gains a per-language coverage table with the residual
gap named per language. The SEGMENTER_SPIKE record is corrected with P0
numbers.

## 4. Speed budget

Every addition is branch-level inside existing single passes; the lexer gains
at most three state bytes; no new file passes. Gate: tokio `trust` wall
within noise of the current 0.6-1.2 s; per-corpus wall recorded before and
after each phase.

## 5. Rejected

- Tuned heredoc cap; per-language keyword lists; any rule whose admission is
  not decided by corpus measurement; "strictly better" claims without a
  measured false-fire surface.
