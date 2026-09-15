# FIX PLAN V17 — Pure Math Performance Optimizations

## Goal
Apply pure-math and data-structure tricks to eliminate remaining overhead in the
hottest code paths. No algorithmic changes — same logic, faster execution.

## Items

### M1: Bitmask delimiter positions (≤64-byte lines)
- Replace `Option<usize>` fields in `LineDelimiters` with `u64` bitmasks
- `first_paren = paren_mask.trailing_zeros()` (1 CPU instruction)
- `last_paren = 63 - paren_mask.leading_zeros()` (1 CPU instruction)
- Struct: 112 bytes → 32 bytes (fits in one cache line)
- Fallback to `Option<usize>` for lines >64 bytes (rare)
- **Files:** structural.rs
- **Effort:** 2h

### M2: memchr string-skipping (SIMD-accelerated)
- In `scan_delimiters`, replace byte-by-byte string-body loop with `memchr::memchr(b'"', &bytes[i..])`
- Jumps directly to next quote using SSE2/AVX2/NEON
- 10-100× faster on string-heavy lines (doc comments, string constants)
- **Files:** structural.rs (scan_delimiters)
- **Effort:** 30min

### M3: Direct-index Vec for phrase_id lookups
- Replace `FxHashMap<i64, T>` with `Vec<T>` indexed by phrase_id (1..=max_phrase_id)
- Zero hash computation, zero collisions, O(1) direct index
- Requires `SELECT MAX(id) FROM phrases` once at startup
- **Files:** search.rs, lazy_occurrence.rs, callgraph_v2.rs
- **Effort:** 2h

### M4: Prefix tree (trie) for keyword detection
- Replace 8+ sequential `starts_with()` checks with single trie lookup
- Eliminates branch mispredictions in control-flow detection
- **Files:** structural.rs (classify_structural lines 50-57)
- **Effort:** 1h
- **Priority:** LOW (existing checks are already fast for short lines)

### M5: u8 tag lookup table (branch-free tag classification)
- Replace `match delim_char { b'(' => 1, b'{' => 2, ... }` with `TAG_TABLE[delim_char]`
- 256-entry `static` array, single table lookup, zero branches
- **Files:** structural.rs (classify_structural tag classification)
- **Effort:** 15min

### M6: Batch SQLite inserts (10-50× on bulk operations)
- Wrap `build_all_occurrence` INSERTs in transaction with periodic commits
- Chunk size 1000 to balance WAL size vs transaction overhead
- **Files:** lazy_occurrence.rs (build_all_occurrence), reindex.rs
- **Effort:** 1h

### M7: SmallVec for hot-path Vec allocations
- Add `smallvec` crate dependency
- Replace `Vec<T>` with `SmallVec<[T; 8]>` for callees/callers/hits (usually <8 elements)
- Eliminates 90%+ of heap allocations in callgraph/type_flow hot paths
- **Files:** callgraph_v2.rs, type_flow.rs, symbol.rs
- **Effort:** 1h

## Execution order
1. M5 (tag table — 15min, zero risk)
2. M2 (memchr — 30min, highest ROI)
3. M6 (batch inserts — 1h, big JIT speedup)
4. M7 (SmallVec — 1h, reduces allocator pressure)
5. M3 (direct-index — 2h, biggest lookup speedup)
6. M1 (bitmask — 2h, cache-line optimization)
7. M4 (trie — 1h, lowest priority)

## Verification
- All 115+50+6 tests pass after each item
- Long bench (seeds 42, 17) — no score regression
- Wall time comparison: V16 baseline vs V17 after each phase