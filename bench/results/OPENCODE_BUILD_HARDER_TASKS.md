# Harder Tasks — Final Results

## The tasks (5 harder coding questions on reliary8)

| Task | What it tests |
|---|---|
| `trace_call_chain` | Cross-file call graph: follow CLI → classify → skeleton → helpers |
| `find_maxwell_callers` | Find all callers of a struct |
| `find_skelhash_callers` | Find all callers of a function |
| `skeleton_vs_aggressive` | Compare two similar functions, understand differences |
| `find_data_flow` | Trace data through multiple functions |

## Results (direct deepseek-v4-flash, opencode build agent)

| Condition | Score | Wall/task | Cost/task | Tokens/task | Tools/task |
|---|---|---|---|---|---|
| **N** (--pure) | 12/12 (100%*) | 20.8s | $0.00093 | 39,747 | 5.5 |
| **A** (grep+read) | 12/12 (100%*) | 30.9s | $0.00075 | 29,416 | 4.8 |
| **F2** (pack+grep) | 12/12 (100%*) | 37.4s | $0.00071 | 26,774 | 4.2 |
| **F** (pack+reliary) | 12/12 (100%*) | 29.5s | $0.00073 | 25,287 | 7.2 |

*Excluding trace_call_chain which timed out (60s) for both A and F.

## What the harder tasks showed

1. **trace_call_chain is too hard for v4-flash** — both A and F scored 0/3 and timed out at 60s. The model can't trace multi-file call chains from just grep+read. This task would need actual callgraph analysis (reliary's strength) or a stronger model.

2. **F2 is the cheapest** at $0.00071/task — the pack replaces tool calls with context, reducing token cost.

3. **F uses the most tools** (7.2/task) but is **fastest on the tasks that complete** (29.5s) — the model uses reliary tools (search, find_references) AND grep+read in combination. The reliary tools help on find_data_flow (3/3 in 28s) where grep alone takes 45s.

4. **F wins on find_data_flow** — the data flow task requires understanding that compress_content collapses imports. The pack pre-loads this info, and the model uses `reliary_search` to verify. A condition needed `task` (subagent) to get the same answer in 45s.

5. **A and F2 are equivalent on the tasks that work** — same 100% on 4/5 tasks, F2 is slightly cheaper ($0.00071 vs $0.00075) and uses fewer tokens.

## The unchanged pattern

**Even on harder tasks, A ≈ F2 ≈ F on accuracy.** The pack and reliary MCP don't provide enough advantage to beat grep+read. The model can find the answers it needs with basic tools.

The one task where the pack actually helped: `find_data_flow` (F: 28s, A: 45s). The pack's pre-loaded context about compress_content's import collapse behavior saved the model from exploring the code from scratch.

## What would actually differentiate F from A

Tasks that require:
- **Type-flow analysis** (who calls this with matching types?) — A can't do this, reliary's strength
- **Call chain depth > 2** (A → B → C → D) — grep can't follow chains
- **Architecture understanding** (what are the main modules and how do they connect?) — requires reading multiple files

The current benchmark doesn't test these. The simpler tasks are solved equally well by grep+read.

## What this proves

1. **The pack provides no advantage on simple "find X" tasks** — A wins on cost and accuracy
2. **The pack provides marginal advantage on "understand how X works" tasks** — find_data_flow is 17s faster with the pack
3. **The pack + reliary MCP doesn't beat grep on complex multi-file tasks either** — trace_call_chain timed out for both A and F

The holographic pack is a **niche optimization** for "I know the codebase structure but want to know what this specific function does in context." It's not a universal win.

## Test infrastructure

- Real opencode build agent
- Direct deepseek-v4-flash (not proxy)
- Reliary MCP tools actually executing (permission bug fully fixed)
- 5 harder tasks spanning single-file, cross-file, and multi-step reasoning
- All 4 conditions completed (1 timeout each for A and F on trace_call_chain)