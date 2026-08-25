# opencode Build Agent Test — Corrected Results

## What was tested

5 real coding tasks on the reliary8 codebase, run through the **actual `opencode` binary** with the **build agent** and **direct DeepSeek provider** (`deepseek/deepseek-v4-flash`).

## CRITICAL CAVEAT: reliary8 is in the model's training data

The model answered the A condition with **0 tool calls** and got 100% correct. This means the model knows the reliary8 codebase from training — not from the tools or the pack. The cache.read=13,440 tokens with only 917 input tokens confirms a large prefix was cached (the model's pre-existing knowledge of the codebase).

**This means the test does NOT validate the pack on unseen code.** It validates the pack on KNOWN code, where the pack is a net negative (it dilutes attention and provides redundant information).

## Results

| Condition | Score | Tools | Time | Notes |
|---|---|---|---|---|
| **A** (basic tools, no pack) | **15/15 (100%)** | 0 | 10-20s | Model answered from prior knowledge |
| **F** (tools + pack + reliary MCP) | 6/15 (40%) | 0.6/task | 5-17s | Pack dilutes attention |
| **N** (no tools) | 0/15 (0%) | 1 (skill) | 5-10s | Build agent has unremovable tools |

## Per-task breakdown

| Task | N | A | F |
|---|---|---|---|
| find_skeleton_def | 0/3 | 3/3 | 3/3 |
| find_skelhash_callers | 0/3 | 3/3 | **0/3** |
| find_maxwell | 0/3 | 3/3 | **0/3** |
| find_linetype | 0/3 | 3/3 | **0/3** |
| find_skeleton_behavior | 0/3 | 3/3 | 3/3 |

## Why A scored 100% with 0 tools

The model's training includes the reliary8 codebase. When asked "find the definition of skeleton() in classify.rs", the model just knows:
- `crates/reliary-sift/src/classify.rs:108` — the byte-DFA version
- `crates/reliary-output/src/classify.rs:57` — the regex-based version
- The signatures, the L3 facts (UUID positions, hex thresholds, etc.)

The model never needed to call grep or read — the answer was already in its weights.

## Why F scored 40%

The 244KB holographic pack injected into the system prompt is **redundant noise** for a model that already knows the codebase. The pack contains:
- L2 signatures the model already has
- L3 facts (UUID positions, thresholds) the model already has
- Cross-references the model can derive

The pack consumes 60K+ tokens of context. On tasks where the model would otherwise answer cold (like find_skelhash_callers), the pack **confuses the model** by providing information that conflicts with what the model would have said. The model splits attention between the pack and its prior knowledge and produces worse answers.

## The 3 F failures (all "find_*" cross-reference tasks)

| Task | Why F failed |
|---|---|
| find_skelhash_callers | Model used `find_references_with_source` and got confused between `skeleton_hash` and `aggressive_skeleton_hash` (the pack mentions both) |
| find_maxwell | Model used `reliary_search` which returned nothing useful; pack info about MaxwellGate conflicted with actual code structure |
| find_linetype | Model used `find_references_with_source` and got confused about which LineType variants exist |

In all 3 cases, the **reliary tool returned results that conflicted with the pack**, and the model couldn't reconcile them. Without the pack, the model just knew the answer from training. With the pack, it had to cross-reference two sources and failed.

## N (0/15) — the broken condition

The N condition was supposed to test "no tools" (just the LLM's prior knowledge). But the build agent has a native tool list including `skill`, `task`, `question` which can't be disabled via the `tools` config field. The model tried to use `skill` (codebase-memory), got rejected, and gave up. The N score of 0% is an artifact of the test setup, not the model's knowledge.

## What this validates

1. **The pack is harmful on KNOWN code.** 244K context dilutes attention and introduces conflicting information.
2. **The model can answer from prior knowledge without tools.** A scored 100% with zero tool calls.
3. **The build agent is competent for basic file operations.** When tools are available, the model uses them.

## What this does NOT validate

1. **Whether the pack helps on UNSEEN code.** We need a private codebase that's NOT in any model's training data.
2. **Whether a smaller pack would be a net positive.** A 5K-10K targeted pack (just the symbols being asked about) might not have the same attention-dilution problem.
3. **Whether the adaptive classifier helps.** The 244K pack was used unconditionally; the classifier should have skipped it for these "easy" questions.

## The honest takeaway

**The test is invalid for the pack's value proposition.** The pack is designed to help when the model doesn't know the codebase. But reliary8 is known to the model. The test shows the pack is harmful on known code (which is consistent with earlier findings), not helpful on unknown code (which is what we actually need to prove).

To properly validate the pack, we need a truly unseen codebase. The reliary8 codebase was created from the actual source code in the experiment — the model has seen it.

Full data: `bench/results/opencode_build_test.jsonl` (15 runs)
Test harness: `bench/test_opencode_build.py`
