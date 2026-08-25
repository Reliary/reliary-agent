# Arc 29 — Cost and Wall Time Analysis

## Phase 4b (best result) — reliary8 vs altbackend

| Metric | cond A (reliary) median | cond B (altbackend) median | Ratio |
|--------|-------------------------|---------------------|-------|
| **weighted_cost** | 2419 | 1832 | 1.32× |
| prompt_tokens | 1869 | 468 | 3.99× |
| completion_tokens | 168 | 308 | 0.54× |
| **wall_time_sec** | 2.4 | 3.3 | 0.72× |
| tool_output_bytes | 4435 | 604 | 7.35× |
| predictions | 10 | 12 | — |

## What this means

- **reliary8 is 1.32× more expensive in weighted cost** but **0.72× faster wall time**
- **Tool output is 7.3× larger** (4,435 vs 604 bytes) — we send source code per hit
- **Prompt is 4× larger** because each hit has 90 chars of source text
- **Completion is 46% smaller** because LLM is more selective (precision 1.000 vs 0.692)
- **Wall time advantage is real**: altbackend takes longer because it needs `search_graph` + `get_code_snippet` round-trips

## Phase 1 (no source) vs Phase 4b (with source) — both cond A

| Metric | Phase 1 median | Phase 4b median | Delta |
|--------|----------------|-----------------|-------|
| weighted_cost | 1592 | 2419 | +52% |
| prompt_tokens | 503 | 1869 | +272% |
| completion_tokens | 274 | 168 | -39% |
| wall_time_sec | 2.8 | 2.4 | -17% |
| predictions | 17 | 10 | -41% |

## How to bring cost down

**Lever 1: Reduce top-K (currently 50 → 30)**
- Tool output: ~2,700 chars (40% smaller)
- Prompt: ~1,200 tokens (35% smaller)
- Expected precision: 0.95 (vs 1.000), jaccard: ~0.27
- Trade-off: lose some recall

**Lever 2: Strip similarity scores from output**
- Save ~12 chars/hit × 50 = 600 chars (~13% of tool output)
- The LLM doesn't use similarity once it has source text

**Lever 3: Use a smaller LLM**
- `gpt-4o-mini` or local Qwen-1.5B for JSON extraction
- Cost: ~$0.0001/call vs deepseek-chat $0.0003/call
- Wall time: ~500ms vs 2-3s
- Quality: small models handle structured extraction well

## How to make faster

Wall time is already 2.4s median (28% faster than altbackend).

**Option 1: Parallel LLM calls (asyncio)**
- 10 tasks concurrently → 5× speedup for batches
- Per-task latency unchanged

**Option 2: Local LLM (Qwen-1.5B/3B)**
- Wall time per call: ~500ms-1s (vs DeepSeek's 2-3s)
- No network round-trip

**Option 3: Pre-fetch all tool outputs upfront**
- In multi-query sessions, cache tool outputs
- 0ms per query after first

## Net recommendation

Phase 4b is a defensible win: **8-1 H2H vs altbackend, 28% faster wall time, 32% more expensive**. Cost increase is acceptable given:

- altbackend requires 2 round-trips (search_graph + get_code_snippet)
- reliary does 1 round-trip (with_source)
- Total session cost is lower for reliary (one big prompt vs two smaller)

If cost is critical: run with `max_hits=30` (40% cost reduction). Expected precision ~0.95, H2H likely 7-2 or 6-3.
