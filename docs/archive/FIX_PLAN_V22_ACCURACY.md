# FIX_PLAN_V22_ACCURACY.md
# Reliary8 Accuracy Gap — Root Cause + Comprehensive Fix Plan
#
# Created: After LLM-as-judge (deepseek-v4-pro) revealed reliary scores 6/30
# on accuracy vs altbackend 13.5/30 and grep 13.0/30.
#
# Root cause: heuristic guards silently skip data, producing 0 results with
# no error or log. The keyword scorer masked this for 15+ sessions.
#
## THE RECURRING PATTERN

Every accuracy bug we've found follows the same structure:

  Guard rejects data → 0 results → no error → no log → tool returns "empty"
  → LLM gets wrong answer → judge scores 0 → we never noticed because
  keyword scorer gave credit for mentioning the symbol name

| Guard | What it skips | Effect | Discovered |
|-------|--------------|--------|------------|
| is_source_like ratio < 0.20 | Doc-heavy .rs files (runtime/runtime.rs) | 0 occurrence rows | Judge bench |
| phrase_id_for stem-only lookup | Unstemmed phrase_id (consume vs consum) | 0 search results | Judge bench |
| flush_occurrence_batch params | All batch INSERTs > 50 rows | 0 occurrence rows | SWE-bench |
| COMMIT .ok() | Transaction rollback | Stale/empty data | SWE-bench |
| build_all_occurrence wrong query | Non-existent file_id column | 0 occurrence globally | SWE-bench |
| scan_identifiers min 3 chars | 2-char identifiers (re, os, id) | ~15% missing tokens | SWE-bench |
| is_pascal_case _ treated as upper | __init__ misclassified | Wrong def tag | Python detection |

## WHY THE PACK ISN'T BEING SLICED

We have `reliary_pack_query(name)` which returns a focused ~2k char slice for
ONE symbol. But the system prompt says "Call reliary_pack FIRST" which dumps
700k chars. The model can't navigate 700k chars in one turn.

The fix: change the system prompt to prefer `pack_query` over `pack`.

## PHASE 1: Fix is_source_like + add guard logging (1 hour)

### 1.1 Lower is_source_like threshold

File: crates/reliary-search/src/lazy_occurrence.rs

Current: ratio >= 0.20 (rejects files with < 20% code lines)
Problem: runtime/runtime.rs has 17% code lines (83% doc comments)
Fix: ratio >= 0.10

Rationale: A file with 10% code lines IS a source file — it just has lots
of documentation. Pure text/config files (README, .toml, .json) have < 5%
code lines and will still be rejected.

### 1.2 Add guard logging to ALL heuristic guards

Every guard that silently skips data should log to stderr when it fires.
This is the "fail informatively" pattern applied to guards, not just errors.

Guards to instrument:

1. is_source_like → eprintln when rejected: file path + ratio
   File: lazy_occurrence.rs

2. phrase_id_for → eprintln when stem miss: name + stem + fallback tried
   File: symbol.rs

3. scan_identifiers → eprintln when filtered: count of filtered tokens
   File: ingest.rs (only log if > 50% filtered, to avoid spam)

4. is_source_like in ingest.rs (same as #1, different call site)
   File: ingest.rs

5. has_blocks / has_occurrence guards → already silent, add eprintln
   File: lazy_occurrence.rs, lazy_tables.rs

Format: [guard:name] detail...

Example: [guard:is_source_like] skipped runtime/runtime.rs (ratio=0.17 < 0.10)

### 1.3 Re-index tokio

After threshold change, rebuild:
  cd /tmp/tokio-corpus/tokio/src
  reliary trust .
  reliary build-all .

Verify:
  - runtime/runtime.rs has occurrence rows > 0
  - find_references("block_on") returns runtime/runtime.rs:340
  - find_references("consume") returns all 6 types in io/util/

## PHASE 2: Fix pack slicing (2 hours)

### 2.1 Change system prompt

File: bench/multi_turn_harness.py — RELIARY_SYS

Current: "Call reliary_pack FIRST to get the codebase overview."
Problem: dumps 700k chars, model can't navigate

New: "Use reliary_pack_query(name) for a specific symbol's context (~2k chars).
Use reliary_search(query) to find files by topic.
Do NOT call reliary_pack — it returns too much data.
Use reliary_goto_def(name) to find where a symbol is defined.
Use reliary_find_references_with_source(name) to find all usages."

### 2.2 Remove pre-generated pack from SWE-bench harness

File: bench/swe_bench_lite.py

The harness pre-generates a pack and saves it to a file. Remove this —
the model should call pack_query on demand, not receive a pre-dumped pack.

### 2.3 Verify pack_query returns focused slices

Test:
  reliary_pack_query("Runtime::block_on") → should return ~2k chars
  with block_on's definition, callees, callers, and file location.

  reliary_pack_query("consume") → should return ~2k chars
  with all types that implement consume.

### 2.4 Update AGENTS.md tool guide

File: AGENTS.md

Add pack_query as the PRIMARY tool for "understand a symbol":
"reliary_pack_query(name) — returns the symbol's definition, callers,
callees, file location, and purpose in ~2k chars. Use this INSTEAD of
reliary_pack (which returns 700k chars)."

## PHASE 3: Fix remaining accuracy gaps (2 hours)

### 3.1 q3 (block_on def) — fixed by Phase 1

After is_source_like threshold change, runtime/runtime.rs will be indexed.
find_references("block_on") should return runtime/runtime.rs:340 as the
top definition (tag=1, is_def=1).

Verify: the result should rank runtime/runtime.rs ABOVE future/block_on.rs.
If not, add a ranking boost for files matching the type hint
(e.g., "Runtime" → files in runtime/ directory).

### 3.2 q4 (call chain) — re-enable multi-hop

File: crates/reliary-search/src/callgraph_v2.rs

Re-enable delegate_depth("block_on") = 3 (was reverted to 1 for token cost).
Accuracy matters more than cost now.

Also enable for: "run", "start", "execute", "spawn", "poll".

The multi-hop expansion follows the delegation chain:
  block_on → block_on_inner → scheduler dispatch → schedule/wake/push/queue

This should give the model enough callees to trace the chain.

### 3.3 q9 (consume impls) — add path_filter to find_references

File: crates/reliary-agent/src/mcp.rs

Add optional `path_filter` parameter to find_references and
find_references_with_source. When provided, filter results to
file_path LIKE 'path_filter%'.

This lets the model call:
  find_references("consume", path_filter="io/util/")
  → returns only Take, Empty, BufWriter, BufStream, Chain
  → excludes BufReader (io/util/buf_reader.rs IS in io/util/, but
    BufReader doesn't implement consume — it uses it)

Actually, BufReader IS in io/util/ and DOES have consume. The judge's
ground truth says BufReader is wrong, but our tool is correct. The judge
may need updating. Let me check:

The judge ground truth says: "Take, BufStream, BufWriter, Empty, Chain"
BufReader has consume at buf_reader.rs:140. This IS a consume impl.
The judge may be wrong — BufReader DOES implement consume (it's
AsyncBufRead + BufRead). We should accept BufReader as correct.

If the judge penalizes us for including BufReader, that's a judge issue,
not a tool issue. But we should still add path_filter for the model to
narrow results.

### 3.4 q5 (spawn callers) — verify test penalty

File: crates/reliary-search/src/symbol.rs

The H1 test penalty (added in V15) boosts production code over test code
by checking if file_path contains "test" as a path segment (not substring).

Verify: find_references("spawn") should rank Handle::spawn (runtime/handle.rs)
above std::thread::spawn in test files.

If the penalty isn't working, investigate why.

### 3.5 q8 (bufwriter write) — verify find_references("poll_write")

The judge says the model describes "write" instead of "poll_write".
This may be a model issue (confusing the method name) rather than a tool
issue. But verify that find_references("poll_write") returns
buf_writer.rs as a top result.

## PHASE 4: Re-run fair 3-way bench with judge (1 hour)

### 4.1 Run bench

  python3 bench/long_session_bench.py --seeds 42 17 --conditions A,B,C

### 4.2 Run judge

  python3 bench/llm_judge.py --input bench/results/long_session_XXXXX.jsonl

### 4.3 Compare

| Backend | Current judge | Target |
|---------|--------------|--------|
| A (reliary) | 6.0 | ≥ 13.5 (beat altbackend) |
| B (altbackend) | 13.5 | 13.5 (no change expected) |
| C (grep) | 13.0 | 13.0 (no change expected) |

### 4.4 If reliary doesn't beat altbackend

Investigate remaining gaps per-query:
  - Which queries still score 0?
  - What does the tool return for those queries?
  - Is it a tool issue or a model interpretation issue?

## PHASE 5: Meta-fix to prevent recurrence (30 min)

### 5.1 Add guard logging (already in Phase 1.2)

### 5.2 Add CI check for occurrence table completeness

After every `trust`, verify:
  - Every file in file_map has at least 1 occurrence row
  - If not, log which files have 0 occurrence (these are the accuracy gaps)

This catches the is_source_like / JIT / build_all_occurrence class of bugs
automatically.

### 5.3 Document the guard philosophy in AGENTS.md

Add to AGENTS.md:
"Guard philosophy: every heuristic guard that skips data MUST log to
stderr when it fires. Guards that silently skip data cause silent
accuracy degradation that is invisible until measured with an LLM judge."

## ESTIMATED TOTAL: ~6 hours

| Phase | Hours |
|-------|-------|
| 1 (is_source_like + guard logging) | 1 |
| 2 (pack slicing) | 2 |
| 3 (accuracy gaps) | 2 |
| 4 (bench + judge) | 1 |
| 5 (meta-fix) | 0.5 |
| Total | 6.5 |

## VERIFICATION CHECKLIST

After all phases:

- [ ] is_source_like threshold = 0.10
- [ ] runtime/runtime.rs has > 0 occurrence rows
- [ ] find_references("block_on") returns runtime/runtime.rs:340
- [ ] find_references("consume") returns all 6+ types
- [ ] All guards log to stderr when they skip data
- [ ] System prompt prefers pack_query over pack
- [ ] delegate_depth("block_on") = 3
- [ ] path_filter parameter works in find_references
- [ ] Test penalty verified for spawn
- [ ] Reliary judge score ≥ 13.5/30
- [ ] No regression in token cost (billed < 30k)
- [ ] No regression in cache hit rate (> 93%)