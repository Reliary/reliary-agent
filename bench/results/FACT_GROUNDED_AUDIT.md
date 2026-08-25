# reliary8 — Fact-Grounded Benchmark Audit

**Source**: Only data in `/home/user/src/reliary8/bench/results/` and `/home/user/src/reliary8/bench/`.
**Method**: Read raw JSONL/JSON + the bench-author's own MD reports. No marketing docs consulted.
**Date of data**: arc27–arc64 (Jun 27 – Jul 2 2026).

---

## 1. Type-flow disambiguation accuracy (`find_references_with_source`)

### What the AGENTS.md headline claims
> reliary 0.299 median vs grep 0.154 vs ALTBACKEND 0.000

### What the data actually shows

**The 0.299 number does not appear in any current result file.** The bench author's own `arc39_final_llm_bench.md` explicitly states:

> "The arc29 Phase 4b wins (reliary 8-1, jaccard 0.294) **cannot be reproduced on the current DeepSeek API**. ... The trajectory is real: Grep is the strongest backend on this prompt format. ALTBACKEND is middle. Reliary is the weakest of the three (loses head-to-head 2/13)."

Verified against `arc39-compare-cost-n20-no-think.jsonl` (n=39, the "fixed" thinking-disabled run):

| Cond | median jacc | mean jacc | max | zero-preds |
|---|---|---|---|---|
| A (reliary) | **0.143** | 0.262 | 0.960 | 3/13 |
| B (altbackend) | **0.221** | 0.192 | 0.385 | 4/13 |
| C (grep) | **0.167** | 0.315 | 0.781 | 1/13 |

Head-to-head wins (from `arc39_final_llm_bench.md`): **Grep 7/13, ALTBACKEND 4/13, Reliary 2/13.**

`arc40_grep_format_report.md` (grep-format output variant, n=20): A median 0.111, B 0.143, C 0.169; wins A=4, B=0, C=6. The plan gate was "median ≥ 0.3 AND wins ≥ 8/13" — **explicitly FAILED** (median 0.111, wins 4/13).

### Deterministic homonym bench (no LLM) — `arc39-homonyms-tokio.json`, n=50 anchors
This measures the tool itself, not LLM judgment:

| Metric | mAP (median) | mAP (mean) | P@5 |
|---|---|---|---|
| strict (exact label match) | **0.1493** | 0.1726 | 0.20 |
| loose (related-label) | 0.2267 | 0.2535 | 0.20 |
| auto (oracle, diagnostic only) | 0.5496 | 0.4809 | 0.00 |

Pass gate (loose mAP ≥ 0.5): **FAIL**. `pass_strict=False, pass_loose=False, pass_auto=True`.

### What Jaccard 0.299 / 0.143 actually means
Jaccard = |predictions ∩ ground_truth| / |predictions ∪ ground_truth| over the *file:line set* the LLM returned vs the oracle set. Per the bench design doc, the LLM is shown tool output for one anchor and must emit a JSON array of `file:line` strings it believes match the anchor's role.

- 0.143 median ≈ the LLM returned a set that overlaps the oracle by ~14% of the union. Most runs returned ~9 predictions vs grep's ~34 (lower recall, not necessarily lower precision).
- The bench author's own honest reading (`arc39_final_llm_bench.md`): "Reliary's extra structure (similarity + source) doesn't help and slightly hurts in this prompt design."

### Per-task correctness (multi-turn rubric, not similarity) — `multi_turn_report.md`, n=45 (5 tasks × 3 cond × 3 seeds)

| Task | A(reliary) | B(altbackend) | C(grep) | Winner |
|---|---|---|---|---|
| consume_impls | 3.00 | 3.00 | 3.00 | tie |
| block_on_chain | 2.00 | 3.00 | 2.67 | **B** |
| bufwriter_write_chain | 2.00 | 3.00 | 2.33 | **B** |
| split_return_type | 2.33 | 2.00 | 2.00 | **A** |
| poll_method_search | 3.00 | 3.00 | 3.00 | tie |

Reliary wins **1/5** tasks; ALTBACKEND wins 2/5; ties on 2/5. Overall ALTBACKEND 2.80 > grep 2.60 > reliary 2.47. Author flags: "Differences between 2.0 and 2.33 score are within noise" (n=15 per cond, within 2.7× LLM variance).

---

## 2. Score parity vs ALTBACKEND — the "2.63 vs 2.70 vs 2.64" claim

### The claim is NOT supported by the multi_turn data.
`multi_turn_report.md` shows ALTBACKEND 2.80, grep 2.60, reliary 2.47. **ALTBACKEND beats reliary by 0.33**, outside the "no statistically significant winner" framing. The 2.63/2.70/2.64 numbers may come from an earlier arc not present in results/ as a per-task breakdown — I could not locate a file with those exact aggregate scores. The closest is `multi_turn_full.jsonl` (the 45-run source), whose per-condition medians are the 2.80/2.60/2.47 above.

### Where reliary actually wins vs ALTBACKEND (per `multi_turn_report.md`)
- **Weighted cost**: 3975 vs altbackend 4447 (−11%)
- **Tool output bytes**: 431 vs altbackend 1402 (−69%) and grep 3650 (−88%) — "the killer metric"
- **Wall time**: tied at 8s median (all three)
- **One specialized task** (split_return_type): 2.33 vs 2.00 — ALTBACKEND can't resolve `Option<Self>` from labels alone

### Where reliary loses to ALTBACKEND
- **Multi-hop call chains** (block_on, bufwriter): 2.00 vs 3.00. ALTBACKEND's `trace_path` returns the chain directly; reliary's `callgraph` needs an anchor_file+anchor_line the LLM must discover via grep-fallback first (extra round-trip).
- **Overall task score**: 2.47 vs 2.80.

### Long-session (arc60–arc64, 10 queries, 2 cond × 6 files aggregated by me)
Aggregated medians across `long_session_arc60/60v2/61/61_clean/62/62_methods/63/63_v2/63_v3/64/64_s2/64_v2.jsonl`:

| Cond | n | wall | weighted_cost | score/30 | tool_bytes | turns | calls |
|---|---|---|---|---|---|---|---|
| A (reliary) | 12 | **52.3s** | 177,350 | 24 | 18,586 | 35 | 25 |
| B (altbackend) | 12 | 60.3s | **121,282** | **25** | **7,460** | 42 | 32 |

**In long sessions reliary is faster (wall 52s vs 60s) but ALTBACKEND is cheaper (WC 121K vs 177K) AND scores higher (25 vs 24/30) AND uses fewer tool bytes (7.5K vs 18.6K).** This inverts the multi_turn short-session cost advantage — reliary's per-call output is small but it makes MORE total calls/turns accumulating tokens. The "reliary cheapest on tokens" claim holds in short sessions but **does not hold in the most recent long-session data**.

---

## 3. The "ALTBACKEND 0.000" claim — is it rigged?

### Two different benches, two different ALTBACKEND treatments

**arc39/arc40 single-turn (where the 0.000 comes from)**: The LLM gets ONE tool call. ALTBACKEND is given `search_graph` which returns `{name, label, file, line}` — no source code. The LLM "doesn't know to call `get_code_snippet` without hints" because **it isn't offered `get_code_snippet` in this single-call format.** ALTBACKEND's 0.000 (arc40) / 0.221 (arc39 no-think) reflects a constraint of the bench design, not ALTBACKEND's capability.

**multi_turn_harness.py (the fair bench)**: ALTBACKEND is given ALL FOUR tools — `search_graph`, `get_code_snippet`, `trace_path`, `get_architecture` (lines 246–303, 386–400). The system prompt advertises all four (lines 439–440). ALTBACKEND wins this bench 2.80 vs 2.47.

### Verdict: partially rigged
The 0.000 is a **real measurement but of a handicapped setup** — it measures "ALTBACKEND with only search_graph, no snippet tool" against "reliary's combined find+source tool". It is not an apples-to-apples capability comparison. The AGENTS.md presents the 0.000 as if ALTBACKEND is fundamentally broken, when the same author's multi_turn harness shows ALTBACKEND winning overall when given its full toolset. The honest comparison is `multi_turn_report.md`, where ALTBACKEND is fairly tooled and **beats reliary**.

The bench author was transparent about this in the report files; the AGENTS.md summary is the part that overstates.

---

## 4. Compression savings

### `unique_compression.jsonl` — the dedicated compression bench
**6 rows, every single one shows `compressed_count: 0` and `total_compressed_savings: 0`.** Compression never fired in this bench. The "on" condition (with `reliary_compress` available) actually cost MORE on two tasks:

| task | cond | wc | wall |
|---|---|---|---|
| consume | on | 8191 | 29.9s |
| consume | off | 5000 | 11.7s |
| block_on | on | 3705 | 11.4s |
| block_on | off | 2626 | 11.7s |
| bufwriter | on | 2601 | 11.7s |
| bufwriter | off | 2098 | 10.2s |

Compression ON is **more expensive** on all three tasks (the LLM made extra tool calls to invoke `compress` that never produced savings). n=1 seed only — weak signal, but it points the wrong way.

### `wrap_deterministic.json` — the `reliary wrap` bash-compression bench
10 commands, measured raw vs wrapped bytes:

| Category | raw_bytes | wrap_bytes | savings% |
|---|---|---|---|
| cargo test | 19,661 | 1,047 | **94.7%** |
| cargo build | 40,804 | 39,544 | 3.1% |
| git status | 142,254 | 142,251 | 0.0% |
| git diff --stat | 76,430 | 76,429 | 0.0% |
| git log | 1,411 | 1,410 | 0.1% |
| grep TODO | 927 | 926 | 0.1% |
| grep unsafe | 575 | 574 | 0.2% |
| find | 1,928 | 1,927 | 0.1% |
| ls | 686 | 573 | 16.5% |
| wc | 3,101 | 2,692 | 13.2% |

**Summary**: total_savings_pct **7.1%**, avg_savings_pct 12.8%. The 94.7% comes entirely from `cargo test` (compiler output is highly compressible). For git/grep/find (the realistic agent workflow commands) savings are **0–0.2%**. Exit codes preserved (good); overhead ~0ms (good).

### The "-43% to -77% weighted cost" project-memory claim
**Not borne out by any data file I could find.** The dedicated compression bench shows 0% savings (compression never fired). The wrap bench shows 7.1% total. I found no JSONL with a -43% to -77% WC delta. If this number exists it predates the results/ directory or was measured by a harness not present here. **Mark as unverified.**

---

## 5. Cross-language capability — `unique_cross_lang.py`

This is a **test script, not a result file.** It creates a 3-file temp corpus (foo.py, foo.rs, foo.js), runs `reliary trust` then `reliary search foo`, and counts substring hits (`.py`/`.rs`/`.js`) in stdout.

**No result file exists** — the script prints to stdout and exits. The pass gate (`py_hits>=1 and rs_hits>=1 and js_hits>=1`) was never persisted. The methodology is weak: it counts `.py` substrings in `reliary search` output, which would count file *paths* containing those extensions — it does NOT verify that symbols were correctly disambiguated across languages, only that files of each extension appear in search results.

The arc38 autolabeler tests (tokio 14/14, hyper 3/3, Python 12/12) confirm the *autolabeler* classifies labels correctly per-language, but that is single-language validation, not cross-language find-references. **Cross-language is claimed but not benchmarked with persisted data.**

---

## 6. Wall time — measured, not projected

PLAN doc projections (154s→80s→60s→40s) are not what the result files show. Measured wall times from the most recent long-session runs (10-query sessions, deepseek-v4-flash):

| File | A wall | B wall |
|---|---|---|
| arc61_clean | 88.4s | 57.5s |
| arc60v2 | 90.5s | 63.4s |
| arc62 | (see below) | 59.4s |
| arc63_v3 | (see below) | — |
| arc64 | (see below) | 61.2s |
| arc64_v2 | **52.4s** | 61.2s |

Aggregated median (arc60–64, n=12 each): **A 52.3s, B 60.3s.** Single-call find_references latency (`arc39_regression_check.md`): **0.76s** (down from arc29's 2.4s). Indexing (`arc33_fast_index_report.md`): tokio 0.85s (from 10.5s, 12× speedup — verified, shipped).

The 40s projection has not been achieved for full 10-query sessions; the best single run is 52.4s. Reliary is currently *faster* than ALTBACKEND on wall time in long sessions (52 vs 60s median), which is real and notable.

---

## 7. Dead-code detection and risk scoring

**Not benchmarked.** Evidence:
- `q10_dead_code` appears in every long_session run. Both A and B consistently score **1/3** (low). Reliary's answers (arc60v2, arc64_v2) say "I cannot directly detect dead symbols due to tool limitations" or attempt to infer from references — scoring 1–2.
- No dedicated bench file for `reliary_dead` / `reliary_dead_symbols` / `reliary_risk` exists in results/. No JSONL, no mAP, no oracle.
- These are features with **no validation data**. The dead-code tool is invoked by the LLM and it cannot answer the bench's dead-code question. Risk scoring has zero benchmark coverage.

---

## 8. Honest assessment — what reliary measurably does better, and where it's parity/worse

### Measurably BETTER (data-backed)
1. **Indexing speed** — 0.85s for tokio (376 files), 12× faster than its own prior build, 3× faster than stria. Verified, shipped, deterministic. (`arc33_fast_index_report.md`)
2. **Single-call query latency** — 0.76s find_references vs 2.4s previously. (`arc39_regression_check.md`)
3. **Wall time in long sessions** — 52.3s vs altbackend 60.3s median (arc60–64, n=12). Real, ~13% faster.
4. **Short-session tool-byte output** — 431 bytes vs altbackend 1402, grep 3650 (multi_turn, n=45). 3–8× less context pressure per call. This is the strongest reliary metric.
5. **One specialized task** — split_return_type (2.33 vs 2.00) where ALTBACKEND's labels can't resolve `Option<Self>`.
6. **Grammar-free autolabeler accuracy** — 100% on tokio/hyper/Python fixtures (deterministic).

### PARITY (no significant difference)
- **Wall time & tool-call count** in short multi-turn sessions — all three backends 8s / 5 calls / 6 turns. (`multi_turn_report.md`)
- **Index features** vs stria — reliary has 19 tools vs stria's phrase search; not directly benchmarked but architecturally broader.

### WORSE (data-backed)
1. **Overall task correctness** — reliary 2.47 vs altbackend 2.80 vs grep 2.60 (multi_turn). Reliary is last.
2. **Multi-hop call chains** — block_on 2.00 vs altbackend 3.00; bufwriter 2.00 vs altbackend 3.00. Reliary's `callgraph` requires an anchor the LLM must grep for first; ALTBACKEND's `trace_path` takes a name directly.
3. **Single-turn find-references jaccard** — reliary 0.143 vs altbackend 0.221 vs grep 0.167 (arc39 no-think). Reliary loses head-to-head 2/13.
4. **Grep-format variant** — reliary 0.111 median, loses 4–6 vs grep. The "destroy ALTBACKEND via grep format" strategy worked against ALTBACKEND (4–0) but **grep is the actual winner**, not reliary. (`arc40_grep_format_report.md`, gate FAILED).
5. **Long-session cost** — reliary WC 177K vs altbackend 121K (arc60–64). Reliary's small per-call output is outweighed by more total calls/turns. The "cheapest on tokens" claim is true short-session but **false long-session**.
6. **Compression** — 0% savings in the dedicated bench (compression never fired); 7.1% in the wrap bench (almost entirely from `cargo test`). The -43% to -77% claim is **unverified by any data file here.**
7. **Dead-code & risk** — not benchmarked; the LLM using reliary scores 1/3 on the dead-code question and says the tool can't do it.

### Things claimed but with NO supporting data file
- "0.299 median jaccard" — not in any current file; bench author says it's irreproducible on the current API.
- "ALTBACKEND 0.000" — real but from a single-tool-call handicap bench; ALTBACKEND wins when given its full toolset.
- "-43% to -77% weighted cost compression" — no file found.
- Cross-language find-references across Python/Rust/JS — no persisted result file.
- Dead-code detection, risk scoring — no benchmark.

---

## File references (all absolute)
- `/home/user/src/reliary8/bench/results/multi_turn_report.md` — fair multi-turn bench, ALTBACKEND wins
- `/home/user/src/reliary8/bench/results/arc39_final_llm_bench.md` — author admits 0.294 irreproducible, reliary weakest
- `/home/user/src/reliary8/bench/results/arc39-compare-cost-n20-no-think.jsonl` — raw n=20 jaccard (A 0.143, B 0.221, C 0.167)
- `/home/user/src/reliary8/bench/results/arc40_grep_format_report.md` — gate FAILED (median 0.111, wins 4/13)
- `/home/user/src/reliary8/bench/results/arc39-homonyms-tokio.json` — deterministic strict mAP 0.1493, gate FAIL
- `/home/user/src/reliary8/bench/results/arc39_regression_check.md` — 0.76s single-call, no regression
- `/home/user/src/reliary8/bench/results/arc33_fast_index_report.md` — 0.85s indexing (12× speedup, verified)
- `/home/user/src/reliary8/bench/results/arc38_smash_ceiling_report.md` — homonym metric definitions
- `/home/user/src/reliary8/bench/results/unique_compression.jsonl` — compression 0% savings, never fired
- `/home/user/src/reliary8/bench/results/wrap_deterministic.json` — wrap 7.1% total savings (94.7% from cargo test only)
- `/home/user/src/reliary8/bench/results/long_session_arc6{0-4}*.jsonl` — long-session wall/cost (A 52.3s/177K, B 60.3s/121K)
- `/home/user/src/reliary8/bench/multi_turn_harness.py` — confirms ALTBACKEND given all 4 tools (fair)
- `/home/user/src/reliary8/bench/unique_cross_lang.py` — cross-lang script, no persisted result
- `/home/user/src/reliary8/bench/RELIARY_BENCH_DESIGN.md` — methodology (interleaved, gate on cost not pass/fail)

---

## Bottom line
Reliary's **real, measured advantages** are indexing speed, single-call latency, short-session tool-byte output, and one type-resolution task. Its **measured disadvantages** are overall task correctness (last of three), multi-hop chains, long-session cost, and find-references jaccard. **Compression, dead-code, risk, and cross-language are claimed but unbenchmarked or bench-failed.** The AGENTS.md headline numbers (0.299, ALTBACKEND 0.000, -43–77%) are either irreproducible, from a handicapped comparison, or unverified — the bench author's own MD reports are more honest than the AGENTS.md summary.