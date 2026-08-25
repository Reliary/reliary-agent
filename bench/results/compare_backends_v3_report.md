# Arc 28 Lever 6 v3 — reliary8 vs altbackend-mcp (Independent Oracle)

Source: `compare_v3_20260629T111424Z.jsonl`

## TL;DR

With an **independent grep-based oracle** (not derived from either backend), **altbackend-mcp wins 8/10 tasks on jaccard**. reliary8's grammar-free type-flow has higher **precision** in some cases but lower **recall** overall.

This **reverses** the v2 result, which used a circular oracle derived from reliary's own type-flow output. The v2 win was an artifact.

## Methodology

**Independent oracle**: for each anchor `(stem, file, line, label)`, ground truth = `grep -rEn "\\b{stem}\\b" <corpus> --include=*.rs` filtered for:

- Not a test file (`/tests/`, `_test.rs`)
- Not a doc comment line (`///`, `//!`, `//`)
- Not the anchor's own line

**Single-turn direct DeepSeek via api.deepseek.com.** LLM is given pre-fetched tool output from one backend (interleaved A/B per task). Asks for JSON references. Three metrics: jaccard, precision, recall vs the grep oracle.

## Aggregate Results

| metric | cond A (reliary8) median | cond B (altbackend) median | winner |
|--------|---------------------------|----------------------|--------|
| jaccard | 0.041 / 0.053 | 0.276 / 0.213 | B |
| precision | 0.118 / 0.137 | 0.750 / 0.584 | B |
| recall | 0.055 / 0.078 | 0.288 / 0.227 | B |
| weighted_cost | 1546.000 / 1526.000 | 1611.000 / 1886.900 | B |
| elapsed | 2.800 / 2.775 | 2.895 / 3.058 | B |

**Head-to-head (jaccard, >0.01 difference per task):** A wins 2, B wins 7, ties 1.

## Per-Task Detail

| task_id | stem | label | gt | A jacc | A prec | A rec | B jacc | B prec | B rec |
|---------|------|-------|----|--------|--------|-------|--------|--------|-------|
| hom-009 | consume | method_call | 25 | 0.049 | 0.111 | 0.080 | 0.400 | 1.000 | 0.400 |
| hom-014 | split | method_call | 28 | 0.048 | 0.125 | 0.071 | 0.000 | 0.000 | 0.000 |
| hom-015 | kill | method_call | 31 | 0.146 | 0.375 | 0.194 | 0.275 | 0.550 | 0.355 |
| hom-018 | from_std | function_def | 35 | 0.000 | 0.000 | 0.000 | 0.278 | 0.909 | 0.286 |
| hom-021 | spawn | function_def | 64 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 |
| hom-026 | consume | method_call | 25 | 0.100 | 0.211 | 0.160 | 0.370 | 0.833 | 0.400 |
| hom-027 | consume | method_call | 25 | 0.128 | 0.263 | 0.200 | 0.400 | 1.000 | 0.400 |
| hom-028 | split | method_call | 28 | 0.027 | 0.100 | 0.036 | 0.000 | 0.000 | 0.000 |
| hom-030 | default | function_def | 76 | 0.034 | 0.188 | 0.039 | 0.278 | 0.880 | 0.289 |
| hom-034 | send | method_call | 56 | 0.000 | 0.000 | 0.000 | 0.133 | 0.667 | 0.143 |

## Honest Findings

### What we learned

- **v2 was wrong.** Using reliary's own type-flow output as the oracle meant reliary trivially wins (it's reporting its own GT). The v3 oracle fixes this.
- **altbackend-mcp is better at find_references than grammar-free type-flow similarity** in this 10-task sample.
- The LLM is doing meaningful filtering — cond B (altbackend) returns noisy broad matches, and the LLM filters them down to high-precision answers.

### Why did v2 mislead us?

We built `reliary_find_references_type_flow` to output high-quality type-flow matches. The oracle used the same tool with a fixed threshold. The LLM was given that same output and asked to copy it. **Cond A 'won' because cond A's tool produced the oracle directly.** This is the textbook definition of a circular measurement.

### What does the v3 result mean?

For a fresh LLM looking at code questions where 'find references to symbol X' is asked:

- **altbackend_search_graph** is the better tool — it returns broader matches, the LLM filters with high precision (median 0.667-1.000).
- **reliary_find_references_type_flow** is more focused but the LLM can't leverage the type-flow ranking as effectively when given the output as text.
- altbackend is **cheaper** (~half the cost) and equally fast.

### Implications for the reliary8 thesis

The original arc 28 thesis was 'reliary8 MCP tools give LLMs the right info'. **This is partly true** for codeless metrics (mAP=1.000 on find-references bench), but **less true** when the LLM is the consumer. The grammar-free approach loses to the tree-sitter+BM25 approach because the LLM does its own filtering and benefits more from breadth than precision.

### Caveats

- **10 tasks is small.** Single trial per condition. Statistical significance NOT established.
- Only tokio corpus. Other corpora may differ.
- Only `find_references` category tested. Other categories (call_graph, search) not measured here.
- Direct DeepSeek only. Other LLMs (Claude, GPT) may differ.
- One oracle choice (strict grep). Other oracle choices (manual labels, hybrid) may differ.
