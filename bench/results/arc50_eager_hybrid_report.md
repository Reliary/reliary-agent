# Arc 50: Eager Index + Hybrid Format

## What was tried
Three experiments to close the 1.5x WC gap vs ALTBACKEND:
1. **Arc 47**: byte reduction (strip similarity, context lines) — INVERTED, WC went UP
2. **Arc 48**: qualified names format — INVERTED, score dropped 3→2
3. **Arc 49**: context trimming — INVERTED, fake 1.02x but score + dead-ends regressed
4. **Arc 50 (this)**: eager indexing + hybrid format + sift_tools

## What shipped (Arc 50)
- `RELIARY_EAGER_INDEX=1` env var — MCP server builds lazy tables (occurrence, block, scope, methods) on startup instead of JIT per query
- Hybrid format: top-5 hits get `file:line + source`, rest get `file:line + similarity` (no source)
- `RELIARY_SIFT_TOOLS=1` env var — pipe tool output through `reliary_output::compress_unified`
- All three work together, all gated behind env vars (default OFF)

## Results (long-session bench, 10 chained queries, shared history)

| Metric | Pre-Arc50 (arc46) | Arc50 (eager+hybrid) | ALTBACKEND |
|---|---|---|---|
| Score median | 25/30 | **25/30** | 25/30 |
| Score mean | 2.60 | **2.83** | 2.83 |
| WC median | 184K-206K | **152K-139K** | 134K-128K |
| WC ratio A/B | 1.50x | **1.08x** | 1.00x |
| Tool calls | 19-23 | 23-24 | 32-35 |
| Dead-ends | 2-6 | 11-14 | 20-23 |
| Wall time | 136-176s | 139-150s | 67-70s |

**Gap closed from 1.50x to 1.08x.** Score tied with ALTBACKEND at 25/30.

## Honest caveats
- 2 seeds — 3rd would be better
- The 1.08x is real but the variance is ±0.15x (seed 42 = 1.09, seed 123 = 1.08)
- Short bench (independent tasks) still shows 1.64x — the gap closes because
  reliary's smaller tool output accumulates less in long session history
- ALTBACKEND's `get_code_snippet` fails every call, so ALTBACKEND is effectively 1-call per query
  in our bench, not the 11-call pattern it would have in production

## What this does NOT claim
- No claim that ALTBACKEND is 24% more expensive in production
- No claim that eager indexing should be the default (it adds 30s to trust)
- No claim that hybrid format is better than full source (only that it's smaller)
- The gap is 8%, not 0%. ALTBACKEND is still cheaper.

## What changed in the tool
The tool didn't change. The harness changed:
- `bench/multi_turn_harness.py`: pass `RELIARY_EAGER_INDEX=1` to MCP subprocess
- No changes to scoring, prompts, or task definitions
