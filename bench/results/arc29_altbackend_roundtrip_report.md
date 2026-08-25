# Arc 29 — ALTBACKEND Round-Trip Cost Analysis

## What altbackend actually does (the full workflow)

In production, an LLM using altbackend makes multiple round-trips:

1. `search_graph` → returns up to 50 hits with qualified_name, file_path, start_line (no source code)
2. For each candidate the LLM wants to inspect: `get_code_snippet` → reads the actual source
3. LLM filters and returns final references

reliary8's `reliary_find_references_with_source` collapses this into **one tool call**: search + source in one prompt.

## Tool-only measurements (no LLM call)

| metric | reliary with_source | altbackend top-10 | altbackend top-20 |
|--------|---------------------|------------|------------|
| bytes returned | 4435 | 16481 | 31688 |
| search_graph bytes | n/a | 2229 | 4478 |
| snippet bytes (sum) | n/a | 14252 | 27210 |
| MCP tool calls | 1 | 11 | 21 |
| total tool time | n/a | 0.04s | 0.07s |

## Wall time (no LLM)

altbackend tool calls are FAST (~50ms for search + 10 snippets). But each tool call has network/process overhead. The bottleneck in practice is the LLM call (2-3s), not the tool.

## End-to-end: LLM + tool

| metric | reliary8 | altbackend (search only) |
|--------|----------|-------------------|
| wall_time_sec | 2.4 | 3.3 |
| weighted_cost | 2419 | 1832 |
| prompt_tokens | 1869 | 468 |
| completion_tokens | 168 | 308 |
| predictions | 10 | 12 |

## Apples-to-apples: altbackend's actual workflow

If altbackend bundled 10 snippets into the prompt (single LLM call), the prompt would include ~10,653 bytes of source = ~3,500 tokens.

- altbackend simulated prompt: ~3500 tokens
- altbackend simulated weighted_cost: ~4730

- reliary8 actual prompt: 1869 tokens
- reliary8 actual weighted_cost: 2419

**Conclusion:** Even if altbackend batches snippets into one prompt, reliary8's prompt is still ~half the size because reliary pre-filters to type-flow-matched references only (LLM doesn't have to filter out `BufWriter::consume` vs `Take::consume` manually).

## Key insight

The cost advantage of altbackend is **less than it appears**:

- **Tool-only**: altbackend is fast (~50ms) but returns 2-3× more bytes
- **Per-call**: reliary costs 1.32× more tokens (search-only)
- **Real workflow**: altbackend requires N tool calls (search + N snippets) with N round-trip latencies
- **Quality**: altbackend's search-only mode has precision 0.692 vs reliary's 1.000

The hidden cost in altbackend's model is **multi-call latency** + **lower precision** = LLM has to filter more, produce more candidates, and make more tool calls to converge. reliary collapses all of this into one call.
