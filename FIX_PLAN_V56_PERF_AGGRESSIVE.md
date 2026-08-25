# FIX_PLAN_V56 — Aggressive Performance Push

Branch: `feat/colbert-late-interaction` (V55 late-interaction is in the working tree,
uncommitted; V56 stacks on top).

## Context

Current V54 state (reliary self-bench, 4 seeds):
- Score 25.0 ± 0.0, Billed ~20k, Wall ~38s, Tool bytes ~12.5k
- Per-call latency ~0.4ms (warm connection, V54 reuse)
- Known: 54% of WC is tool-result compounding across turns; models re-query
  the same symbol 2-4× per session

## Goals (all cache-safe, deterministic, grammar-free)

1. Reduce WC -15-30% via session-level tool-result cache with compact repeats
2. Reduce search latency 10-30× by moving phrase lookups out of SQLite
3. Reduce wall -20-35% via fewer redundant calls + parallel dispatch
4. Preserve score 25.0 (no accuracy regression — verify with bench)

## Phase 1 — Session-level tool-result cache (HIGHEST LEVERAGE)

Files: `crates/reliary-agent/src/mcp.rs`

### 1a. Result cache
- Add `thread_local!` cache: `FxHashMap<u64, String>` keyed by
  `fxhash(tool_name ++ canonical_json(args))`
  (args sorted keys → deterministic serialization).
- Wrap `dispatch_tool_call`: before dispatch, hash key → on hit, return the
  cached serialized text (full `DispatchResult::Success` JSON).
- Store the result text on first computation.
- Cap at 256 entries; on overflow clear half (simple, no LRU needed at this scale).
- Determinism requirement already proven (V26: same query + index = byte-identical output).

### 1b. Compact repeat responses
- On a cache hit, return ONLY the first line of the cached text (the
  "answer line" — every tool's V40 one-line format starts with the answer),
  prefixed with `cached:`.
- Rationale: the model asked the same question again; re-sending the full
  result re-bills every token. One line re-establishes the answer.
- Cache-safety: modifies only the FUTURE result for the repeat call; the
  first result (already in context) is untouched. KV cache intact.
- Risk: model expects identical output for identical call → one-line reply
  is strictly smaller and carries the same answer; judge impact neutral
  (verified by bench in Phase 6).

### 1c. Single-flight
- Model may emit N tool_calls in one message. Before executing each, check
  the in-message hash set; identical concurrent calls share one execution,
  both receive the same result.

### 1d. Parallel dispatch (cheap win)
- In `tools/call` handler, execute the tool_calls of one message via
  `std::thread::scope` (reads are independent; SQLite connection is
  per-call via the thread-local cache → each thread gets its own conn).
- Sequential fallback when `RELIARY_PARALLEL=0`.
- Expected: saves ~N-1 × latency per multi-call message (LLM emit latency
  dominates, but this is free).

## Phase 2 — In-memory phrase index

Files: `crates/reliary-search/src/schema.rs` (or new `phrase_index.rs`),
`search.rs`, `mcp.rs` (`closest_symbols`)

### 2a. Load once at connection open
- `PhraseIndex { phrases: Vec<Box<str>>, sorted: Vec<u32> /* indices sorted */ }`
- Built from `SELECT id, phrase FROM phrases ORDER BY id` (~14K rows, <1MB).
- Stored in the thread-local connection cache (V54) alongside the Connection.
- Rebuild on reindex (watcher/trust invalidates).

### 2b. Rust-side search (SWAR + memchr)
- `find_prefix(term) -> Vec<&str>`: binary search on sorted phrase list,
  memchr on common prefix bytes.
- `find_substring(term)`: SWAR byte-window scan over phrases whose length
  allows the substring (skip short phrases via length check first).
- `find_closest(term)`: edit-distance ≤2 filter over candidates from
  prefix+substring results (bounded at ~200 candidates).
- Replace in:
  - `search_fts5` like_terms phase (currently `LIKE '%term%'` per term)
  - `closest_symbols` (currently 5 `LIKE 'x%' ORDER BY LENGTH` scans)
  - V55 `late_interaction_rerank` phrase scan (biggest cost: per-term LIKE)
- Expected: search call 1-3ms → <0.1ms; zero SQLite LIKE scans.

### 2c. Keep SQLite as source of truth
- The in-memory index is a read accelerator only. All writes go through the
  existing ingest/JIT paths. If the index is stale (file mtime newer than
  load time), fall back to SQL (correctness over speed).

## Phase 3 — Micro-latency

### 3a. Qualified-name derivation once per file
- `hit_with_qualified_name` derives per hit; group hits by file_path in the
  handler, derive once, clone the derived qname per hit.
- Files: `mcp.rs` (find_references + with_source paths).

### 3b. CPU-aware rayon sizing
- `main.rs`: `let threads = if cfg!(target_arch = "aarch64") { 4 } else { available_parallelism().min(8) };`
  honor `RELIARY_RAYON_THREADS` override (keep existing env support).

### 3c. describe default cap
- `mcp.rs` describe/pack_query output cap 5000 → 1500 chars.

## Phase 4 — V55 late-interaction integration

- Gate the V55 rerank behind the in-memory index (Phase 2b) so its phrase
  scan is free.
- Add `RELIARY_LATE=0` env kill-switch for bench A/B testing.
- Commit V55 code first (separate commit), then V56.

## Phase 5 — Tests

- Unit: result-cache hit/miss, compact-repeat one-line, single-flight dedupe,
  phrase index prefix/substring/closest correctness vs SQL results.
- Integration: lazy_jit still passes (cache doesn't intercept during index
  build paths).
- Determinism: same query twice → cache returns identical full text.

## Phase 6 — Benchmark verification

- Run reliary self-bench (4 seeds, A only) before/after:
  - Score must stay 25.0 ± 0.5 (no regression)
  - WC: expect -15-30%
  - Billed: expect -10-20%
  - Wall: expect -20-35%
- Run 3-way (A/B/C) once if the A-only numbers hold.
- If score regresses on compact-repeat (1b), disable 1b only (keep 1a full-text).

## Rollout order

1. Commit V55 (late-interaction) — isolated, testable.
2. Phase 1a/1c/1d (cache + single-flight + parallel) — biggest wall/WC win.
3. Phase 2 (in-memory phrase index) — latency + V55 gating.
4. Phase 3 (micro) + Phase 4 (gate late-interaction).
5. Phase 5 tests + Phase 6 bench.

## Effort estimate

| Phase | Time |
|-------|------|
| 1 (cache + dispatch) | 2h |
| 2 (phrase index) | 2.5h |
| 3 (micro) | 45min |
| 4 (late-interaction gating) | 30min |
| 5 (tests) | 1h |
| 6 (bench, 2 runs × ~30min) | 1.5h |
| Total | ~8h |

## Risks & mitigations

| Risk | Mitigation |
|------|-----------|
| Compact-repeat breaks model behavior | Bench A/B; disable 1b if score drops |
| In-memory index stale after reindex | mtime check + SQL fallback |
| Parallel dispatch + SQLite contention | thread-local conns (already per-thread via V54 cache) |
| Cache key collisions (fxhash) | 64-bit + include tool name + full canonical args |
| V55 rerank slows search | Phases 2b/4 move its scan to in-memory index |
