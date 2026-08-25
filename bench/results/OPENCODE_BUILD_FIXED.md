# opencode Build Agent — Fixed Results

## Bugs fixed

1. **stdout truncation** (was `[-3000:]`, now full) — unblocked tool call and token counting
2. **Full 244K pack → sliced 1.3-2.5K pack** — matches what was validated in synthetic harness
3. **N condition**: added `--pure` flag — disables native tools properly
4. **Slice fallback**: empty slice now falls back to raw query text

## Results (5 tasks, 3 conditions, direct deepseek-v4-flash)

| Condition | Score | Tools/task | Pack | Notes |
|---|---|---|---|---|
| N (no tools, --pure) | 8/15 (53%) | 0 | 0 | Model from training only |
| A (basic tools) | 12/15 (80%) | 3.0 | 0 | grep + read |
| **F** (sliced pack + reliary MCP) | **9/15 (60%)** | 1.4 | 1.3-2.5K | Pack + tools |

## Per-task breakdown

| Task | N | A | F | F pack |
|---|---|---|---|---|
| find_skeleton_def | 3/3 | 3/3 | 3/3 | 1.6K |
| find_skelhash_callers | 1/3 | 3/3 | 0/3 | 2.5K |
| find_maxwell | 1/3 | 3/3 | 3/3 | 1.4K |
| find_linetype | 2/3 | 0/3 | 3/3 | 1.8K |
| find_skeleton_behavior | 1/3 | 3/3 | 0/3 | 2.3K |

## The honest picture

**A (80%) > F (60%) > N (53%)**

- A wins because grep + read is fast and reliable for these simple "find X" tasks
- F scores lower because the reliary tools return results the model doesn't trust (it re-verifies with grep anyway, doubling work)
- N is the baseline: model knowledge alone gets 53%

**The pack + reliary doesn't help on these tasks.** The model can find code faster with grep than with the reliary MCP. The pack adds 1-2K tokens of context but the reliary tool calls add latency and confusion.

## Why F loses on specific tasks

| Task | Why F scored 0 |
|---|---|
| find_skelhash_callers | Model called `find_references_with_source` and got confused between `skeleton_hash` and `aggressive_skeleton_hash` |
| find_skeleton_behavior | Model used `reliary_search` which returned wrong hits; then used `grep` to re-verify |

## Why F wins on specific tasks

| Task | Why F scored 3 |
|---|---|
| find_maxwell | Pack pre-loaded MaxwellGate info; model read the source directly |
| find_linetype | Pack pre-loaded LineType variants; model answered from pack |

## Token counts

F uses **16K input tokens** on most queries (sliced pack + system prompt). A uses **500-36K** (depends on grep/read results). N uses **100** (just the question).

## The conclusion

On these 5 simple coding tasks on reliary8:
- **Basic tools (grep + read) are the fastest and most accurate** (80%)
- **The pack + reliary MCP is slower and less accurate** (60%)
- **No tools** gives 53% (model training only)

The holographic pack doesn't add value when the model has basic file tools (grep + read). The reliary MCP adds overhead without accuracy gain.

## What this means

The pack's value is NOT in replacing grep+read. The pack's value (proven earlier in the synthetic harness) is in **reducing tool calls** — from 3.0/task to 1.4/task. But the accuracy dropped because the reliary tools return different (less direct) results than grep+read.

The right comparison would be: F (pack + bash/grep/read, no reliary MCP) vs A (bash/grep/read only). That would isolate the pack's contribution from the reliary MCP's interference.

Full data: `bench/results/opencode_build_test.jsonl` (15 runs, full stdout)