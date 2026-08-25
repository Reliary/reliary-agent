# FIX_PLAN_V58 — Untapped Synergies

Goal: convert idle assets into product wins. Five phases + three micro-items.
Order chosen so each phase de-risks or feeds the next. All grammar-free.
All cache-safe unless noted. Target branch: `feat/v58-synergies` off `feat/v56-perf-cache`.

---

## Phase 1 — JIT generation counter × result cache  (~1h)  **DO FIRST**

**Problem**: V56's result cache regressed score because it served stale
pre-JIT results (`ensure_occurrence_for_phrase` populates rows lazily; a
cached hit from call #1 hides rows built by call #2).

**Design**: monotonic generation counter folded into cache key.

1. `crates/reliary-agent/src/mcp.rs`:
   - `static INDEX_GEN: AtomicU64 = AtomicU64::new(1);`
   - `pub fn bump_index_gen()` — call after every `ensure_occurrence_for_*`
     success that inserted >0 rows, after `trust`, after reindex/watcher build,
     after `build-all`.
   - In `handle_tool_call_stdio`: cache key = `(name, canonical_args_json, INDEX_GEN.load(Relaxed))`.
     Store gen with entry; on hit, if stored_gen != current → treat as miss.
2. Bump sites: wrap `ensure_occurrence_for_phrase/file/phrase_all` results in
   mcp.rs dispatch paths (single choke point per tool call is fine — bump once
   post-dispatch if any ensure returned rows>0). Simplest correct version:
   bump inside `reliary-search::lazy_occurrence` via a callback registered at
   server start (`set_gen_bumper(fn)`); default no-op keeps crate pure.
3. Flip default: `RELIARY_RESULT_CACHE` default **on** (env `=0` disables).
   Keep compact-repeat format (`cached: <first line>`).

**Tests**: unit test — two identical calls where first triggers JIT, second
must NOT return `cached:` of the empty result. Integration: existing MCP smoke
(def_only → repeat) plus methods path.

**Verify**: snapshot bench cond A, 4 seeds. Gate: score ≥ V54 band (≥23),
WC ≤ 90k, dead-ends ≤ 3. If pass → this becomes default behavior before push.

---

## Phase 2 — reliary-memory wiring: `similar` + `prior`  (~4h)

The HDC crate is built + math-optimized (bit-packed HVs, XOR+popcount,
integer Hebbian) and completely idle. Two tools:

### 2a. `similar(name)` — near-clone detection
1. At `trust` time (ingest.rs): for every `is_def=1 tag=1|2` row, encode
   token-set → hypervector. Storage: new table `hv(fn_name TEXT, file TEXT,
   line INT, hv BLOB)` (1250 bytes packed per row; tokio ≈ 3.5K defs ≈ 4MB).
   Encoder lives in reliary-memory (`encode_token_hv(tokens) -> Hypervector`),
   already exists as `ensure_token_hv` path.
2. Query path (`mcp.rs`, new arm `reliary_similar`):
   - load target fn's tokens (file_meta lines slice), encode, XOR+popcount vs all
   - rank desc, top 8, output one-line: `similar to <name>: other_fn (file:line, sim 0.87), ...`
3. Incremental: on reindex of file F, delete+re-encode F's defs only.

### 2b. `prior` — cross-session memory
1. Store at session end (MCP shutdown hook / `Drop`): JSON blob per repo hash:
   `{tools_used_counts, last_edits:[file,line], successful_task_summaries}`.
   Location: `.reliary/prior.json` (per-repo, gitignored).
2. Tool `reliary_prior`: returns top-N lines ("last session touched X, Y;
   most-used tool was find_references"). Hebbian consolidation = decay old
   entries (crate already has tiered consolidation).
3. Optional synergy: feed prior into system-prompt tail? NO — breaks KV cache.
   Keep it pull-only (model calls the tool).

**Grammar check**: token→HV encoding is bag-of-tokens hashing; no parsers.
**Tests**: roundtrip encode/similarity (crate has some); MCP smoke for both arms.
**Risk**: low. Crate untouched; only additive tables/tools.

---

## Phase 3 — edit_context + hologram_plan → describe() superpower  (~3h)

Stria-proven features onto our faster index.

1. Port `edit_context(file_or_symbol)` from stria into
   `crates/reliary-search/src/edit_context.rs`:
   - blast radius: files sharing ≥k rare phrases with target (use phrase_index
     for candidate scan, occurrence for counts)
   - verify candidates: test files whose path mirrors target dir OR share
     ≥j def-phrases with target
   - read_first: top-3 blast files by overlap
   - coupled: 4 highest-coupling non-test files (latent deps)
   - risk: low/moderate/high from (blast size, test coverage presence)
2. Wire into `describe` handler: when args has `edit_context=true` OR name
   resolves to a file with recent-edit intent flag, append section:
   ```
   Editing X affects: a.rs, b.rs
   Verify with: crates/x/tests/y.rs
   Read first: c.rs, d.rs
   Risk: moderate
   ```
3. hologram_plan stays standalone (`reliary_plan`) for task→file routing;
   `reliary fix` consumes it as move #1 (see Phase 5).

**Output budget**: cap whole describe at ~1500 chars (Phase-6c rule below).
**Tests**: port stria's edit-context tests, adapt fixtures to our index.

---

## Phase 4 — Bench-in-CI regression gate  (~2h)

All pieces exist; connect them.

1. New `.github/workflows/bench-gate.yml`:
   - runs on PRs touching `crates/**` or `bench/**`
   - steps: build release → `reliary trust bench/fixtures/mini-corpus` → run
     **deterministic** checks only (NO LLM calls in CI):
     a. determinism suite (sha256 twice on fixed queries — exists)
     b. accuracy_scorer against canned model outputs (no API)
     c. perf smoke: p95 latency of 20 core queries < threshold (e.g. 50ms)
   - job summary posts score table; fail if any regression > tolerance
2. Create `bench/fixtures/mini-corpus/` — 30-file synthetic Rust tree with
   known ground truth (reuse edit_tasks.json markers + reliability_ground_truth).
3. Script `bench/ci_gate.py`: single entrypoint, exits nonzero on regression.
4. Do NOT wire DeepSeek-keyed benches into CI (cost + flakiness). LLM judge
   stays manual/local.

---

## Phase 5 — Edit primitive → `reliary fix "<task>"`  (~8h, product headline)

1. **Port stria edit primitive** into `crates/reliary-edit` (replace stub):
   - `apply_edit(db, file, old_text, new_text) -> Result<AppliedEdit>`
   - resolve boundaries via cached brace_graph (function-targeted replace);
     indentation-anchored fallback (grammar-free)
   - fuzzy match: exact → whitespace-normalized → AST-equivalent block match
     (stria reported fuzzy failure 15% → ~0 with boundary resolution)
2. **DeepSeek client** in reliary-agent (reqwest+rustls already dep):
   - POST chat/completions, model `deepseek-chat`, key `DEEPSEEK_API_KEY`
   - parse `tool_calls`; strict JSON schema for edits
3. **Agent loop** `src/fix_loop.rs`:
   - move #1: `hologram_plan(task)` (Phase 3 asset) → read_first files
   - iterate ≤10: LLM → dispatch tool via direct library calls (zero MCP
     overhead) → append result; edits route through apply_edit
   - after each edit: `cargo check -p <crate>` gate; on fail, feed errors back
   - final: summary {files_changed, verify_result, tokens, wall}
4. **CLI**: `reliary fix "<task>" [--repo .] [--max-iters 10] [--dry-run] [--json]`
5. **Verify**: run the 3 Pi-Agent edit-bench tasks through `reliary fix`;
   gate: score ≥ 2.33 (Pi baseline), compile 3/3.

---

## Phase 6 — Micro-items (each ≤45min)

### 6a. phrase_index powers late-interaction rerank
Replace per-term SQL LIKE scans in `late_interaction_rerank` with
in-memory index lookups (V56 index already has prefix/substring).
Expected: rerank cost ms → µs.

### 6b. BraceNode.method_calls_in for callee extraction
callgraph_v2 body-scan → use cached graph node's `method_calls_in`
(eliminate re-tokenization per anchor). Guard: fall back to scan if
graph lacks data.

### 6c. describe() output budget
Hard-cap describe text at 1500 chars: answer line first, evidence after
("Lost in the Middle" ordering already proven). Trim evidence lines from
bottom, never the answer.

### 6d. Watcher decision
Default-off watcher currently dead weight. Choose: (a) delete code, or
(b) repurpose as background JIT warmer (on file-change events, pre-build
occurrence for touched files + bump INDEX_GEN). Recommend (b) only if
Phase 1 lands cleanly; else delete.

### 6e. Unified eval runner
`bench/run_eval.py --all`: comprehension bench + edit bench + accuracy +
LLM judge, one JSON report. Removes 4-script dance before every comparison.

---

## Execution order & gates

| Step | Depends on | Gate |
|------|-----------|------|
| P1 gen-counter | — | bench A ≥23 score, WC ≤90k |
| P6a/6b/6c | P1 optional | unit tests, no bench change needed |
| P2 memory tools | — (parallel-safe) | MCP smokes + similarity sanity (BraceNode dupes found) |
| P3 edit_context | — (parallel-safe) | ported tests pass |
| P4 CI gate | mini-corpus fixture | workflow green on self-PR |
| P5 fix loop | P3 (plan) + P1 (cache safe) | edit-bench tasks ≥2.33 via CLI |
| Push master-rebuild | P1 landed | scrub complete + signed squashed commit |

## Risk register

- R1: gen-counter misses a bump site → stale cache again. Mitigation: bump
  conservatively (any ensure-with-rows bumps); bench gate catches regressions.
- R2: HV table bloat on huge repos. Mitigation: cap defs encoded at 50K;
  document scaling limit.
- R3: edit_context false-positive coupling noise. Mitigation: rare-phrase
  threshold k≥3; cap lists at 4; user-visible labels not hard claims.
- R4: fix-loop runaway costs. Mitigation: --max-iters default 10, per-run
  token budget hard stop, --dry-run prints planned moves without executing.

