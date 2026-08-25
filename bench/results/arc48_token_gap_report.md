# Arc 48 — Token Gap Hypothesis Test

## The gap

ALTBACKEND WC=1,832 vs Reliary WC=2,419 (arc42 baseline). 24% gap.

## Hypothesis

ALTBACKEND returns qualified names (`tokio::io::util::buf_writer::BufWriter::consume`) which the LLM trusts from training data. Reliary returns source code (`file:line: fn consume(...)`) which the LLM verifies with reasoning tokens. Completion tokens (4x cost) dominate the gap.

## Tests run

### Test 1: Byte reduction (arc47) — FAIL

Stripped similarity scores, context lines, reduced top-K 50→20.
Tool output: 4,435 → 2,013 bytes (55% smaller).

Result: WC INCREASED 31% (6,500 → 8,487). Leaner output = more completion tokens.
The LLM compensates for less data by generating longer reasoning.

### Test 2: Qualified names (arc48) — PARTIAL

Constructed qualified names from grammar-free index (file path → module path, brace-graph → impl target, source line → method name).

Tool output: 4,435 → 800 bytes (82% smaller).

| Metric | grep format (baseline) | qualified format | Delta |
|--------|------------------------|-----------------|-------|
| Score median | 3.0 | 2.0 | -1.0 |
| Score mean | 2.47 | 2.33 | -0.14 |
| WC median | 8,661 | 6,732 | -22% |

WC dropped 22% but score dropped from 3.0 to 2.0. The LLM can't distinguish definitions from call sites without source code.

## Root cause

The 24% gap is the **structural cost of providing source code inline**. ALTBACKEND is cheaper because it doesn't include source — the LLM must call `get_code_snippet` separately. But ALTBACKEND's `get_code_snippet` fails every call in our bench, so ALTBACKEND's cost advantage is partially from broken tool calls (the LLM gives up on verification and answers from names alone).

The gap cannot be closed without trading answer quality for token cost. Source code inline is what makes reliary's answers better (precision 1.000 in arc29) — removing it makes the tool cheaper but worse.

## Conclusion

Accept the honest tie. Reliary trades ~24% more tokens for:
- Source code inline (no follow-up calls needed)
- Grammar-free universality (any language, any file)
- One-call UX (1 call vs ALTBACKEND's 11)
- mAP 1.000 on find-references (tokio + hyper cross-corpus)

The 24% gap is the cost of being grammar-free. Not closable without losing the features that make reliary different from ALTBACKEND.