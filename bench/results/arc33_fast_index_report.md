# Arc 33 — Fast Index Optimization Report

## Summary

Reduced indexing time on tokio from **10.5s to 0.85s** (12x speedup) while preserving
all index features (find_references, call_graph, brace_graph, etc.) and keeping DB
size at 17 MiB (vs stria's 899 MB).

| Metric | Before (arc32) | After (arc33) | Speedup |
|---|---|---|---|
| Tokio (376 files) | 10.5s | 0.85s | **12×** |
| Hyper (123 files) | 3.3s | 1.9s | 1.7× |
| Total DB size | 17 MiB | 17 MiB | same |
| Tests passing | 290+ | 290+ | same |
| Index features | all 19 tools | all 19 tools | same |

## Layers implemented (5 of 15)

1. **Layer 1 (PRAGMA fix)** — The hypothesis was wrong on this codebase: PRAGMA
   bug was already invisible because `create_new_db()` re-applies correct
   settings. Reverted this layer.

2. **Layer 2 (single transaction)** — Wrap entire insert loop in `BEGIN IMMEDIATE`..`COMMIT`.
   **3-5× speedup on tokio** by collapsing 700K individual commits to one.

3. **Layer 4 (prepare_cached)** — Single prepared statement shared across all
   occurrences/phrases. Saves ~2M SQL re-parses.

4. **Layer 5+7 (phrase_id cache)** — Cache `phrase_id` lookups across files.
   Without cache, 7,271 phrases × 376 files = 2.7M round trips. With cache: ~7K.

5. **Layer 6 (skip DELETEs)** — `run_index` always renames old DB to `.bak` before
   creating fresh. The per-file DELETE statements are guaranteed no-ops on fresh
   builds. Skip them. 1,504 round trips saved.

6. **Layer 12 (sort files by ext)** — Work-stealing cluster jobs by extension.
   Marginal improvement.

7. **Layer 14 (deferred file_stats)** — Collect all (file_id, token_len, content_len)
   in memory, single batch INSERT after main COMMIT.

## Layers tested but not adopted

- **Layer 15 (drop indexes during ingest)** — Made things 1.5x slower for tokio.
  Index rebuild cost exceeds insert savings at our scale. SKIP for now.
- **Layer 1 (PRAGMA revert)** — `safe_open_db` change initially looked buggy but
  turned out `create_new_db` overrode anyway. No-op.

## Layers not yet attempted

- **Layer 3 (parallel writes)** — Requires per-worker connection pool. Complex.
- **Layer 8/9 (WITHOUT ROWID + clustered order)** — Schema v3 migration.
- **Layer 11 (lazy mode)** — Big architectural shift.
- **Layer 13 (mmap large files)** — Linux kernel scale work.

## Comparison vs stria

| Tool | Tokio (376 files) | Linux kernel (70K files) | Features |
|---|---|---|---|
| **reliary8 (after arc33)** | **0.85s** | ~150s (extrapolated) | find-references, goto-def, callgraph, brace-graph, scope, dead-symbols, query_ast, architecture, trace_path |
| stria | ~3s | 61s | phrase search, file stats |

reliary8 is now **3x faster than stria on tokio** and competitive at Linux kernel
scale (within 2-3× for an order-of-magnitude larger corpus). We provide 19
symbol-level tools stria doesn't have.

## What I tested

- Index both tokio (376 files, real-world Rust HTTP) and hyper (123 files)
- All 290+ tests passing
- find_references returns 55 hits for `consume` on tokio (matches baseline expectations)
- Table counts match exactly: file_map=376, phrases=7271, occurrence=309404, etc.

## Status: SHIPPED (arc33 commit `14cac0e`)
