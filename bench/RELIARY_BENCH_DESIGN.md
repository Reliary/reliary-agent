# reliary benchmark — design

Goal: a benchmark that **works on the first run, every run**, with no "I can't find the
API key" messing around. Measures the things that matter for the reliary vision
(fewer tokens, safer, less hallucination) and supports the Step-2 gate
("proxy sole-owner must hold the proven -X% weighted-cost savings").

---

## 1. What we are NOT measuring (and why)

- **"Did the LLM fix the bug?"** — LLM success rate is dominated by 2.7x irreducible
  provider variance and model blind spots (memory id 3294). It is noise as a benchmark
  axis. We record it for context, but the gate is on COST, not pass/fail.
- **Single-turn cost.** Memory id 3334: compression only fires on multi-turn sessions with
  accumulated conversation. A 1-turn bench measures nothing. Min 6 turns.
- **Char counts.** Memory id 3920: char/4 overstates real tokens (`§` is 2-3 tokens). We
  use the provider's own `prompt_tokens` / `completion_tokens` from the usage object.

## 2. The metrics (per trial, per turn, cumulative)

### Primary (the gate axis)
| Metric | Definition | Source |
|--------|------------|--------|
| `input_tokens` | sum of `usage.prompt_tokens` across turns | proxy `/tmp/reliary_proxy.jsonl` (proxied) or direct API call (baseline) |
| `output_tokens` | sum of `usage.completion_tokens` | same |
| `weighted_cost` (WC) | `input + 4 × output` | computed — output tokens cost ~2-4x more and are generated linearly (memory USER_DIRECTIVES) |
| `wc_savings_pct` | `1 - WC_proxied / WC_baseline` | computed |

### Secondary (diagnostic — explain WHY savings did/didn't happen)
| Metric | Why |
|--------|-----|
| `turns` | session length; <6 turns = no signal (id 3334) |
| `wall_time_s` | proxy adds latency (KNOWN +10%); must stay acceptable |
| `compression_fired` | count of assistant-reasoning compressions the proxy did |
| `kv_cache_hit_estimate` | if input_tokens drops sharply turn N→N+1, KV cache held (good) |
| `first_appearance_freeze_hits` | how often frozen compressed bytes were reused |
| `tool_call_count` | edit vs bash ratio (a behaviour signal, not a gate) |
| `task_pass` | did the diffs land the intended fix? (context only, not gate) |

## 3. Methodology — interleaved trials (NON-NEGOTIABLE)

Memory id 3687 / 3609 / WORKFLOW_RULES: **2.7x LLM variance is irreducible.** Sequential
runs (all baselines then all proxied) confound provider cache-state with the treatment.
Rule:

- **Interleave:** trial order is `B P B P B P ...` (or randomized ABAB). Never `BBBPPP`.
- **Min 3 trials per condition** (so >=6 total) to see past variance.
- **Same task, same model, same target repo snapshot** for every trial.
- Report median + min/max range, never a single number.

## 4. "It just works" — the env contract

The benchmark must fail LOUDLY and EARLY with a precise fix, never silently drift.

```
RELIARY_BENCH_API_KEY   required — the upstream provider key (e.g. DeepSeek sk-...)
RELIARY_BENCH_UPSTREAM  required — provider base URL (e.g. https://api.deepseek.com)
RELIARY_BENCH_MODEL     required — model id (e.g. deepseek-chat)
RELIARY_BIN             optional — path to reliary binary (default: ./target/release/reliary)
RELIARY_BENCH_TRIALS    optional — trials per condition (default 3)
RELIARY_BENCH_PORT      optional — proxy port (default 9099 to avoid clobbering prod)
```

`bench --check` runs BEFORE any LLM call and verifies, in order, with one clear error
per failure:
1. `RELIARY_BENCH_API_KEY` / `_UPSTREAM` / `_MODEL` are all set and non-empty.
2. `$RELIARY_BIN` exists and `--version` succeeds.
3. The proxy can start on `RELIARY_BENCH_PORT`, `/health` returns ok, then shut down.
4. The proxy can reach the upstream (one trivial 1-token call, NOT counted).
5. The frozen target repo exists and `reliary index` succeeds on it.
6. The baseline-direct path also reaches the upstream (proves the key works outside proxy).

If any step fails, exit non-zero with the exact missing var or failed check. No partial runs.

## 5. The task — frozen, git-tracked, deterministic input

`pipeline` is the buggy Python target. We freeze a *copy* into the bench dir so the repo
state is identical across every trial (an LLM editing files would otherwise mutate the
target between runs — the #1 silent contamination, per WORKFLOW_RULES "use git worktrees
or fresh branches/clones to avoid contaminated baselines").

For each trial:
1. `rm -rf /tmp/reliary-bench-target && cp -r <frozen> /tmp/reliary-bench-target`
2. `reliary index /tmp/reliary-bench-target` (fresh FTS5)
3. Run the agent against the fresh copy.
4. Diff the result against the known-good fix → `task_pass`.

## 6. Two execution paths in one script

The same script drives both conditions, so they share task/repo/key/model — only the
HTTP path differs:

- **BASELINE (`-b baseline`):** call the upstream provider directly with the raw messages.
  The agent's messages go to `$RELIARY_BENCH_UPSTREAM` with no reliary in the path.
- **PROXIED (`-b proxied`):** point the agent at `http://127.0.0.1:$PORT/v1/chat/completions`.
  The proxy forwards to the upstream, compressing both directions.

To stay agent-agnostic and deterministic, the "agent" is a **scripted multi-turn
conversation** (a fixed message list per turn), NOT a real agent loop. This kills the
"which agent" and "stochastic agent behaviour" variance entirely — we measure the
*proxy's compression* in isolation, which is what Step 2 is gated on. (A real-agent
end-to-end bench is a separate, later harness; the proxy gate does not need it.)

## 7. Output

Every trial writes a JSON line to `bench/results/<timestamp>/<condition>-<trial>.json` with
all metrics. A final `summary.json` aggregates medians + ranges + wc_savings_pct.
Human-readable table to stdout. Nothing else.

## 8. What kills this bench (anti-patterns to avoid)

- Reading the API key from a settings file the user may or may not have → **env vars only.**
- Mutating the target repo between trials → **fresh copy every trial.**
- Sequential runs → **interleave.**
- Trusting char counts → **provider usage tokens only.**
- Single trial → **min 3, report range.**
- Silent fallback when a component is missing → **fail loud in `--check`.**
- Mixing "did it fix the bug" with "did it save tokens" → **record pass/fail as context,
  gate on cost.**
