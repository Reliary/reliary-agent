# Synthetic vs Real Agent: The Cost Story

## The User's Question

> "Does our harness use the real pi agent to get work done? Or is it completely synthetic work with real LLM calls?"

## The Answer

**The harness is 100% synthetic.** It does NOT use the real Pi agent.

`bench/unseen_session_bench.py` does:
1. Calls the DeepSeek API directly via `deepseek_chat()`
2. Manually constructs the messages array
3. Manually parses tool calls from JSON
4. Manually executes tools via the MCP subprocess
5. Manually formats prompts and tool results

The real Pi agent was used ONLY in the earlier `compare_three.py` competitor benchmark, and that benchmark was so problematic (reliary timing out 9/10 times) that we abandoned it.

## Why Synthetic Was Used

The synthetic harness was the right choice for the holographic pack work because:
1. **Control over pack injection** — Pi doesn't support custom system prompts
2. **Deterministic scoring** — the same inputs produce the same outputs
3. **Reliable cost measurement** — direct API token counts, not Pi's session reconstruction
4. **The real Pi agent's reliary integration was broken** — 9/10 tasks timed out

## What the Synthetic Harness Misses

Real agents (Pi, Claude Code, opencode, Cursor) have:
- **Richer tool formatters** — Pi formats tool calls differently than our manual JSON parser
- **Built-in cache optimization** — Pi may use prefix caching more aggressively
- **Different prompt overhead** — Pi adds its own system instructions
- **Session continuity** — Pi carries context across tool calls in ways our manual harness doesn't
- **Rate limiting / retry logic** — Pi handles transient errors differently

## What This Means for Our Cost Numbers

| Source | What it measures | Accuracy | Cost |
|---|---|---|---|
| `unseen_session_bench.py` (old, accumulated) | Synthetic, single-session, accumulated | 72.5% (F) | $0.023 |
| `unseen_session_bench.py` (new, reset) | Synthetic, single-shot, no carryover | 55.0% (F) | $0.011 |
| `compare_three.py` (real Pi, condition A) | Real Pi agent with reliary | 0% (timeouts) | $0 |
| `compare_three.py` (real Pi, condition B) | Real Pi agent with altbackend | varies | $0 (B is fast/cheap) |

**The "real Pi agent" data is sparse and unreliable.** The token counts in `compare_three.py` only capture the final message's usage, not the full session. The reliary condition times out 9/10 times in the real agent, so we have no clean real-agent numbers for the pack.

## What We Can Say Honestly

1. **In synthetic single-shot**: the pack costs $0.011/query, gives 55% accuracy
2. **In synthetic accumulated**: the pack costs $0.023/query, gives 73% accuracy
3. **In real Pi agent with broken reliary**: we have no clean data
4. **In real Claude Code / Cursor / opencode**: we have NO data

The pack's value in real production agents is **unproven** in this experiment. The synthetic harness shows it works in controlled conditions. Whether it works in a real coding agent depends on:
- How the agent handles the pack injection
- Whether the agent's tool execution is fast enough to not time out
- Whether the model's accuracy transfers from synthetic to real use

## What Would Be Needed to Verify

To prove the pack works in real production:
1. Wire `reliary pack` into a real coding agent (Pi, Claude Code, opencode)
2. Run a real coding session (not a probe) with the pack injected
3. Measure: did the agent complete the task faster/with fewer tool calls than without?
4. Compare against a control session without the pack

This was not done. The synthetic harness was used because the real agent was broken.

## The Bottom Line

**Our cost numbers are from a synthetic harness, not a real agent.** The technique works in synthetic. Whether it works in real production is unproven. The numbers are a lower bound on accuracy (no real-world noise) and a rough estimate on cost (real agents may have different overhead).

If you want to ship the pack, the next step is integration with a real coding agent, not more synthetic benchmarks.
