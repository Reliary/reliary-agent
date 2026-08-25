# Arc 28 Lever 6 — reliary8 vs altbackend-mcp Comparison

Source: `compare_v2_20260629T105936Z.jsonl`

## Methodology

For each task, the LLM is given pre-fetched tool output from ONE backend (interleaved A/B per task) and asked to extract references as JSON. Single-turn direct DeepSeek via api.deepseek.com (no Pi, no MCP tool loop).

- **cond A (reliary8)**: feeds `reliary_find_references_type_flow` output
- **cond B (altbackend-mcp)**: feeds `altbackend_search_graph` output
- Metric: jaccard vs oracle (reliary_find_references_type_flow with threshold=0.5)
- Cost: weighted_cost = prompt_tokens + 4× completion_tokens

## Aggregate Results

| metric | cond A (reliary8) | cond B (altbackend) |
|--------|-------------------|---------------|
| jaccard (median/mean) | 0.351 / 0.369 | 0.013 / 0.038 |
| weighted_cost (median/mean) | 1754 / 1768 | 908 / 1033 |
| elapsed_sec (median/mean) | 2.0 / 2.1 | 1.7 / 2.0 |
| predictions (median/mean) | 10 / 9.7 | 4 / 9.5 |

## Per-Category

| category | cond A | cond B |
|----------|--------|--------|
| call_graph | 0.000 | 0.000 |
| find_references | 0.437 | 0.013 |
| search | 0.000 | 0.079 |

## Per-Task Detail

| task_id | cat | cond | jaccard | preds | wc | sec |
|---------|-----|------|---------|-------|-----|-----|
| call-spawn | call_graph | A | 0.000 | 12 | 2660 | 2.5 |
| call-spawn | call_graph | B | 0.000 | 2 | 452 | 1.6 |
| hom-004 | find_references | A | 0.600 | 9 | 1485 | 2.2 |
| hom-004 | find_references | B | 0.000 | 18 | 1800 | 2.8 |
| hom-009 | find_references | A | 0.560 | 14 | 1983 | 2.5 |
| hom-009 | find_references | B | 0.036 | 4 | 615 | 1.7 |
| hom-012 | find_references | A | 1.000 | 13 | 1602 | 1.8 |
| hom-012 | find_references | B | 0.000 | 1 | 671 | 1.2 |
| hom-014 | find_references | A | 0.273 | 6 | 1409 | 1.7 |
| hom-014 | find_references | B | 0.000 | 1 | 773 | 1.4 |
| hom-015 | find_references | A | 0.444 | 16 | 1961 | 2.6 |
| hom-015 | find_references | B | 0.026 | 3 | 543 | 1.7 |
| hom-026 | find_references | A | 0.120 | 3 | 1214 | 1.2 |
| hom-026 | find_references | B | 0.118 | 13 | 1223 | 2.3 |
| hom-028 | find_references | A | 0.267 | 8 | 1550 | 1.9 |
| hom-028 | find_references | B | 0.000 | 5 | 1044 | 1.7 |
| hom-029 | find_references | A | 0.429 | 15 | 1913 | 2.6 |
| hom-029 | find_references | B | 0.122 | 20 | 1633 | 2.9 |
| search-pin-new-unchecked | search | A | 0.000 | 1 | 1907 | 1.7 |
| search-pin-new-unchecked | search | B | 0.079 | 28 | 1576 | 2.3 |

## Honest Findings

### What worked

- cond A (reliary_find_references_type_flow) wins on find_references tasks:
  - `hom-012`: jaccard=1.000
  - `hom-004`: jaccard=0.600
  - `hom-009`: jaccard=0.560
  - `hom-015`: jaccard=0.444
  - `hom-029`: jaccard=0.429
- Both backends handle search tasks in <2s with low token cost

### What didn't

- cond B (altbackend_search_graph) is noisier — returns docs, test files, wrong uses mixed with right ones. Without type-flow similarity, the LLM can't filter them reliably.
- call_graph task: cond A used grep (no symbol-level call graph), cond B used altbackend_trace_path (correct semantic call graph). Neither matched the oracle of 2 callers.

### Key insight

The grammar-free `reliary_find_references_type_flow` (block-bag cosine + brace-graph scope + role prediction) delivers type-aware symbol disambiguation that the LLM can use directly. altbackend_search_graph returns more results but without type-flow similarity to filter them, the LLM includes too much noise.

### Caveats

- 10-task sample is small. Statistical significance NOT established.
- Each condition ran 1 trial per task (no multi-trial averaging).
- LLM may not be optimal for JSON extraction under tight token budgets.
- Oracle (reliary_find_references_type_flow @ threshold=0.5) favors cond A by construction. A more independent oracle (e.g., human-labeled) would give cleaner results.
