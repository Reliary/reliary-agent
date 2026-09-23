# V78 Next Steps Plan

## Current state (V77b, honest read)

### 3-way comparison (4 seeds, RELIARY_GT=1, `v75-canon-replay` corpus)

| Metric | A (reliary) | B (altbackend) | C (grep) |
|--------|-------------|----------------|----------|
| **F1 (claim-weighted)** | **0.963** | 0.495 | 0.618 |
| Precision | 0.975 | 0.887 | 0.898 |
| Recall | 0.951 | 0.348 | 0.476 |
| Coverage | 1.00 | 0.88 | 0.82 |
| Keyword rubric (do not cite) | 28.0 ±0.0 | 26.0 ±1.2 | 26.5 ±0.6 |
| Billed | 21,997 | 23,694 | 38,930 |
| Dead-ends | 0 | 4 | 0 |
| Wall (median) | 29s | 31s | 35s |
| Tool bytes | 8,364 | 13,405 | 39,802 |

Reliary leads F1 by +0.345 over grep and +0.468 over altbackend, at 44% lower billed than grep.

### Caveats (must not be papered over)

1. **q9 was targeted.** We saw it fail → fixed it → re-measured. The *fix* is general
   (PascalCase keyword-drop class bug + `derive()` detection + path_filter wiring) but
   the *evaluation loop* on q9 is fitting-shaped. Disclose, do not count as free win.
2. **Keyword rubric is gameable.** 28.0/30 measures substring recall, inflates real
   accuracy ~2×. Never cite as accuracy. Zero variance = easy lookups, not solved hard
   problems.
3. **F1 verifier has format bias.** Claim extractor looks for `symbol at file:line`.
   Our tools emit exactly that; grep prose ("in structural.rs around line 31") extracts
   worse when correct. Part of the +0.345 gap is measurement artifact.
4. **8/10 questions are one-call lookups.** This is a retrieval benchmark, not a coding
   benchmark. It measures "call the right tool once and copy the answer."
5. **Private corpus = no training priors = model copies more.** High F1 partly measures
   trust in tool output on unknown code, not general superiority.

### What is solidly real

- PascalCase keyword-drop fix: `Default`/`Fn`/`String`/`Box`/`None` were silently
  unindexable. Class bug, any language, any PascalCase identifier. Independent of bench.
- `derive(Trait)` detection: grammar-free, general.
- path_filter wiring: bare `path_filter` without `def_only` was returning cross-crate
  noise or empty. General dispatch bug.
- Cost/wall/dead-end advantages are method-independent (no format bias in billed bytes
  or tool-call counts).
- 0 dead-ends across 4 seeds vs altbackend's 4 — model never enters retry loops.

---

## Goals for V78

1. **Kill measurement bias** so the F1 gap is trustworthy or disappears honestly.
2. **Build harder benchmarks** that cannot be won by one-call copy-paste.
3. **Close residual quality gaps** (q7 precision, q5 recall) with general fixes only.
4. **Ship honest docs** with caveats disclosed alongside the wins.

---

## Phase 1 — Measurement honesty (no product changes, ~half day)

**Status: DONE (V78-P1/P3).** Broadened extractor landed with 7 negative controls;
re-verified all three conditions; GT source-audited. Fair 3-way (4 seeds, claim-weighted):
A F1 0.949 / B 0.452 / C 0.622. The earlier 0.963 was partly format-favoured — the
broadened extractor dropped A 0.963→0.872 and C 0.618→0.591, so the lead shrank but held
(A−C gap was inflated ~19% by format). Later Phase-3 extractor fixes (prose-word shape
filter, grouped-caller inheritance) lifted both fairly to the current numbers.

**Goal:** Make claim extraction fair across answer formats so F1 is comparable.

- [ ] **M1. Broaden claim extractor** in `bench/deterministic_verify.py`:
  - Accept `file.rs line N`, `file.rs:N`, `file.rs (line N)`, bare `symbol` + nearby
    file mention, prose "X defined in file.rs".
  - Symmetric across A/B/C: same extractor, same tolerance, no per-condition rules.
  - Keep strict mode as secondary metric (exact `symbol at file:line`) for regression
    detection, but primary report = broadened extractor.
- [ ] **M2. Re-verify all three conditions** with the broadened extractor on existing
  `v77b_live.jsonl` + `v77b_bc.jsonl` (zero LLM spend).
  - If A's lead shrinks but holds → confidence up.
  - If A's lead collapses → we were measuring format, not accuracy. Disclose loudly.
- [ ] **M3. Negative control:** inject a "correct-looking but wrong" answer (right
  format, wrong file) and confirm F1 penalizes it. Inject a "right answer, wrong
  format" and confirm broadened extractor still credits it.
- [ ] **M4. Freeze GT.** Run existing `bench/gt_audit.py` against current corpus;
  fix any stale facts mechanically. No hand-editing after seeing answers.

**Deliverable:** one verifier version, three conditions re-scored, caveats table in
README with measurement disclosure.

**Kill criterion:** if broadened extractor cannot be made symmetric without
condition-specific hacks, keep dual-metric report (strict + broad) and stop.

---

## Phase 2 — Difficulty upgrade (real coding benchmark, ~1–2 days)

**Goal:** Questions that cannot be won by one tool call + copy.

### 2a. Mutations that force localization (resume V53, pre-registered)

- [ ] **D1. Balance check first** — pilot died on 402. Confirm balance before any run.
- [ ] **D2. Pre-registration (commit before run):**
  - Task list (12 mutations), seeds, paired test, kill criterion — all in git before
    first API call.
  - Mutations derived from *this repo's actual git-history bug fixes*, stratified:
    local-name (rename, off-by-one) vs cross-file (broken call, wrong dispatch).
  - Harder than V53 pilot: symptom-only prompts that never name the file or symbol;
    target tests must fail pre-fix (validated mechanically).
- [ ] **D3. Score = test outcome** (f2p 0–3), not claim extraction. Immune to format
  bias by construction.
- [ ] **D4. Kill criterion (pre-registered):** reliary f2p ≥ both competitors by
  +15pp, else report honestly as tie/fail.

### 2b. Cross-repo generalization (gated on 2a)

- [ ] **D5. Generator audit** — `auto_questions.py` is Rust-shaped. Per-language
  `is_def` smoke test (Python/Go/TS) before any LLM spend. Grammar-free detection
  must actually work or multi-repo bench is fiction.
- [ ] **D6. Second foreign repo** — one more non-reliary corpus with auto-generated
  questions + mechanical GT. Proves no corpus-specific tuning.

**Deliverable:** mutation bench results + at least one foreign-repo F1 table.

**Non-goals:** rewriting history, changing questions after seeing answers, adding
verifier fact types mid-run.

---

## Phase 3 — Residual quality gaps (general fixes only, ~half day)

**Status: DONE (V78-P3/P3b), with one skipped.** Product fixes: pack fields now carry
type + line (q1 0.00→1.00), pack L2 line was 0-indexed (fixed), verifier accepts
enclosing-function caller claims (q2 measurement). Measurement fixes (disclosed
separately): case-insensitive stop-word filter, grouped-caller inheritance (q2 0.56→0.89).
q7's lone miss is the model paraphrasing away the impl-line fact — correctly scored, no
fix. q1 method recall was skipped as fitting risk; q9 remains model-routing-dependent.

From V77b per-query data — only fix if the root cause is a general bug:

- [ ] **Q1. q7 methods precision 0.88** — 1 unverified claim among 8. Root-cause
  (likely a phantom method line or wrong file). Fix the bug class, not the question.
- [ ] **Q2. q5 callers recall 0.83** — 5 of 6 GT facts found. Check missing caller
  (test-file filter? boundary check?). If missing caller is legitimate (e.g. in
  `target/`), correct GT via `gt_audit.py` mechanically; if a real caller is dropped,
  fix the filter.
- [ ] **Q3. q8 chain precision 0.67** — 2 claims, 1 verified. Small-n; inspect the
  unverified claim. Likely `call_graph` callee noise (returns non-defs).
- [ ] **Q4. q4 callgraph precision 0.92** — 1 unverified of 13. Same class as Q3.

**Rule:** every fix must pass "would this help a user who never heard of the bench?"
If no, discard.

---

## Phase 4 — Documentation & ship posture (~half day)

- [ ] **S1. README/AGENTS honesty block:**
  - Publish F1 0.963 / 0.495 / 0.618 **only after Phase 1** re-verification.
  - Always pair with: private-corpus caveat, format-bias caveat, retrieval-not-editing
    caveat, q9-targeting disclosure.
  - Never cite keyword rubric as accuracy (already stated; keep).
- [ ] **S2. Update CHANGELOG** Unreleased: PascalCase keyword class bug, derive()
  detection, path_filter wiring, measurement fixes (disclosed separately).
- [ ] **S3. Push strategy** — signed squash to `master-rebuild` per established
  pattern; local `feat/v59-accuracy` retains full history. Confirm no login-name
  leakage before push.
- [ ] **S4. Tag** only if user directs (prior instruction was ship without tagging).

---

## Decision points (ask user)

1. **Phase 1 first?** Recommended — cheap, zero LLM spend for re-verify, decides
   whether we can honestly publish the 0.963.
2. **Phase 2a mutation bench** — requires balance top-up and pre-registration
   discipline. Worth it or park it?
3. **Phase 2b multi-repo** — gated on generator audit. Proceed only if 2a ships?
4. **Dual-metric verifier** if broadened extractor can't be made fair — accept?

---

## Sequencing

```
Phase 1 (measurement)  →  gate: F1 gap survives symmetric extractor?
    │ yes                          │ no
    ▼                              ▼
Phase 3 (quality gaps)      disclose measurement artifact;
    │                        keep strict+strict metrics;
    ▼                        skip to Phase 4 docs
Phase 2a (edit bench)  [if balance + pre-reg accepted]
    │
Phase 2b (multi-repo)  [if generator audit passes]
    │
Phase 4 (docs + push)
```

Phase 3 can run in parallel with Phase 1 (different files, no conflict).

---

## Explicit non-goals

- No question rewrites after seeing model answers.
- No verifier fact types added mid-run to improve our score.
- No keyword-rubric chasing.
- No history rewriting / cache-breaking conversation compression.
- No model-ceiling excuses — measurement and tool output remain the levers.
