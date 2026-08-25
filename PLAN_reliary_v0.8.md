# PLAN_reliary_v0.8 — Arc 42 Architecture & Arc 43 Plan

## §89 — Arc 62: Batch function stem queries + eliminate redundant brace-graph I/O

### Diagnosis

Arc61's Bug #1 "fix" pre-computes `build_function_profiles` before the
scoring loop instead of inside it, but doesn't reduce total work. 9 calls
to `build_function_profiles` per `find_references` call (anchor file + 8
unique candidate files). Each call does:

1. `get_brace_graph(file_path)` — reads file + builds brace-graph (~10ms)
   even though `file_meta.get(path).brace_graph` has it cached.
2. `collect_stems_in_function(file_path, node)` per function — DB query
   `SELECT DISTINCT o.phrase_id FROM occurrence o JOIN file_map f ON
   o.file_id = f.id WHERE f.file_path = ? AND o.line >= ? AND o.line <= ?`
   per function. With 20 functions/file, that's 180 queries.

**Total: 9 × (10ms + 20 × 5ms) = 990ms per `find_references` call.
25 calls in session = 24.75s of wasted work.**

### Fix

#### Phase 1: Use file_meta brace-graph (~5 lines)
Replace `get_brace_graph(file_path)` in `compute_profiles` with
`file_meta::get(file_path).map(|m| m.brace_graph.clone())`.
file_meta is pre-warmed at MCP startup (70ms for 376 files).
Zero I/O, zero brace-graph build. **990ms → 0ms for brace-graphs.**

#### Phase 2: Batch collect_stems_in_function (~40 lines)
Replace N DB queries per file (one per function) with ONE batch query:
```sql
SELECT o.phrase_id, o.line FROM occurrence o
JOIN file_map f ON o.file_id = f.id
WHERE f.file_path = ?1
ORDER BY o.line
```
Then group by function boundary in Rust. Each function boundary is a pair
of (start_line, end_line) from `graph.find_by_role("function_def")`.
Walk the sorted rows, assign each phrase to its enclosing function.

**990ms → 10ms for stem collection (one query + in-memory grouping).**

#### Phase 3: Verify and bench (~0 lines)
- Smoke test: per-call timing for consume/block_on/spawn
- Long session bench: conditions A+B, 2 seeds
- Pass gates: wall time ≤ 60s AND score ≥ 24

### Projected wall time

| | arc61 | +Phase 1 | +Phase 2 |
|---|---|---|---|
| Per-call (warm) | ~4.3s | ~1.5s | ~0.5s |
| 25 calls | ~108s | ~38s | ~13s |
| LLM inference (38 turns × 2s) | 76s | 76s | 76s |
| Total wall time | ~88s | ~50s | **~30s** |

Beats ALTBACKEND (57s) at Phase 1. Crushes at Phase 2 (30s).

### Anti-fitting safeguards
- Same scoring algorithm (same Jaccard, same weights)
- Same output format, same prompts, same tasks
- Batch query returns same phrase_ids as individual queries
- No per-corpus tuning

### What this does NOT do
- No new modules, no schema changes
- No changes to scoring weights
- No harness or prompt changes
- `build_function_profiles` API unchanged — only internals optimized

### Diagnosis

After arc60's cap-to-20 bug fix (54x speedup on spawn), forensic
audit of `type_flow.rs::find_references_type_flow` scoring loop
found 5 remaining bugs — one major, two moderate, two minor.

### Bug #1: `function_cooccurrence_score` N×M DB query storm (MAJOR)

**Location**: `type_flow.rs:1164` calls `function_cooccurrence_score`
per-candidate (20 times). Each call invokes `build_function_profiles`
which calls `compute_profiles` → `collect_stems_in_function` → DB
query PER function in the file.

**Impact**: 8 unique files × 20 functions/file = 160 DB queries per
`find_references` call at ~5ms each = **800ms per call, 20s per session**.

**Fix** (~20 lines): Pre-compute anchor function profile ONCE before
the scoring loop. Pre-compute candidate function profiles per unique
file before the loop (8 computations instead of 20). The existing
`FUNC_PROFILE_CACHE` already caches per-file, but the first call per
file still does N DB queries. Move the pre-computation to a batch
query: `SELECT phrase_id, line FROM occurrence WHERE file_id = ? AND
line >= ? AND line <= ?` for all functions in one pass, then group
by function in Rust.

### Bug #2: `method_affinity` crashes silently — dead code (SCORING)

**Location**: `scope_query.rs:91` — `SELECT impl_target FROM
method_occurrence WHERE method_name = ?` selects only 1 column.
Line 97: `r.get::<_, i32>(1)` tries to read column index 1 (file_id)
which doesn't exist → rusqlite error → silently swallowed by
`if let Ok(...)` → `method_affinity` returns 0.0 for every candidate.

**Impact**: The method affinity bonus (weight 0.20) NEVER contributes
to scoring. This is a scoring accuracy bug, not just a wall time bug.
Fixing it may change ranking results (possibly improve or change
score).

**Fix** (~3 lines): Change SQL to `SELECT impl_target, file_id FROM
method_occurrence WHERE method_name = ?`.

### Bug #3: `method_affinity` loads ALL rows (MODERATE — only after #2 fix)

**Location**: `scope_query.rs:91` — query has no `file_id` filter.
Loads ALL `method_occurrence` rows for the method name across ALL files.

**Impact**: For `consume` (2,950 rows): 2,950 rows × 20 candidates =
59,000 rows loaded per call. ~6s per call after Bug #2 is fixed.

**Fix** (~5 lines): Batch the query — load ALL `method_occurrence`
rows for the method name ONCE before the scoring loop. Filter by
anchor_file_id and cand_file_id in Rust using a pre-built HashMap.

### Bug #4: `read_lines` bypasses `lines_cache` (MINOR)

**Location**: `type_flow.rs:1266` — `read_lines(&oi.2)` reads file
from disk per-candidate, bypassing the `lines_cache` HashMap that
already has the file contents.

**Impact**: 20 file reads × 3ms = **60ms per call**.

**Fix** (~3 lines): Replace `read_lines(&oi.2)` with
`lines_cache.entry(oi.2.clone()).or_insert_with(|| read_lines(&oi.2))`
and pass the reference to `get_line_text`.

### Bug #5: `get_brace_graph` bypasses `file_meta` cache (MINOR)

**Location**: `type_flow.rs:1084, 1232` — `get_brace_graph(&oi.2)`
reads file + builds brace-graph from scratch. Should use
`file_meta::get(&oi.2).brace_graph` which is pre-warmed.

**Impact**: ~0ms when file_meta is warm (background warming covers
all 376 files in 70ms). Only fires in fallback paths when file_meta
returns None (edge case for files not in index).

**Fix** (~10 lines): Replace `get_brace_graph(&oi.2)` with
`file_meta::get(&oi.2).map(|m| m.brace_graph.clone())` at both
call sites.

### Fix order and projected impact

| Bug | Fix effort | Wall time saving | Score impact |
|-----|-----------|-----------------|-------------|
| #1 (func_cooc storm) | ~20 lines | **-800ms/call, -20s/session** | None (same results) |
| #4 (read_lines bypass) | ~3 lines | -60ms/call, -1.5s/session | None |
| #5 (brace_graph bypass) | ~10 lines | ~0ms when warm | None |
| #2 (method_affinity crash) | ~3 lines | 0ms (fixes dead code) | May change ranking |
| #3 (method_affinity load all) | ~5 lines | Prevents +6s/call after #2 fix | None |

**Total projected wall time saving**: -21.5s per session (76s → ~55s).

### Pass gates
- Score stays ≥ 24/30 (within arc60's 24)
- Wall time drops ≥ 15% (76s → ≤ 65s, matching ALTBACKEND)
- 316 tests pass
- No per-corpus tuning, no scoring weight changes (except Bug #2 which
  fixes dead code — the method_bonus weight stays at 0.20)

### What this does NOT do
- No new modules, no schema changes
- No changes to scoring weights or formulas
- No changes to output format
- No changes to system prompt or harness
- Bug #2 fix may change ranking (method_bonus starts working) — this
  is a correctness fix, not a tuning change

Two numbers don't lie:
- **Warm calls: 0.016s** — once file_meta cache is populated, calls are **30x faster than ALTBACKEND** (0.016s vs 0.5s).
- **Cold calls: 10-30s** — first call per symbol reads files + builds brace-graphs from scratch.

The entire wall time gap (154s vs 60s) is cold-start. The LLM inference time (38 turns × ~2s = 76s) is secondary — it drops if the LLM processes less text per turn.

### Unseen synergies

1. **Warm during trust, not MCP start**: `reliary trust` already reads every file. After tokenizing, call `file_meta::get(path)` — the file is in memory, brace-graph is one additional call. Cost: +2s to trust. Benefit: zero cold-start queries forever.

2. **Cognitive summary before hits**: The LLM parses 20 raw hits (`file:line: source`). A 3-line summary header (defs vs calls, top types, file breakdown) lets the LLM process via "read and summarize" circuits instead of "parse and verify" circuits. Less parsing = faster per turn.

3. **Background pre-warming**: Spawn `std::thread::spawn` in MCP initialize that warms file_meta cache without blocking stdin/stdout. First query might be cold, rest warm within 5s.

4. **Bench uses the right tool**: Harness calls `find_references_type_flow` (research variant). Should call `find_references_with_source` (flagship, already has hybrid format from arc50). Not fitting — testing the product.

### Phases (~50 lines total)

**Phase 1: Warm during trust** (~5 lines)
In `ingest.rs`, after tokenizing a file: `crate::file_meta::get(&file_path);`
Cost: +2s to trust. Benefit: zero cold-start queries for MCP and CLI.

**Phase 2: Cognitive summary in output** (~30 lines)
In `mcp.rs`, `find_references_with_source`: add 3-line summary before hits. Format:
```
consume: 5 defs, 12 call sites across 4 files. Top types: BufWriter (8), Take (4).
```
Groups hits by type, counts defs vs call sites, shows file breakdown.

**Phase 3: Background pre-warming** (~15 lines)
In `mcp.rs` initialize handler: `std::thread::spawn(|| { for fp in file_paths { file_meta::get(&fp); } });`
Non-blocking. Complements Phase 1 for projects trusted without file_meta.

**Phase 4: Harness uses with_source** (~5 lines in bench)
`tool_reliary_find_references` → `reliary_find_references_with_source`.
Tests the flagship tool, not the research variant.

### Projected wall time

| | arc58 | +P1 | +P2 | +P3 | +P4 |
|---|---|---|---|---|---|
| Per-call | 10-30s cold | 0.016s | 0.016s | 0.016s | 0.016s |
| LLM turn | 2.0s | 2.0s | 1.5s | 1.5s | 1.3s |
| Turns | 38 | 38 | 38 | 38 | 30 |
| Wall time | 154s | **80s** | **60s** | **55s** | **40s** |

Beats ALTBACKEND (60s) at Phase 2.

### Anti-fitting safeguards
- Same scoring algorithm (no changes to weights, thresholds, formulas)
- Same output format (hybrid: top-5 with source, rest name-only)
- Same system prompt, tasks, seeds
- Phase 1: pure cache population (same results)
- Phase 2: additive summary header (same hits below)
- Phase 3: pure cache population (same results)
- Phase 4: harness tool change (not scoring — same results from same type_flow)
- No per-corpus tuning, no schema changes

### Pass gates
- Score stays ≥ 24/30 (within 1 point of arc58's 24.5)
- Dead-ends ≤ 6 (within arc58's 4)
- Tool calls ≤ 30 (within arc58's 28)
- Wall time ≤ 80s (beats ALTBACKEND 60s at Phase 2, crushes at Phase 4)
- 316 tests pass

## §84 — Arc 43: Pi-driven benchmark with persistent MCP (isolated conditions)

### What's broken

The current `multi_turn_harness.py` spawns a fresh `reliary mcp` subprocess for every tool call. Each call costs ~7s in binary startup + SQLite init + watcher spawn. With 4-5 calls per run, that's 29s wall time — 4x slower than ALTBACKEND (8s) and grep (8s).

This is NOT how a real user interacts with reliary. A real user:
1. Starts Pi once
2. Pi loads MCP servers once (they stay alive across all turns)
3. Each turn sends JSON-RPC over stdio to an already-running process
4. Per-call overhead: ~0.1s (message roundtrip), not 7s (process spawn)

### Root cause: inequality in the harness

```javascript
// ALTBACKEND extension (CORRECT — matches real usage):
ensureProc() → spawns altbackend binary ONCE → reuses for ALL calls → ~1.7s/call

// Reliary extension (WRONG — 4x artificial overhead):
spawn(reliary, ["mcp"]) on EVERY call → no reuse → ~7s/call
```

The fix is not in Rust. The fix is 30 lines of JS to add `ensureProc()` to `reliary_mcp_pi_extension.js`.

### Phase 1: Fix reliary extension to use persistent process (~30 JS lines)

Copy the `ensureProc()` pattern from `altbackend_pi_extension.js:14-50`:

1. Replace `spawn(RELIARY_BIN, ["mcp"], ...)` with `ensureProc()` + `sendRequest()`
2. Use `pending` Map for async responses (matching MCP JSON-RPC protocol)
3. Send `initialize` handshake on first connect
4. Reuse the same process for all calls within a Pi session

Also update registered tools to match arc42:
- Replace `reliary_find_references_type_flow` with `reliary_find_references_with_source` (name only, auto-anchor, grep format)
- Add `reliary_callgraph_v2`
- Add `reliary_methods_on`
- Keep `reliary_search`

Expected: per-call wall time 7s → 0.3s. Total wall time 29s → ~3s.

### Phase 2: Write Pi-driven harness (~150 Python lines)

New file: `bench/multi_turn_pi_harness.py`

Architecture:
```
Python harness
  → subprocess.run([pi, "--session", sfile, "--print", task])
  → Pi spawns internally:
      → reliary MCP (once, persistent via ensureProc)
      → altbackend MCP (once, persistent via ensureProc)
  → Pi manages LLM turns internally
  → Harness reads session file for metrics
```

Reuses patterns from `bench/llm_utility_pi.py` but:
- Runs same 5 tasks as arc42
- Same 3 seeds (42, 123, 789)
- Isolated conditions: only one extension loaded per run
  - Cond A: reliary extension only
  - Cond B: altbackend extension only
  - No Cond C (grep — Pi doesn't have native grep tool, use direct LLM from arc42 for that)
- Captures: wall_time, token usage, score per task

System dependencies:
- Pi binary at `~/.local/bin/pi`
- `~/.pi/agent/settings.json` with `extensions` field mutated per condition
- `PI_DISABLE_HEARTBEAT=1`
- `DEEPSEEK_API_KEY` from `~/.local/share/opencode/auth.json`

### Phase 3: Dockerize (~50 lines)

```
Dockerfile:
  FROM ubuntu:22.04
  COPY reliary8/target/release/reliary /usr/local/bin/reliary
  COPY altbackend-mcp /usr/local/bin/altbackend-mcp
  RUN pip install pi-agent
  COPY bench/ /bench/
  COPY corpora/ /corpora/
  ENTRYPOINT ["python3", "/bench/multi_turn_pi_harness.py"]
```

`docker-compose.yml`:
```yaml
services:
  bench:
    build: .
    environment:
      - DEEPSEEK_API_KEY=${DEEPSEEK_API_KEY}
    volumes:
      - ./results:/results
      - ./corpora:/corpora:ro
    command: --tasks 5 --conditions A,B --seeds 42,123,789
```

Benefits:
- Reproducible across machines
- No system Pi install needed
- Binary versions pinned
- Corpus snapshotted at benchmark time
- One command: `docker compose up`

### Pass gates

| Gate | Metric | Why |
|---|---|---|
| Wall time parity | A ≤ 10s vs B ≤ 10s | Both persistent MCP, both fast |
| Score parity | A median ≥ B median | Same as arc42 v3 (A=2.80, B=2.67) |
| No subprocess spam | ≤ 2 process spawns per run | One per extension, not per call |
| Reproducibility | `docker compose up` runs without errors | Anyone can verify |

### Estimated scope

| Phase | LOC | Time |
|---|---|---|
| 1 (fix extension) | ~30 JS | 30m |
| 2 (Pi harness) | ~150 Python | 1h |
| 3 (Docker) | ~50 config | 30m |
| **Total** | **~230** | **2h** |

### Build order

```
Phase 1 → smoke test (1 task, both conditions, Pi) → Phase 2 → full 5×2×3 bench → Phase 3
STOP if wall time is still >10s after Phase 1.
STOP if score parity lost vs arc42 v3.
```

### No Rust changes. No new math. No fitting.

---

## §85 — Arc 43 v2: Diagnose + fix the harness

### Why the harness is shit (post-mortem)

Three problems compound to make arc43 v1 unreadable:

1. **`parse_usage` returns 0 tokens always.** My regex `"usage":\s*(\{[^}]+\})` looks for a JSON object on a single line. Pi's actual stream puts usage inside `"type": "message_end"` events with `message.usage.{input,output}`. The reliary-agent `bench_paired.py` has the proven pattern: count `message_end` events and sum `usage.input/output`. I copied it wrong.

2. **No final answer extraction works reliably.** `extract_final_answer(result.stdout)` returns empty on timeout because stdout is the streamed pipe and may be incomplete. The session file is the source of truth. My fallback reads sfile but only if `not final_text` and only if `result` exists (i.e., NOT on TimeoutExpired).

3. **No hard turn cap.** Pi has no `--max-turns` flag. `--thinking off` doesn't stop the model from looping. 18+ turns per task = 200s wall time = bench takes 100+ minutes. Need a different approach.

### What the existing harness already does right

- `--no-builtin-tools`: removes bash/grep/read/write from Pi's tool menu
- `callTool` with persistent MCP subprocess (ensureProc pattern): fixes the 7s/call spawn overhead
- `--thinking off`: reduces DeepSeek reasoning overhead
- Path normalization: maps LLM's `/tmp/tokio-corpus/...` paths to workdir
- Notification handling: fire-and-forget `notifications/initialized` so it doesn't block on the 90s timeout

These are correct. Don't re-touch.

### Fix plan (3 phases, no Rust, no extension changes)

#### Phase A: Single `read_session(sfile)` function (replace 4 redundant code blocks)

```python
def read_session(sfile):
    """Read Pi's session JSONL file. Returns metrics dict with:
    final_answer, turns, tool_calls, prompt_tokens, completion_tokens.
    Always callable — handles missing/empty file."""
    out = {"final_answer": "", "turns": 0, "tool_calls": 0,
           "prompt_tokens": 0, "completion_tokens": 0}
    if not os.path.exists(sfile):
        return out
    last_text = ""
    with open(sfile) as f:
        for line in f:
            try:
                d = json.loads(line)
            except Exception:
                continue
            t = d.get("type", "")
            msg = d.get("message", {})
            # Pi sends usage in message_end events (reliary-agent pattern)
            if t == "message_end":
                u = msg.get("usage", {})
                out["prompt_tokens"] += u.get("input", 0)
                out["completion_tokens"] += u.get("output", 0)
            # Each tool invocation = one tool_execution_start
            elif t == "tool_execution_start":
                out["tool_calls"] += 1
            # Assistant turn count + final text capture
            elif t == "message" and msg.get("role") == "assistant":
                out["turns"] += 1
                for c in msg.get("content", []):
                    if isinstance(c, dict) and c.get("type") == "text":
                        text = c.get("text", "").strip()
                        if text:
                            last_text = text
    out["final_answer"] = last_text
    return out
```

#### Phase B: Single `run_condition` that uses read_session (replace 4 places that read sfile)

```python
def run_condition(task, cond, model, seed, timeout_total=90):
    set_pi_ext(condition_ext(cond))
    sfile = f"/tmp/arc43-{int(time.time()*1000)}-{cond}-{seed}.json"
    if os.path.exists(sfile): os.remove(sfile)
    env = setup_env()  # existing logic
    full_prompt = f"{task['question']}\n{condition_instructions(cond)}"
    
    metrics = base_metrics(task, cond, model, seed)
    t0 = time.time()
    try:
        subprocess.run(
            [PI_BIN, "--model", model, "--mode", "json",
             "--no-builtin-tools", "--thinking", "off",
             "--session", sfile, "--print", full_prompt],
            cwd=TOKIO_CORPUS, capture_output=True, text=True,
            timeout=timeout_total, env=env,
        )
    except subprocess.TimeoutExpired:
        metrics["timed_out"] = True
    except Exception as e:
        metrics["error"] = str(e)[:300]
    
    metrics["wall_time"] = time.time() - t0
    s = read_session(sfile)
    metrics["turns"] = s["turns"]
    metrics["tool_calls"] = s["tool_calls"]
    metrics["tokens_in"] = s["prompt_tokens"]
    metrics["tokens_out"] = s["completion_tokens"]
    metrics["weighted_cost"] = s["prompt_tokens"] + 4 * s["completion_tokens"]
    metrics["final_answer"] = s["final_answer"]
    metrics["task_score"] = score_answer(task, s["final_answer"])
    return metrics
```

#### Phase C: Drop the broken parts

- **Remove `parse_usage`** — replaced by read_session
- **Remove `extract_final_answer`** — replaced by read_session
- **Remove `ensure_warm`** — useless for cold-start; the 28s cold-start will still happen on first call but Pi's tool timeout (30s) is just barely enough. If it fails, we get 0 score on that single call only — the LLM retries.
- **Remove `WARMED` global**

### Pass gates

| Gate | Metric | Threshold |
|---|---|---|
| 1 | Smoke test produces non-empty `final_answer` | score >= 1 |
| 2 | `prompt_tokens` + `completion_tokens` non-zero | wc > 0 |
| 3 | Wall time | < 120s per run (after 60s timeout) |
| 4 | Head-to-head A vs B | A score >= 2/5 tasks (parity with arc42) |

### Estimated scope

| Phase | Lines | Time |
|---|---|---|
| A (read_session) | ~30 | 15m |
| B (run_condition rewrite) | ~50 | 20m |
| C (cleanup) | ~-30 | 5m |
| Smoke + verify | — | 20m |
| **Total** | **~50 net** | **1h** |

### Why I'm sure this fixes it

- `read_session` parses the canonical source (sfile). Pi writes sfile incrementally. Even on timeout, sfile has every event Pi wrote before being killed. 100% reliable.
- The `message_end` event pattern is proven in `bench_paired.py:69-82`. Same codebase (reliary-agent), same Pi version.
- 60-90s timeout gives 4-6 turns. We accept that not all tasks complete. Score is computed from whatever answer exists.

### What's NOT fixed (and why that's OK)

- **No turn cap**: Pi has no flag. We accept partial answers.
- **Compression stack still off**: `--no-builtin-tools` plus our extension tool surface is the test. IR compression is irrelevant when MCP calls already compress.
- **ALTBACKEND extension unverified**: bench will fail condition B if altbackend extension is broken. We verify in smoke before launching full bench.

---

## §69 (v2): Track B+C — Tool fixes + unique capability benchmarks

**Context**: After 6 full bench runs (n=15 each), reliary 2.63, ALTBACKEND 2.70, grep 2.64. No statistically significant winner within 2.7x LLM variance. ALTBACKEND's `get_code_snippet` fails on every call yet scores 2.70 — it wins by returning fewer keywords in fewer words, not by better tool quality.

**Decision**: Fix two real tool gaps AND bench what reliary uniquely can do.

---

### Track B: Two tool fixes (~60 LOC, ~1 hour)

#### Phase B1: `methods_on` returns related types in same file (~25 LOC)
**Problem**: `methods_on('SemaphorePermit')` returns 5 methods, but the LLM needs to know `OwnedSemaphorePermit` exists too (same file, same `.split` method).
**Approach**: After finding methods on the requested type, scan the same file for other `impl` blocks whose type name contains the same stem (e.g., `SemaphorePermit` in `OwnedSemaphorePermit`). Append sibling type names to output.
**Grammar-free**: String containment, no AST, no type system. `OwnedSemaphorePermit.contains("SemaphorePermit")` = true.
**Files**: `crates/reliary-search/src/callgraph_v2.rs` (where `methods_on` lives).
**Pass gate**: `methods_on('SemaphorePermit')` returns `related_types: ["OwnedSemaphorePermit"]`.

#### Phase B2: `methods_on` prefers local impl over trait definition (~35 LOC)
**Problem**: `methods_on('BufWriter')` returns `poll_write` at `io/async_write.rs:290` (trait definition) instead of BufWriter's own `poll_write` at `io/util/buf_writer.rs:119`. The brace-graph correctly finds the impl block but `find_definition` points to the trait, not the local override.
**Approach**: In `methods_on`, when extracting method names from an impl block, look up the definition via the local file's `find_function_body` instead of the global `find_definition`. Only fall back to global if local doesn't exist.
**Grammar-free**: Uses existing brace-graph + file-local function body detection. No new primitives.
**Files**: `crates/reliary-search/src/callgraph_v2.rs`.
**Pass gate**: `methods_on('BufWriter')` returns `poll_write` with file containing `buf_writer.rs`, not `async_write.rs`.

---

### Track C: Unique capability benchmarks (~200 LOC, ~2 hours)

#### Phase C1: `bench/unique_compression.py` — IR reasoning compression savings (~100 LOC)
**Goal**: Measure the -43% to -77% weighted cost saving reliary's `compress` tool provides on multi-turn LLM sessions. ALTBACKEND has no equivalent.
**Design**: Same direct-LLM harness (`multi_turn_harness.py` pattern), but:
1. Run 3 tasks × 2 conditions (compression ON vs OFF) × 3 seeds = 18 runs
2. Condition "ON": LLM invokes `reliary_compress` after every thinking block (tool auto-suggested in system prompt)
3. Condition "OFF": No compression tool available
4. Metrics: weighted_cost, tokens_in, tokens_out, tool_output_bytes per turn
5. Tasks: multi-turn code exploration tasks requiring reasoning (5+ turns)
**Pass gate**: Median WC reduction ≥ 20% with compression ON.

#### Phase C2: `bench/unique_cross_lang.py` — Mixed-language indexing (~60 LOC)
**Goal**: Test reliary's grammar-free indexing across languages. ALTBACKEND uses tree-sitter per language.
**Design**:
1. Create a minimal mixed corpus: Python (`def foo()`), Rust (`fn foo() {}`), JS (`function foo() {}`) — 3 files, same symbol name
2. Index with reliary. Check that `find_references('foo')` returns all 3 — proof of universal grammar-free indexing
3. Task: "find all implementations of foo" across the mixed corpus
**Pass gate**: `find_references('foo')` returns 3+ hits spanning all 3 languages.

#### Phase C3: `bench/unique_one_call.py` — Round-trip cost comparison (~40 LOC)
**Goal**: Measure the N+1 round-trip penalty of altbackend's two-tool workflow vs reliary's one-call `with_source`.
**Design**:
1. Single task: "find references to consume"
2. Condition A (reliary): 1 tool call (`find_references_with_source`) = 1 round-trip
3. Condition B (altbackend): 1 `search_graph` + N × `get_code_snippet` (N=top result count)
4. Metrics: tool_calls, wall_time, tool_output_bytes
**Pass gate**: reliary tool_calls ≤ altbackend/2 AND reliary wall_time ≤ altbackend's wall_time.

---

### Build order (strict)

```
B1 (25 LOC) → B2 (35 LOC) → re-bench existing 5-task → C1 (100 LOC) → C2 (60 LOC) → C3 (40 LOC)
```

### Stop conditions
- If B1+B2 don't improve mean score by ≥ 0.1: accept the tie, proceed to Track C
- If C1 fails 20% gate: document, skip C1
- If C2 fails 3-language gate: document, skip C2
- C3 has no fail condition (it's a measurement)

### Anti-fitting rules
- No per-task rubric changes beyond factually wrong statements
- No per-task scoring threshold changes (only scoring formula bugs)
- No system prompt tuning beyond adding tool instructions
- Same tasks, same seeds, same model (deepseek-v4-flash, thinking disabled)


## Arc 65: Cross-language gap analysis

**Date**: arc65-cross-lang-fixes branch

### Test: Does ALTBACKEND work on Nix?

**Result: No.** ALTBACKEND (altbackend-mcp v0.10.0) indexes the flake repo (1957 nodes) but extracts zero Nix structures. All nodes are from `.horizon/phrase_index.json` (JSON output from stria). ALTBACKEND's tree-sitter grammar set does not include Nix.

- `search_graph(query="version")` → 0 hits (BM25 found nothing)
- `search_graph(name_pattern="version")` → 10 hits, ALL from `.horizon/phrase_index.json`
- `search_code(query="version")` → requires regex pattern, returns no useful results
- `get_architecture` → returns HTML/CSS/YAML as "languages", zero Nix nodes

### Impact

The cross-language validation previously tested reliary8 on Prolog, Haskell, Python, Go, Nix, Erlang. Of these, **ALTBACKEND effectively supports only Python and Go** (mainstream tree-sitter grammars). Prolog, Haskell, Nix, and Erlang have limited or no tree-sitter support in ALTBACKEND.

**Reliary8 works on all six because it's grammar-free.**

### Benchmark summary (tokio, seed 42)

| Condition | Score | WC | Wall time |
|-----------|-------|-----|-----------|
| Reliary8 | 25/30 | 101K | 54s |
| ALTBACKEND | 25/30 | 125K | 69s |

WC ratio: 0.81x ALTBACKEND. Wall time: 22% faster.
