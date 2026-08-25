# Arc 44 — Track B+C Results

## Track B: Tool fixes (~120 LOC)

### B1: methods_on returns related_types
- **Tool**: `methods_on('SemaphorePermit')` now returns `related_types: ["OwnedSemaphorePermit"]`
- **Result**: Tool improvement shipped, but LLM doesn't mention OwnedSemaphorePermit in its answer
- **Pass gate**: ✓ Tool returns related types

### B2: methods_on prefers local impl over trait
- **Skipped**: The LLM's issue was answer quality (summarizing instead of listing), not tool data
- **Verdict**: Remaining failures are LLM answer quality, not tool gaps

## Track C: Unique capability benchmarks (~200 LOC)

### C1: IR Reasoning Compression
- **Condition ON**: WC=4832 (median)
- **Condition OFF**: WC=3241 (median)
- **compressed_count**: 0 (LLM never called reliary_compress)
- **Pass gate**: ✗ WC REDUCTION -41.1% (LLM ignored instruction)
- **Meta-finding**: LLMs don't self-compress even when told to. The -43% to -77% IR compression requires proxy-level integration, not voluntary tool use.

### C2: Cross-language grammar-free indexing
- **Test**: Index Python (def foo()), Rust (fn foo() {}), JS (function foo())
- **Result**: `reliary search foo` returns hits in all 3 languages
- **Pass gate**: ✓ PASS — grammar-free indexing works across languages

### C3: Round-trip cost (reliary vs altbackend)
- **Reliary**: 1 call, 0.0s, 2506 bytes
- **ALTBACKEND**: 11 calls, 0.1s, 12384 bytes
- **Pass gate**: ✓ BOTH PASS — 11x fewer round-trips than altbackend

## Summary

| Track | Status | Pass gate |
|-------|--------|-----------|
| B1 (related_types) | Shipped | ✓ |
| B2 (local impl) | Skipped | n/a |
| C1 (compression) | LLM doesn't use | ✗ |
| C2 (cross-lang) | Works | ✓ |
| C3 (one-call) | 11x fewer round-trips | ✓ |

**Key insight**: The IR compression value (-43% to -77% WC) is real but requires proxy integration — LLMs won't voluntarily call a compress tool. Reliary's unique wins are: universal grammar-free indexing and one-call round-trip efficiency.
