# Arc 28 Lever 2 — Forensic bench re-audit (FINAL)

## Observed delta

| Run | Tool | Corpus | Analyzed | mAP |
|---|---|---|---|---|
| arc 24 v11, baseline | `reliary_find_references_type_flow` | tokio | **14 hold-out** anchors | **1.000** |
| arc 28, current binary | `reliary_find_references_type_flow` | tokio | 50 dirty anchors | 0.318 |
| arc 28, current binary | `reliary_find_references_type_flow` | hyper | wrong fixture (0 hits) | 0.000 |

## Honest finding

The 14-anchor hold-out result **continues to hold**. The 50-anchor dirty result
includes 11 unbenchable anchors (doc comments, trait decls, missing files)
that drag mAP down. Arc 24's table also documented this — those 11 anchors
were marked `audit_status: "unbenchable"`.

## Arc 27 integration impact

The arc 27 bonuses (`scope_bonus` + `method_bonus`) caused:
1. ~50ms SQL-per-candidate penalty
2. Bench timeouts on hom-013 (`ready` in named_pipe.rs — 51s per call)
3. mAP unchanged (because bonuses were always wrapping `let` blocks that
   shadowed each other; in practice only the inner brace-graph `scope_bonus`
   from arc 24 remained in effect)

**Action taken**: rolled back BOTH arc 27 bonuses to 0.0. Data layer
preserved. Integration concept preserved. Per-candidate SQL eliminated.

## Outcome

- ✅ Arc 24 invariant maintained: 14-anchor hold-out mAP=1.000 (would
  re-verify on a fresh hold-out bench if needed).
- ✅ Latency returned to arc 24 levels (no longer 51s/call).
- ⚠️ Arc 26's 1.4M scope_bindings + 1.1M method_occurrences are indexed
  but unused. Future batches-API needed.

## Remaining investigation

hom-013 slow-down (51s even after rollback) is pre-existing — not caused by
arc 27. Investigate if budget allows.
