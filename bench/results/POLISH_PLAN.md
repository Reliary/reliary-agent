# Holographic Pack — Polishing Plan (Pre-Ship)

> Written: 2026-07-09. Status: NOT BUILT. This document is a plan only.
>
> All decisions, implementation steps, file paths, and validation gates are specified
> so that any builder (human or agent) can execute without ambiguity.

---

## Architecture Snapshot (What We Have)

```
Persistent Session Flow:
  Turn 0:  opencode run -s <id>  →  system prompt + 25K pack → cache miss
  Turns 1-N: opencode run -c -s <id> → query only → cache hit (pack stable)
  On edit:  gate.js triggerReindex + triggerPackRegen → pack regenerated

Static Artifacts:
  reliary pack . --format l2l3 --strategy full           → full pack (846 symbols, ~60K tokens)
  reliary pack . --format l2l3 --strategy hotspot --top-k 50  → hotspot pack
  reliary pack . --format l2l3 --strategy full --slice-query <q>  → BM25 slice (top-10, ~3K tokens)

Proven Metrics:
  20-turn persistent session: 95% score, $0.00047/turn, 1.0 tools/turn, 9.7s/turn
  Fresh (no pack):           90% score, $0.00094/turn, 5.3 tools/turn, 21.5s/turn
```

---

## Tier 1 — Ship Blockers (5 items)

### S1. Clean L3 Noise — Remove "unique line" Pollution

**Problem**: `is_noise_line` drops docstrings, simple assignments, imports, and bare returns.
The "unique line: ..." fallback still captures 5-10% of L3 entries for patterns not
covered by the existing detectors.  These entries add pack size without information.

**Patterns still appearing in L3 that should be filtered**:

| Pattern | Example | Why noise |
|---|---|---|
| `self.xxx(...)` method calls | `self.assertEqual(...)` | Python test assertions |
| Call chains ending in `;` | `block_phrases.append(line);` | Normal operation, not surprising |
| Python `from X import Y` | `from collections import defaultdict` | Already handled? (verify) |
| Bare identifier assignments | `found = true;` | Not surprising |
| Lines that are just a single word + comment or semicolon | `break;`, `continue;` | Already covered by length filter? |
| Rust derive attributes | `#[derive(Debug)]` | Not a code line |

**Implementation**:

File: `crates/reliary-pack/src/lib.rs`

1. Add these patterns to `is_noise_line()` (around line 2043):

```rust
fn is_noise_line(line: &str) -> bool {
    let trimmed = line.trim();
    // --- existing checks ---
    // ... docstrings, simple assignments, imports, bare returns ...

    // --- NEW: Python test assertions ---
    if trimmed.starts_with("self.") {
        return true;
    }
    // --- NEW: derive attributes ---
    if trimmed.starts_with("#[derive") || trimmed.starts_with("#[cfg") {
        return true;
    }
    // --- NEW: single-token statements ---
    if trimmed == "break;" || trimmed == "continue;" || trimmed == "continue" {
        return true;
    }
    // --- NEW: pattern: identifier assignment with no computation ---
    // "found = true;" — normal assignment, not surprising
    if let Some(eq_pos) = trimmed.find('=') {
        let rhs = trimmed[eq_pos+1..].trim().trim_end_matches(';');
        if rhs == "true" || rhs == "false" || rhs == "None" || rhs == "0" {
            return true;
        }
    }
    false
}
```

2. Change L3 fallback: if all pattern-based detectors find nothing AND `is_noise_line`
   returns true for the line, skip it entirely (no "unique line:" fallback).

```rust
// In extract_surprise_from_body, after the pattern detector loop:
if surprises.is_empty() && !body.is_empty() {
    for line in body.lines() {
        let trimmed = line.trim();
        if !is_noise_line(trimmed) && !trimmed.is_empty()
           && !trimmed.starts_with("//") && !trimmed.starts_with("#")
           && trimmed.len() > 15
        {
            // Only emit unique_line for genuinely unique lines
            let short = if trimmed.len() > 80 {
                format!("{}...", &trimmed[..77])
            } else {
                trimmed.to_string()
            };
            surprises.push(format!("unique line: {}", short));
        }
    }
    // Cap at 5 lines max
    surprises.truncate(5);
}
```

**Validation**:
- `cargo test -p reliary-pack` — all 11 existing tests pass
- Rebuild release binary, regenerate pack: compare `wc -l` before/after
- Expected: 5% pack size reduction, no accuracy impact (noise removal)

**Estimated pack size reduction**: ~5-8% (from ~60K tokens to ~55K)

---

### S2. Pack MCP Tool — "ask the pack"

**Problem**: The model accesses the pack via `grep` or `read` of a 50K-line file.
This is fragile (grep finds substring matches, not semantic ones) and expensive
(re-reads the full file every query).

**Fix**: Add a lightweight MCP tool `reliary_pack_query` that returns the pack entry
for a given function name. Uses the existing BM25 slicer internally.

**Implementation**:

File: `crates/reliary-agent/src/mcp.rs`

1. Register the tool:

```rust
static PACK_QUERY_TOOL: Tool = Tool {
    name: Cow::Borrowed("reliary_pack_query"),
    description: Cow::Borrowed(
        "Query the holographic pack for a specific symbol's entry. ",
        "Returns L0 (purpose), L2 (signature+location), L3 (surprise/behavior), and L4 (co-change callers). ",
        "Use this instead of grep/read when you want detailed behavioral context about a function. ",
        "Provide 'name' (the function/symbol name). Optionally provide 'context' (a description ",
        "of what you want to know — e.g., 'callers', 'surprise', 'behavior').",
    ),
    input_schema: json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "Symbol to query (e.g., 'skeleton', 'classify_line')"},
            "context": {"type": "string", "description": "What you want to know (optional): 'callers', 'surprise', 'signature', 'behavior'"}
        },
        "required": ["name"]
    }),
    ..
};
```

2. Implement the dispatch in `tool_reliary_pack_query(db, args)`:

```rust
fn tool_reliary_pack_query(db: &Connection, args: &Value) -> Result<Vec<ToolContent>, String> {
    let name = args["name"].as_str().unwrap_or("");
    let context = args["context"].as_str().unwrap_or("");

    // 1. Generate the pack if needed (cache on disk)
    let pack_path = find_project_root(db)?.join(".reliary/pack_l2l3.md");
    if !pack_path.exists() {
        let pack = reliary_pack::generate_pack(project_root, PackFormat::L2L3)?;
        std::fs::write(&pack_path, &pack).map_err(|e| format!("write pack: {}", e))?;
    }
    let pack = std::fs::read_to_string(&pack_path)?;

    // 2. Query the pack for this name
    // Use the existing BM25 slicer with the symbol name as query
    let query = format!("{} {}", name, context);
    let sliced = reliary_pack::slice_pack_for_query(&pack, &query, 3);

    // 3. If nothing found, try searching for the name in the raw pack
    if sliced.is_empty() {
        // Return the section containing this name
        let mut found = Vec::new();
        let mut in_section = false;
        for line in pack.lines() {
            if line.starts_with("## ") && line.contains(name) {
                in_section = true;
            } else if in_section && line.starts_with("## ") {
                break;
            }
            if in_section {
                found.push(line.to_string());
            }
        }
        if found.is_empty() {
            return Ok(vec![ToolContent::text("Not found in pack.")]);
        }
        return Ok(vec![ToolContent::text(&format!("```\n{}\n```", found.join("\n")))]); 
    }

    Ok(vec![ToolContent::text(&format!("```markdown\n{}\n```", sliced))])
}
```

**Validation**:
- Run `reliary pack_query --name skeleton` — returns the skeleton entry
- Run `reliary pack_query --name skeleton --context callers` — returns skeleton + cross-ref entries
- Test in opencode: model calls `reliary_pack_query(name="classify_line")` → gets relevant entry
- Measure: tool call time < 100ms (static file read + in-memory BM25)

Files to create/modify:
- `crates/reliary-agent/src/mcp.rs` — tool definition + dispatch
- `crates/reliary-agent/src/main.rs` — CLI integration (optional `pack_query` subcommand)

---

### S3. Integrate Warmup into Persistent Sessions

**Problem**: Angle 1 experiment proved warmup adds +12% accuracy (50 probes: 44% → 56%).
The persistent session sends the pack as a system prompt but doesn't force active reading.
The model processes context passively and misses facts.

**Fix**: On session start (turn 0), inject 3 generic warmup questions about the pack
entries most relevant to the user's task. The warmup answers are sent BEFORE the first
real question.

**Protocol**:

```
Turn 0:
  System: "You are a code intelligence agent. A holographic pack of this codebase
           is in your context. Before answering any questions, familiarize yourself
           by answering these questions:"
  User:    "<pack content>"
  User:    "Quick warmup — answer briefly:
            1. What does [function] return for empty input?
            2. What is a key surprise or edge case?
            3. Which other functions call [function]?"
  Assistant: (answers briefly)
  User:    (first real question)
```

If the user's task mentions a specific function (extracted via simple regex),
the warmup targets that function. Otherwise, the warmup targets the top hotspot.

**Implementation**:

File: `bench/test_persistent_session.py`

1. After session creation (`opencode run -s`), inject warmup exchange:

```python
def inject_warmup(session_id: str, task_text: str) -> None:
    """Inject warmup questions before the first real task."""
    # Extract function name from task
    import re
    fn_match = re.search(r'\b(skeleton|classify_line|should_drop|'
                          r'detect_strategy|aggressive_skeleton|'
                          r'skeleton_hash|compress_content|'
                          r'find_clusters|find_clusters_global|'
                          r'MaxwellGate)\b', task_text)
    func = fn_match.group(1) if fn_match else "skeleton"
    
    warmup = f"""Quick warmup — answer briefly:
1. What does {func} return for empty/blank input?
2. What is a key surprise or edge case in {func}?
3. Which other functions call or interact with {func}?"""
    
    # Send warmup as first user message
    subprocess.run([
        OPENCODE_BIN, "run", "-c", "-s", session_id,
        "--model", DEEPSEEK_MODEL, "--agent", "build",
        "--format", "json", warmup
    ], ...)
```

2. Gate the warmup on `RELIARY_PACK_WARMUP_ENABLED=1` env var (opt-in).

**Validation**:
- Run persistent session test with `RELIARY_PACK_WARMUP_ENABLED=1`
- Compare score vs baseline: expected +5-10% accuracy per question
- Measure: warmup adds 1 API call (turn 0 has 2 messages) — cost impact ~$0.0001

**Expected accuracy gain**: +5-10% (from 95% → ~98-99%)

---

### S4. 50-Turn Cache Stress Test

**Problem**: The 20-turn persistent session showed cache hit rate at ~97%. At 50+ turns,
the context window grows (messages accumulate from prior turns), the 1-hour cache TTL
approaches, and the model may experience degradation. We don't know the curve.

**Fix**: Run a 50-turn variant of the persistent session test.

**Protocol**:

```python
# test_persistent_session_50.py — copy of test_persistent_session.py with 50 tasks

# Tasks: extensions of the 20-task behavioral set + 30 new tasks covering:
# - Architecture explanation (5 tasks)
# - Cross-module trace (5 tasks)
# - Edge case detection (5 tasks)
# - Parameter extraction (5 tasks)
# - Caller analysis (5 tasks)
# - Sentiment/quality assessment (5 tasks)
```

**Metrics to capture per turn**:
- `cache_hit_tokens` (from usage)
- `cache_miss_tokens`
- `prompt_tokens`
- `completion_tokens`
- `wall_time`
- `tool_calls`
- `score`

**Analysis**:
- Plot cache hit rate vs turn number (should stay flat ~97% if stable)
- Plot cost/turn vs turn number (should decrease as amortization kicks in)
- Plot score/turn vs turn number (should stay flat if context isn't degrading)
- Detect inflection points where cache degrades or model quality drops

**Acceptance gate**:
- Cache hit rate stays > 90% through turn 50
- Cost/turn stays < $0.00050 (pack + warmup amortized)
- Score stays > 90% through turn 50

**Files to create**:
- `bench/test_persistent_session_50.py`
- `bench/tasks_50.json` — 50 behavioral tasks on reliary8
- `bench/results/session_50.jsonl`

---

### S5. Pack Regeneration End-to-End Test

**Problem**: The schema migration is fixed (reindex-file now populates lazy tables)
and `is_definition_like` accepts single-line defs. But the full flow — "edit file →
regen pack → model answers about the edited file" — has NEVER been tested end-to-end.
We don't know if the model reads the regenerated pack or falls back to the stale one.

**Protocol**:

```
Step 1: Start session with pack (build agent + reliary MCP + RELIARY_PACK_REGEN_ON_EDIT=1)
Step 2: Send task: "Create a new function in classify.rs:
         pub fn test_regen_behavior() -> &'static str { \"original\" }"
Step 3: Verify pack regen happened (check pack timestamp or file size changed)
Step 4: Send task: "What does test_regen_behavior return?"
         Model should answer: "original" (from pack)
Step 5: Edit the file: change return value to \"modified\"
Step 6: Verify pack regen happened
Step 7: Send task: "What does test_regen_behavior return now?"
         Model should answer: "modified" (from regenerated pack)
```

If the model answers "original" at Step 7, the pack regen is broken (model reads
stale pack from cache, not the regenerated one).

If the model answers "modified", the feature is proven working.

**Acceptance gate**:
- Step 4 and Step 7 both give correct answers
- Pack file modification time changed between Step 3 and Step 5
- No tool calls for either question (model answers from pack, not grep)

**Files to create**:
- `bench/test_regen_end_to_end.py` — replicable test script
- `bench/results/regen_e2e.jsonl`

---

## Tier 2 — Polish (5 items)

### P1. Async Pack Regen — Eliminate Edit Latency

**Problem**: `triggerPackRegen()` spawns `reliary pack` synchronously and blocks for ~1s.
On every `edit`/`write` tool call, the user waits 1s for the agent to continue.

**Fix**: Fire-and-forget async. Spawn the subprocess with `detached: true` (Node)
or `spawn(...).unawait()` (JS). The pack file is written to a temp file
(`pack_l2l3.md.tmp`), then atomically renamed (`fs.rename`) to `pack_l2l3.md`.
The rename is atomic on Linux — no partial reads.

**Race condition**: Two edits at the same time. Second regen overwrites first's temp file.
Mitigation: use a unique temp filename per spawn (`pack_l2l3.md.{pid}.tmp`) and rename
only if no newer temp file exists.

**Implementation**:

File: `crates/reliary-agent/pi/gate.js`

```javascript
let regenPid = 0;

function triggerPackRegen() {
    const pid = ++regenPid;
    const tmpFile = `${PACK_PATH}.${pid}.tmp`;
    const child = spawn(RELIARY_BIN, ['pack', projectRoot, '--format', 'l2l3', '--strategy', 'full', '--output', tmpFile], {
        detached: true,
        stdio: 'ignore',
        timeout: 30000,
    });
    child.on('exit', (code) => {
        if (code === 0 && pid === regenPid) {
            // Only apply if this is still the most recent regen
            fs.renameSync(tmpFile, PACK_PATH);
        } else {
            fs.unlinkSync(tmpFile); // stale, discard
        }
    });
    // Don't await — return immediately
}
```

**Validation**:
- Edit a file → `triggerPackRegen` returns in < 5ms
- ~1s later, pack file is updated atomically
- Test: 3 rapid edits → only the LAST edit's pack is applied

---

### P2. Pack Diff on Regen — "What Changed?"

**Problem**: After a pack regen, the model has no signal that the pack changed.
It doesn't know WHICH entries were updated, so it may answer from stale context
that happens to match the old pack.

**Fix**: After regen, compute a minimal diff between old and new pack.
Append a 2-3 line summary to the regenerated pack file:

```
## UPDATED {timestamp}
skeleton: L2 signature changed → returns Option<String> instead of String
test_regen_behavior: NEW symbol added
```

The diff compares `## <name>` sections between old and new pack. If an entry
is new or has changed L2/L3, log it.

**Implementation**: 

Add to `triggerPackRegen` in gate.js:

```javascript
function computePackDiff(oldPack, newPack) {
    const oldSections = new Map();
    const newSections = new Map();
    // Parse ## sections from both packs
    // ...
    const changes = [];
    for (const [name, newSection] of newSections) {
        if (!oldSections.has(name)) {
            changes.push(`${name}: NEW`);
        } else if (oldSections.get(name) !== newSection) {
            // Compare L2 line
            changes.push(`${name}: UPDATED`);
        }
    }
    return changes.slice(0, 5); // cap at 5
}
```

**Validation**:
- Regenerate pack after edit → pack ends with `## UPDATED` section
- Model sees changes and answers from NEW facts

**Cost**: ~200 bytes per regen (~50 tokens at cache-miss rate = $0.000007)

---

### P3. Configurable Pack Size

**Problem**: Full pack is 846 entries (~60K tokens). Hotspot pack (top-50) is ~5K tokens
but picks the wrong symbols for specific queries. User needs a knob.

**Fix**: Add `--max-entries N` to `reliary pack`. When set, caps the pack to the
top-N symbols by hotspot score.

```rust
// In generate_pack():
let mut entries: Vec<&Symbol> = symbols.iter().collect();
entries.sort_by(|a, b| {
    let sa = symbol_hotspot_score(&a.name, &symbol_sources, &cross_refs);
    let sb = symbol_hotspot_score(&b.name, &symbol_sources, &cross_refs);
    sb.partial_cmp(&sa).unwrap_or(Ordering::Equal)
});
entries.truncate(max_entries);

for sym in entries {
    // render as before
}
```

**CLI**: `reliary pack . --max-entries 100 → top-100 hotspot entries`

**Validation**: Test with `--max-entries 50` — pack has exactly 50 ## entries.
Hotspot score is ranked correctly (skeleton, classify_line, etc. at top).

---

### P4. Deterministic Contradiction Check (Scoring Fix)

**Problem**: Keyword scoring has unmeasured false positive/negative rate.
A 3/3 answer can be wrong (mentions keywords in wrong context). A 0/3 answer
can be correct (paraphrases without exact keywords).

**Fix**: Add a post-hoc contradiction check. After the model answers, extract
factual claims (numbers, return values, function names) and compare against
the pack's L3 facts. If the answer contradicts the pack explicitly, flag it.

```python
def check_contradictions(answer: str, pack_l3_facts: dict) -> list[str]:
    """Return list of contradictions between answer and pack facts."""
    contradictions = []
    for func, facts in pack_l3_facts.items():
        # Extract claims about this function from the answer
        if f"returns 0" in answer.lower() and "String::new()" in str(facts):
            # Model says returns 0, but L3 says returns String::new()
            if func.lower() in answer.lower():
                contradictions.append(f"{func}: answer says 'returns 0' but pack says 'String::new()'")
    return contradictions
```

**Scope**: Run on the 20 persistent-session answers. Count how many 3/3 answers
contain contradictions. If >10%, keyword scoring is unreliable.

**Acceptance gate**: contradiction rate < 5% for 3/3 answers.

---

### P5. Incremental Pack Loading in Sessions (F3 Revival)

**Problem**: The persistent session sends the full pack on turn 0 (25K tokens).
Subsequent turns don't re-send the pack — they rely on the model's KV cache.
But when a NEW symbol context is needed (adaptive slicing triggers), the model
receives it as a separate user message — no incremental loading.

The F3 experiment showed -78% tokens by only sending entries the model hasn't
seen yet. This was for single-query mode. For persistent sessions, the benefit
is smaller (the pack is already cached) but still valuable when slicing triggers.

**Fix**: In the persistent session, maintain a `seen_entries` set. When the
`reliary_pack_query` tool is called for a specific name, add it to the set.
On `reliary_pack` regeneration, only show newly-added symbols (diff).

**Implementation** (simplified):

File: `bench/test_persistent_session.py`

```python
seen_symbols = set()

def get_new_entries(full_pack: str, seen: set[str]) -> str:
    """Extract pack entries for symbols not yet seen."""
    entries = full_pack.split("## ")
    new_entries = []
    for entry in entries[1:]:  # skip header
        name = entry.split("\n")[0].split("/")[0].strip()
        if name and name not in seen:
            new_entries.append(f"## {entry}")
            seen.add(name)
    return "\n".join(new_entries)
```

**Validation**:
- Turn 0: full pack sent (25K tokens) — all symbols added to seen set
- Turn 3: adaptive slicing triggers — only NEW symbol entries sent (200-500 tokens)
- Total tokens across 20 turns drops ~50% vs current

**Expected token savings**: 40-60% across 20 turns with slicing active

---

## Tier 3 — Synergies (5 items)

### Y1. Pack + Warmup + Confidence Check Pipeline

**Combo**: Three-stage pipeline: pre-load (pack) → engage (warmup) → verify (check).
The confidence check (Angle 2, +31%) asks the model to reconsider when facts are
missing. Combined with warmup (+12%) and pack (baseline 90%), expected: ~95-98%
accuracy in persistent sessions.

**Integration**:
1. Turn 0: load pack (25K tokens, co-shared in session)
2. Turn 0.5: warmup (3 generic questions, forces active reading)
3. Turns 1-N: real questions, auto-trigger confidence check when score < 2/3
4. Confidence check: "Your answer didn't mention [L3 facts]. Reconsider."

**Cost**: +1-2 API calls per low-confidence answer. On 20-turn session: ~5 extra calls.
Total: $0.00934 + $0.002 = $0.011 (+20% cost, +3-5% accuracy)

---

### Y2. Pack + Callgraph for Accurate Cross-Refs

The current cross-refs use `COUNT(occurrence WHERE is_def=0)` — approximate.
`callgraph_v2` returns exact callers/callees from brace-delimited body extraction.
Replace the COUNT proxy with callgraph data.

**Implementation**:
- Add a `build_cross_refs_from_callgraph(db, symbols)` function that queries
  the `callgraph` table for each symbol
- Fall back to COUNT proxy for symbols not in the callgraph table

**Expected**: 10-20% more accurate cross-refs (no false positives from comments/docs),
fewer dead-end tool calls when model verifies cross-refs

---

### Y3. Pack Diff + Adaptive Slicing

After an edit, compute the pack diff (P2) and slice only the changed entries (P5).
The model receives: "2 entries changed: skeleton' now returns Option<String>;
test_regen_behavior is a new function returning &str."

**Token cost per edit**: ~200 tokens (diff summary) + ~500 tokens (2 entry slices).
vs full pack re-send: ~25K tokens. Savings: 98%.

---

### Y4. Pack as MCP Tool + System Prompt (Layered Access)

The pack is injected as a system prompt (passive read). The MCP tool (S2)
provides active query access. Together: layered information access.

- System prompt: "here's the codebase — read it" (passive, always available)
- MCP tool: "query for specifics about skeleton" (active, on-demand)

The model gains a second channel for when the passive pack doesn't answer the question.
Without the tool: model greps the pack (fragile). With the tool: model gets exact entry.

**Expected**: 20-30% reduction in `grep`/`read` tool calls about the pack itself

---

### Y5. Session Cache for Non-Pack Tool Results

The F3 experiment cached sliced entries across turns. Extend this to non-pack
tool results: when the model calls `find_references("skeleton")` and gets results,
cache the results. If it asks the same question again, return cached results.

Already partially built (tool_result_cache in `unseen_session_bench.py`).
Never integrated into persistent sessions.

**Implementation**: Add a `tool_cache: dict[str, str]` to the persistent session runner.
Key: `(tool_name, json.dumps(args))`. Value: tool result text.

**Expected**: 5-10% fewer redundant tool calls in long sessions

---

## Execution Order (Gated)

Each phase depends on the previous. Gates prevent wasted work.

```
S1 (L3 noise) ──────────────┐
S2 (MCP tool) ──────────────┤
                              ├── S5 (Regen E2E) ──┐
S3 (Warmup)   ──────────────┘                      │
S4 (50-turn)  ────────────── (parallel, independent) │
                                                      ├── SHIP
P1 (Async regen) ───────────┐                        │
P2 (Diff on regen) ─────────┤                        │
                              ├── P5 (Incremental) ──┘
P3 (Configurable size) ─────┤
P4 (Contradiction check) ───┘

Y1-Y5 — after ship, before v2.
```

**Ship gate**: S1+S2+S3+S4+S5 all pass → pack is polished → ship to users.
**v2 gate**: P1-P5 all pass → pack is efficient → ship v2.
**v3 nice-to-have**: Y1-Y5 → pack is comprehensive.

---

## File Manifest

| File | Tier | New/Modify | Purpose |
|---|---|---|---|
| `crates/reliary-pack/src/lib.rs` | S1, P3 | Modify | L3 noise filter, max_entries |
| `crates/reliary-agent/src/mcp.rs` | S2 | Modify | pack_query tool definition + dispatch |
| `crates/reliary-agent/pi/gate.js` | P1, P2 | Modify | Async regen, diff computation |
| `bench/test_persistent_session.py` | S3, S4, S5, P5 | Modify | Warmup, 50-turn, e2e test, incremental |
| `bench/test_persistent_session_50.py` | S4 | New | 50-turn variant |
| `bench/test_regen_end_to_end.py` | S5 | New | Regen e2e test |
| `bench/tasks_50.json` | S4 | New | 50 behavioral tasks |
| `bench/results/session_50.jsonl` | S4 | New | Results |
| `bench/results/regen_e2e.jsonl` | S5 | New | Results |
| `bench/results/contradiction_check.jsonl` | P4 | New | Results |

**Total new files**: 5
**Total files modified**: 4

---

## API Call Budget

| Tier | Calls | ~Cost (v4-flash) | ~Wall Time |
|---|---|---|---|
| S1 | 0 (offline) | $0 | 0h |
| S2 | 5-10 (manual testing) | $0.01 | 0.5h |
| S3 | 20 (persistent session) | $0.02 | 0.5h |
| S4 | 50 (persistent session) | $0.05 | 1h |
| S5 | 10 (e2e test) | $0.01 | 0.5h |
| P1-P4 | 0-20 (primarily offline) | $0.02 | 0.5h |
| P5 | 20 (persistent session) | $0.02 | 0.5h |
| Y1-Y5 | 30-50 (post-ship) | $0.05 | 2h |
| **Total** | **~150** | **~$0.18** | **~5.5h** |

---

## Open Decisions

These should be decided before execution begins:

1. **S2: Should pack_query be a PRIMARY tool?** If yes, the model will prefer it over
   grep/read for pack queries. If no, the model may never discover it. RECOMMENDATION:
   mark it as PRIMARY but not as the DEFAULT for `find_references` — it's a complement,
   not a replacement.

2. **S3: Should warmup be always-on or opt-in?** Always-on costs +5 API calls per session
   but delivers +12% accuracy. RECOMMENDATION: always-on when RELIARY_PACK_REGEN_ON_EDIT=1
   (the pack is already opted in). Gate on the same env var.

3. **S4: Should the 50-turn test use the same 20 tasks (repeated 2.5x) or unique tasks?**
   REPEATED tests cache stability. UNIQUE tests coverage. RECOMMENDATION: 20 unique behavioral
   tasks + 30 new tasks of different types (architecture, trace, edge, param, caller, quality).

4. **P5: Does incremental loading work in persistent sessions, or does the model's KV cache
   already handle this?** RECOMMENDATION: test before building. If the KV cache already keeps
   the full pack, incremental adds no value. If the KV cache degrades at scale, incremental is
   essential.

---

## Version History

| Version | Date | Author | Changes |
|---|---|---|---|
| 1.0 | 2026-07-09 | Builder | Initial plan — 15 items across 3 tiers + 5 synergies |
