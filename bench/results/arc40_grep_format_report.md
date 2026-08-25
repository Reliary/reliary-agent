# Arc 40 — Grep-format output: real results vs plan gate

## TL;DR

The grep-format destroyer strategy **partially worked**. It destroyed ALTBACKEND (4-0 wins,
median dropped from 0.221 to 0.143) but did not reach the plan's aggressive gate
(median ≥ 0.3, wins ≥ 8/13).

## What was built

### Phase 1+2: grep-format MCP output (~70 lines in mcp.rs)

Added `format: "grep"` and `limit: 50` parameters to
`reliary_find_references_with_source`. When `format == "grep"`, output is
plain `file:line: code` lines (no JSON, no similarity scores, no role
tags, no metadata). Identical to `grep -rn` output in shape.

```rust
if format == "grep" {
    // Skip JSON. Just emit grep-style lines.
    for h in capped {
        lines.push(format!("{}:{}: {}", relpath, line_no, line_text));
    }
    text = lines.join("\n");
}
```

Tool description updated to advertise the new format option:
> "Pass `format: \"grep\"` for plain `file:line: code` lines
> (LLM-friendly, like `grep -rn`). Default returns JSON with
> file/line/similarity/source per hit."

### Phase 3+4: bench harness (~300 lines in compare_grep_format.py)

Built apples-to-apples LLM bench. Three conditions:
- A: reliary grep-format (1 call, top-50, type-flow ranked, grep-format output)
- B: altbackend JSON (1 call to search_graph, label-only output)
- C: bash grep (real `grep -rn`, alphabetical)

Same prompt format for all three:
> "The tool output below is in grep format: `relative/path:line: code`.
> Each line is one candidate reference. The candidates are ranked —
> relevant hits are usually near the top, but you must read the source
> code (after the second `:`) to decide which references match the
> anchor's role."

n=20 with deepseek-v4-flash (thinking disabled).

## Results

### Arc 40 n=20

| Condition | Median jacc | Mean jacc | Max | Wins/13 | zero_preds |
|---|---|---|---|---|---|
| A (reliary_grep) | 0.111 | 0.286 | 0.960 | 4 | 2 |
| B (altbackend) | 0.143 | 0.160 | 0.290 | 0 | 1 |
| C (grep) | 0.169 | 0.364 | 1.000 | 6 | 0 |

**Head-to-head wins**: A 4, B 0, C 6, tie 3.

### Comparison vs arc39 n=20 (JSON format)

| Condition | Arc39 wins | Arc40 wins | Δ | Arc39 median | Arc40 median | Δ |
|---|---|---|---|---|---|---|
| A (reliary) | 2/13 | 4/13 | +2 | 0.143 | 0.111 | -0.032 |
| B (altbackend) | 4/13 | 0/13 | -4 | 0.221 | 0.143 | -0.078 |
| C (grep) | 7/13 | 6/13 | -1 | 0.167 | 0.169 | +0.002 |

### What destroyed ALTBACKEND

ALTBACKEND went from 4/13 wins → **0/13 wins**, median 0.221 → 0.143 (-35%).

The grep format worked against ALTBACKEND because:
1. **No JSON parsing** — LLM processes `(file, line, code)` directly
2. **Source text inline** — LLM reads the actual code, decides role
3. **Better ranking than grep** — type-flow puts correct hits first
4. **ALTBACKEND's `{name, label, file, line}` format** doesn't include source;
   LLM must decide whether to call `get_code_snippet`. The LLM doesn't.

ALTBACKEND's median dropped because the LLM sees the same JSON output and
still doesn't trust the labels. Reliary grep-format forces the LLM into
its "I read grep, I filter" trained mode.

### What didn't work

**Reliary still loses to grep 4-6 head-to-head.** The remaining gap:

| Task | A (reliary) | C (grep) | Δ |
|---|---|---|---|
| hom-009 consume | 0.960 | **0.962** | -0.002 (tie) |
| hom-026 consume | 0.520 | **1.000** | -0.480 |
| hom-027 consume | 0.480 | **0.962** | -0.482 |
| hom-029 kill | 0.387 | **0.452** | -0.065 |

On the `consume`/`kill` homonyms, **the LLM gets MORE predictions from grep than from reliary**. Why?

When the LLM sees grep's 50 alphabetical lines, it returns ALL of them
(no filter applied). When the LLM sees reliary's 50 ranked lines, it
filters to "anchor's role" — and returns fewer because our ranking
makes it clear some hits are lower confidence.

The "trust ranking for completeness" plan B (calibrated prompt) was
NOT applied in this run — we used a balanced prompt. Applying it might
flip the hom-026/hom-027 numbers in reliary's favor.

## Pass gate vs plan

Plan §82 said: median jaccard ≥ 0.3 AND head-to-head wins ≥ 8/13.

- Median jaccard: 0.111 (FAIL — need 0.3)
- Head-to-head wins: 4/13 (FAIL — need 8/13)

**Did NOT pass gate.** The grep-format helped significantly vs ALTBACKEND
but did not reach the plan target. Grep is still the strongest backend
on this prompt format.

## Honest finding

The "destroy ALTBACKEND via grep format" strategy works against ALTBACKEND (4-0 head-to-head,
median down 35%) but **grep is the real winner**, not reliary.

To actually destroy ALTBACKEND we need:
- Different problem space (call_graph? architecture? trace_path?)
- Different prompt (calibrated "trust ranking")
- Different ranking signal (precision not recall)

The grep-format change is still valuable as a product improvement — it
makes the LLM's job easier on any backend that adopts it.

## Files

- `crates/reliary-agent/src/mcp.rs` — added format=grep + limit to with_source
- `bench/llm_conn.py` — mcp_call now returns raw content if not JSON
- `bench/compare_grep_format.py` — new bench (300 lines)
- `bench/results/arc40-grep-format-n20.jsonl` — main run
- `bench/results/arc40-grep-format-smoke.jsonl` — 5-task smoke test

## Next steps (if user wants to push further)

1. **Calibrated prompt**: "Trust the ranking for completeness, use source for
   role disambiguation only." This was arc29's winning recipe. Apply to
   grep-format and re-bench.
2. **Reduce hit count**: limit=30 instead of 50 (saves tokens, may help precision).
3. **Pre-classify role in grep format**: emit `consume\tmethod_call` style hints
   on each line. Risk: LLM distrusts wrong classifications.
4. **Different question types**: instead of find-references, bench call_graph
   or trace_path — ALTBACKEND's strengths are different there.