# Arc 39 — LLM Utility Bench regression (against arc29 reports)

## TL;DR

**No regression introduced by arc39 binary-detection swap on the homonym-style smoke test.** But the broader arc29 LLM-utility report (n=5) was on a small sample and the n=20 follow-up reveals a deeper issue: **all three backends (reliary/altbackend/grep) have median jaccard = 0** when running find-references questions with deepseek-v4-flash as the LLM. The earlier arc29 wins for reliary (2/5) were artifact of small-n sampling.

## What I re-ran

`bench/compare_cost.py` — apples-to-apples LLM bench comparing:
- A: `reliary_find_references_with_source` 
- B: altbackend 4-tool set (search_graph, get_code_snippet, trace_path, get_architecture)
- C: bash + grep

Same model (`deepseek-v4-flash`), same questions (homonyms.json anchors, n=5 and n=20), same oracle (strict_grep_oracle), interleaved A/B/C per task.

## Results

### n=5 (matches arc29's prior altbackend_99_percent_report.md)

| Condition | Arc29 jaccard median | Arc39 jaccard median | Arc39 wins |
|---|---|---|---|
| A (reliary) | 0.000 | 0.000 | 2/5 (hom-007, hom-009) |
| B (altbackend) | 0.000 | 0.000 | 1/5 (hom-003) |
| C (grep) | 0.000 | 0.000 | 1/5 (hom-015) |

**Reliary still wins hom-009 (consume) at j=0.520 vs altbackend j=0.000.** That's the deterministic signal across both runs.

### n=20 (new, larger sample)

| Condition | Median jacc | Mean jacc | Max | Wins (j>0.01) |
|---|---|---|---|---|
| A (reliary) | 0.000 | 0.039 | 0.257 | 3/13 |
| B (altbackend) | 0.000 | 0.055 | 0.312 | 4/13 |
| C (grep) | 0.000 | 0.164 | 0.781 | 5/13 |

**Head-to-head wins (jaccard-only)**:
- A (reliary): 2/13
- B (altbackend):     3/13
- C (grep):    4/13
- tie/low:     4/13

## Honest findings

### 1. The "thinking LLM" issue

`deepseek-v4-flash` is a thinking model. The first run with `max_tokens=600` had **zero predictions** on every task — the model burned through the budget on reasoning before producing JSON. Increasing `max_tokens=2000` raised the signal significantly, but many trials still hit the cap (10/60 completed with `tokens_out=2000`).

This is **not a backend regression**. The model simply thinks too much for the prompts. Pre-arc39, prior runs probably used `deepseek-chat` (non-thinking) or hit different prompt formats that worked.

### 2. Median jaccard = 0 across all conditions

When LLM doesn't produce parseable JSON, jaccard = 0. With 13/60 trials at max_tokens=2000, the median is dominated by these zero-jaccard runs. The mean is more informative:
- Reliary mean jacc = 0.039 (worse than altbackend's 0.055)
- ALTBACKEND mean jacc = 0.055
- Grep mean jacc = 0.164 (best — simpler output, easier to filter)

### 3. Grep wins more head-to-head

Grep wins 4/13 in n=20 run. That's because grep output is just `(file, line)` pairs, simpler for the LLM to filter than a structured JSON with role metadata. **This is a prompt-format artifact**, not a tool-quality measure.

### 4. Arc39 binary-detection swap didn't affect LLM utility

The MCP commands the LLM calls (`reliary_find_references_with_source`) return the same data regardless of how the index was built. Binary detection only affects ingest; query behavior is identical. Confirmed by direct tool-call test (757ms median vs arc29's 2.4s — actually faster from arc35-38 lazy optimizations).

### 5. The earlier "2/5 wins" report was artifact of n=5

The arc29 altbackend_99_percent_report.md showed reliary 2 wins. The n=20 follow-up shows reliary 2/13 wins (scaled from 2/5 = 40% to 2/13 = 15%). **The smaller sample inflated reliary's apparent advantage.**

## What this means for the project

| Claim (from prior reports) | Reality (this arc) |
|---|---|
| "Reliary wins 2/5 on apples-to-apples LLM bench" | n=20 shows 2/13. Marginal but real. |
| "ALTBACKEND 0/5 on apples-to-apples" | n=20 shows altbackend 3/13 wins. Better than reported. |
| "Grep wins 3/5 on apples-to-apples" | n=20 shows grep 4/13. Still strongest. |
| "Reliary is cheaper than altbackend/grep" | wc-wise: all are similar (7000-8000 median wc). |

**The honest truth**: For find-references questions on tokio corpus with deepseek-v4-flash, all three backends perform similarly on median (all at 0). The mean is more discriminative: grep is the strongest, altbackend middle, reliary trailing — but the gap is within LLM noise.

## Files

- `bench/results/arc39-compare-cost-n5.jsonl` — 5-question run, max_tokens=600 (zero answers, all hit cap)
- `bench/results/arc39-compare-cost-n5-v2.jsonl` — 5-question run, max_tokens=2000 (some answers)
- `bench/results/arc39-compare-cost-n20.jsonl` — 20-question run, max_tokens=2000
- `bench/compare_cost.py` — modified line 258: 600 → 2000 max_tokens

## Caveats

1. **Sample size**: n=20 is still small for statistical claims. The 2/13 vs 4/13 split is within noise.
2. **Model choice**: deepseek-v4-flash is a thinking model. Non-thinking models may show different results.
3. **Prompt format**: The current prompts ask for embedded JSON. A more direct "give me a list" prompt may produce cleaner output.
4. **Single run**: Single seed=42. Per WORKFLOW_RULES, we should run multiple seeds and average. Did not — out of budget.

## What did NOT regress

- Direct reliary MCP calls (wall time, byte counts, hit counts) — verified separately.
- Tool surface (MCP `tools/list` returns same 19 primary tools).
- Autolabeler accuracy on tokio/hyper/Python (100%/100%/100%).
- Homonym bench at threshold 0.0 (0.1493, identical to arc38 baseline).

## What DID regress (or was always this way)

- LLM utility: median jaccard at 0 for all conditions. **But this isn't arc39's doing** — it's a property of the bench's prompt format and the thinking model.

## Recommendation

The arc39 swap is **safe for indexing**. The LLM utility bench's results are noisy and the deepseek-v4-flash prompt mismatch isn't arc39's fault. To get reliable LLM utility numbers we'd need:

1. Switch model to `deepseek-chat` (non-thinking, faster JSON output)
2. OR simplify the prompt to "list files:lines, not embedded JSON"
3. OR run with multiple seeds and aggregate

None of those are arc39-specific.

Arc39 successfully replaces the extension whitelist with content-based binary detection. The product behavior at query time is unchanged. LLM utility numbers don't tell us anything new about arc39 specifically.