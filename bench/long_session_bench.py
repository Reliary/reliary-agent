#!/usr/bin/env python3
"""Long-session benchmark — measures cumulative cost over 10+ chained queries.

Unlike multi_turn_harness.py (independent 6-turn tasks), this runs ONE continuous
session where the LLM answers 10 code intelligence questions in sequence. The
conversation history never resets, so tool output from query 1 is still in context
at query 10. This tests the compounding hypothesis:

- Reliary: 1 call/query × 776 bytes = ~7.7K bytes accumulated
- ALTBACKEND (working): 11 calls/query × 1500 bytes = ~165K bytes accumulated
- Bash compression: 30-95% per bash call (reliary only)

The short-bench gap (1.84x) should widen in reliary's favor over a long session
because ALTBACKEND's 11-call pattern accumulates 11x more history per query.

Conditions:
- A (reliary): find_references + callgraph + goto_def + search + bash(wrap)
- B (altbackend): search_graph + get_code_snippet + trace_path + bash
- C (grep): grep + read + bash

Usage:
  python3 long_session_bench.py --conditions A,B,C --seeds 42 123 --out results/long_session.jsonl
"""
import argparse
import json
import os
import random
import re
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from llm_conn import (deepseek_chat, TOKIO_CORPUS, DEEPSEEK_MODEL)
from multi_turn_harness import (
    MCPSession, _sessions, _close_sessions,
    CONDITION_TOOLS, CONDITION_NAMES,
    RELIARY_SYS, ALTBACKEND_SYS, GREP_SYS,
    SYSTEM_PROMPTS, parse_llm_response, execute_tool, score_answer,
)
import multi_turn_harness as mth

RELIARY_BIN = mth.RELIARY_BIN
ALTBACKEND_BIN = mth.ALTBACKEND_BIN

# ============================================================
# Session queries — 10 chained code intelligence questions
# Each builds on the previous (realistic coding session)
# ============================================================

SESSION_QUERIES = [
    {
        "id": "q1_consume_impls",
        "question": "In the tokio codebase, how many different types implement a `consume` method? List the type names and file paths where each is defined.",
        "rubric": {"accept": ["Take", "BufStream", "BufWriter", "Empty", "Chain", "AsyncBufRead", "reader"], "min_count": 3, "max_count": 12},
    },
    {
        "id": "q2_consume_callers",
        "question": "Now, find all the places in tokio where the `consume` method is actually CALLED (not defined). List file:line for each call site.",
        "rubric": {"accept_keywords": ["consume", "amt", "buf", "read", "take"], "min_calls": 2},
    },
    {
        "id": "q3_block_on_def",
        "question": "Where is `Runtime::block_on` defined? Give file:line and show the function signature.",
        "rubric": {"accept_keywords": ["block_on", "Runtime", "fn", "poll"], "min_steps": 1},
    },
    {
        "id": "q4_block_on_chain",
        "question": "Trace the call chain from `Runtime::block_on` — what functions does it call directly? List each callee with file:line.",
        "rubric": {"accept_keywords": ["block_on", "spawn", "schedule", "wake", "push", "queue"], "min_steps": 2},
    },
    {
        "id": "q5_spawn_callers",
        "question": "Who calls `spawn` in the tokio runtime? List the top 5 callers with file:line.",
        "rubric": {"accept_keywords": ["spawn", "handle", "runtime", "task"], "min_calls": 2},
    },
    {
        "id": "q6_sleep_def",
        "question": "Find the definition of the `Sleep` struct in tokio's time module. Show file:line and the struct definition.",
        "rubric": {"accept_keywords": ["Sleep", "struct", "time", "sleep", "timer"], "min_steps": 1},
    },
    {
        "id": "q7_sleep_methods",
        "question": "List all methods defined on the `Sleep` type. For each, give file:line.",
        "rubric": {"accept_keywords": ["poll", "sleep", "elapsed", "deadline", "reset", "is_elapsed"], "min_methods": 2},
    },
    {
        "id": "q8_bufwriter_write",
        "question": "When you call `write` on a `BufWriter`, which inner methods does it eventually call? Trace the delegation chain.",
        "rubric": {"accept_keywords": ["write", "flush", "poll_write", "inner", "pin", "buffer"], "min_steps": 2},
    },
    {
        "id": "q9_consume_impls_recheck",
        "question": "Earlier you found types implementing `consume`. Which of those types also implement `poll_write`? Cross-reference the two lists.",
        "rubric": {"accept_keywords": ["BufWriter", "poll_write", "consume", "write"], "min_steps": 1},
    },
    {
        "id": "q10_dead_code",
        "question": "Find any functions in the tokio io/util module that are defined but never called from anywhere in the codebase. List file:line for any you find.",
        "rubric": {"accept_keywords": ["dead", "unused", "consume", "empty", "chain", "take"], "min_steps": 1},
    },
]

# V58b: cache-exercise tail — exact repeats of q3, q6, q1 (in that order)
CACHE_QUERIES = [
    {"id": "q11_repeat_block_on_def",
     "question": "Where is `Runtime::block_on` defined? Give file:line and show the function signature.",
     "rubric": {"accept_keywords": ["block_on", "Runtime", "fn", "poll"], "min_steps": 1}},
    {"id": "q12_repeat_sleep_def",
     "question": "Find the definition of the `Sleep` struct in tokio's time module. Show file:line and the struct definition.",
     "rubric": {"accept_keywords": ["Sleep", "struct", "time", "sleep", "timer"], "min_steps": 1}},
    {"id": "q13_repeat_consume_impls",
     "question": "In the tokio codebase, how many different types implement a `consume` method? List the type names and file paths where each is defined.",
     "rubric": {"accept": ["Take", "BufStream", "BufWriter", "Empty", "Chain", "AsyncBufRead", "reader"], "min_count": 3, "max_count": 12}},
]

MAX_TURNS_PER_QUERY = 8



def run_long_session(cond, model, seed, timeout_total=1800):
    """Run one long session: 10 chained queries with shared conversation history."""
    _sessions_keys = list(_sessions.keys())
    mth._ensure_sessions(cond)
    rng = random.Random(seed)
    sys_prompt = SYSTEM_PROMPTS[cond]
    
    # Shared conversation history across ALL queries
    messages = [{"role": "system", "content": sys_prompt}]
    
    session_metrics = {
        "cond": cond,
        "cond_name": CONDITION_NAMES[cond],
        "model": model,
        "seed": seed,
        "queries": [],
        "total_wall_time": 0,
"total_tool_calls": 0,
         "total_turns": 0,
         "total_tokens_in": 0,
         "total_tokens_out": 0,
         "total_cached_tokens": 0,
         "total_weighted_cost": 0,
         "total_billed_cost": 0,
         "total_tool_bytes": 0,
         "total_dead_end_calls": 0,
         "total_score": 0,
         "history_bytes_at_end": 0,
     }
    
    t_start = time.time()
    
    import os as _os
    _active_queries = SESSION_QUERIES + (CACHE_QUERIES if _os.environ.get("RELIARY_CACHE_BENCH") == "1" else [])
    for qi, query in enumerate(_active_queries):
        print(f"  Query {qi+1}/10: {query['id']}", file=sys.stderr)
        
        # Add query as user message (history persists)
        messages.append({"role": "user", "content": query["question"]})
        
        q_metrics = {
            "query_id": query["id"],
            "tool_calls": 0,
            "turns": 0,
            "tokens_in": 0,
            "tokens_out": 0,
            "cached_tokens": 0,
            "weighted_cost": 0,
            "tool_bytes": 0,
            "dead_end_calls": 0,
            "score": 0,
            "answer": "",
            "timed_out": False,
        }
        
        last_message = ""
        for turn in range(MAX_TURNS_PER_QUERY):
            if time.time() - t_start > timeout_total:
                q_metrics["timed_out"] = True
                break
            
            # V59m: warn at turn 5 that time is running out — prevents the
            # explore-until-cap-then-empty pattern.
            if turn == MAX_TURNS_PER_QUERY - 3:
                messages.append({"role": "user", "content": "Two turns left. If your next tool result answers the question, give your FINAL answer immediately: {\"final\": true, \"answer\": \"...\"}."})
            # Force final answer on last turn
            if turn == MAX_TURNS_PER_QUERY - 1:
                messages.append({"role": "user", "content": "Give your FINAL answer now. ONE LINE: {\"final\": true, \"answer\": \"...\"}. Use whatever you have gathered so far — a partial answer with evidence beats no answer."})
            
            resp = deepseek_chat(messages, model=model, max_tokens=1500,
                                  timeout=120, disable_thinking=True)
            
            if "error" in resp:
                q_metrics["answer"] = f"(error: {str(resp['error'])[:200]})"
                break
            
            msg = resp.get("choices", [{}])[0].get("message", {})
            content = msg.get("content", "") or msg.get("reasoning_content", "")
            usage = resp.get("usage", {})

            q_metrics["tokens_in"] += usage.get("prompt_tokens", 0)
            q_metrics["tokens_out"] += usage.get("completion_tokens", 0)
            # DeepSeek API reports cached input tokens under prompt_tokens_details.
            # Capture the authoritative cache hit count from the provider.
            prompt_details = usage.get("prompt_tokens_details", {})
            cached_tokens = prompt_details.get("cached_tokens", 0)
            q_metrics["cached_tokens"] = q_metrics.get("cached_tokens", 0) + cached_tokens
            q_metrics["turns"] = turn + 1
            last_message = content
            
            action_type, action = parse_llm_response(content)
            
            if action_type == "final":
                q_metrics["answer"] = action.get("answer", content[:2000])
                q_metrics["weighted_cost"] = q_metrics["tokens_in"] + 4 * q_metrics["tokens_out"]
                break
            elif action_type == "tool":
                tool_name = action.get("tool", action.get("name", ""))
                tool_args = action.get("args", action.get("arguments", action))
                if isinstance(tool_args, str):
                    try:
                        tool_args = json.loads(tool_args) if tool_args.strip().startswith("{") else {"query": tool_args}
                    except Exception:
                        tool_args = {"query": tool_args}
                
                output, tool_elapsed = execute_tool(cond, tool_name, tool_args)
                q_metrics["tool_calls"] += 1
                q_metrics["tool_bytes"] += len(output.encode())
                
                print(f"    [tool] {tool_name}({tool_args}) -> {repr(output[:100])}", file=sys.stderr)
                
                no_hits_markers = ["(no ", "(no results)", "(no definition", "(no trace", "(no search", "(error", "(altbackend error", "(grep error", "(read error"]
                if any(output.strip().startswith(m) for m in no_hits_markers):
                    q_metrics["dead_end_calls"] += 1
                
                messages.append({"role": "assistant", "content": content})
                messages.append({"role": "user", "content": f"Tool result:\n{output[:4000]}"})
            else:
                q_metrics["answer"] = content[:2000]
                q_metrics["weighted_cost"] = q_metrics["tokens_in"] + 4 * q_metrics["tokens_out"]
                break
        
        # Score the answer
        q_metrics["score"] = score_answer(query, q_metrics["answer"])
        
        # Add final answer to history (so next query sees it)
        if q_metrics["answer"] and not q_metrics["timed_out"]:
            messages.append({"role": "assistant", "content": q_metrics["answer"]})
        
        # NOTE: no context trimming. We keep full tool results because the LLM
        # needs file:line data from earlier queries to answer cross-reference
        # questions. Trimming causes dead-end re-queries and score regression.
        
        # Accumulate
        session_metrics["queries"].append(q_metrics)
        session_metrics["total_tool_calls"] += q_metrics["tool_calls"]
        session_metrics["total_turns"] += q_metrics["turns"]
        session_metrics["total_tokens_in"] += q_metrics["tokens_in"]
        session_metrics["total_tokens_out"] += q_metrics["tokens_out"]
        session_metrics["total_cached_tokens"] += q_metrics.get("cached_tokens", 0)
        session_metrics["total_weighted_cost"] += q_metrics["weighted_cost"]
        # Billed cost: cache discount — DeepSeek charges ~10% for cached input tokens.
        # Full rate for prompt - cached, plus 4× completion (cache doesn't apply to output).
        cached = q_metrics.get("cached_tokens", 0)
        uncached_in = max(0, q_metrics["tokens_in"] - cached)
        q_metrics["billed_cost"] = uncached_in + int(cached * 0.1) + 4 * q_metrics["tokens_out"]
        session_metrics["total_billed_cost"] += q_metrics["billed_cost"]
        session_metrics["total_tool_bytes"] += q_metrics["tool_bytes"]
        session_metrics["total_dead_end_calls"] += q_metrics["dead_end_calls"]
        session_metrics["total_score"] += q_metrics["score"]
        
        print(f"    score={q_metrics['score']} wc={q_metrics['weighted_cost']} calls={q_metrics['tool_calls']}", file=sys.stderr)
        
        if q_metrics["timed_out"]:
            break
    
    session_metrics["total_wall_time"] = time.time() - t_start
    session_metrics["history_bytes_at_end"] = sum(len(json.dumps(m).encode()) for m in messages)
    
    _close_sessions()
    return session_metrics


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default=DEEPSEEK_MODEL)
    parser.add_argument("--seeds", nargs="+", type=int, default=[42, 17, 123, 456])
    parser.add_argument("--conditions", default="A,B,C")
    parser.add_argument("--out", default=None)
    parser.add_argument("--timeout", type=int, default=1800)
    parser.add_argument("--sift-bash", action="store_true",
                        help="Pass RELIARY_SIFT_BASH=1 to MCP subprocess env (enables bash auto-rewrite)")
    args = parser.parse_args()

    if args.sift_bash:
        os.environ["RELIARY_SIFT_BASH"] = "1"

    conditions = args.conditions.split(",")
    out_path = args.out or f"results/long_session_{int(time.time())}.jsonl"
    
    all_runs = []
    
    for seed in args.seeds:
        for cond in conditions:
            cond_name = CONDITION_NAMES.get(cond, cond)
            print(f"\n=== seed={seed} cond={cond} ({cond_name}) ===", file=sys.stderr)
            
            try:
                metrics = run_long_session(cond, args.model, seed, args.timeout)
                all_runs.append(metrics)
            except Exception as e:
                print(f"ERROR: {e}", file=sys.stderr)
                all_runs.append({"cond": cond, "seed": seed, "error": str(e)})
            
            _close_sessions()
    
    # Write results
    RESULTS_DIR = Path(__file__).parent / "results"
    RESULTS_DIR.mkdir(exist_ok=True)
    out_path = RESULTS_DIR / Path(out_path).name
    with open(out_path, "w") as f:
        for run in all_runs:
            f.write(json.dumps(run) + "\n")
    
    # Print summary
    import statistics
    print(f"\n{'='*80}")
    print(f"Long-session results: {len(all_runs)} runs")
    print(f"{'='*80}")
    
    for cond in conditions:
        runs = [r for r in all_runs if r.get("cond") == cond and "error" not in r]
        if not runs:
            continue
        scores = [r["total_score"] for r in runs]
        wcs = [r["total_weighted_cost"] for r in runs]
        calls = [r["total_tool_calls"] for r in runs]
        turns = [r["total_turns"] for r in runs]
        tbs = [r["total_tool_bytes"] for r in runs]
        hbs = [r["history_bytes_at_end"] for r in runs]
        walls = [r["total_wall_time"] for r in runs]
        deads = [r["total_dead_end_calls"] for r in runs]
        
        print(f"\n{cond} ({CONDITION_NAMES.get(cond, cond)}):")
        if len(scores) >= 2:
            std = statistics.stdev(scores)
            print(f"  Score:      {statistics.mean(scores):.1f} ± {std:.1f}/{10*3} (median {statistics.median(scores):.1f})")
            print(f"  Score per seed: {[(s, scores[i]) for i, s in enumerate(args.seeds)]}")
        else:
            print(f"  Score:      {statistics.median(scores):.1f}/{10*3} (mean {statistics.mean(scores):.1f})")
        print(f"  WC:         median={statistics.median(wcs):.0f} mean={statistics.mean(wcs):.0f}")
        # Cache hit rate and billed cost from DeepSeek's prompt_tokens_details.
        cached_totals = [r.get("total_cached_tokens", 0) for r in runs]
        tin_totals = [r["total_tokens_in"] for r in runs]
        cache_pcts = [(c / t * 100) if t > 0 else 0 for c, t in zip(cached_totals, tin_totals)]
        billed_costs = [r.get("total_billed_cost", r.get("total_weighted_cost", 0)) for r in runs]
        if any(c > 0 for c in cached_totals):
            print(f"  Cached:     median={statistics.median(cached_totals):.0f} ({statistics.median(cache_pcts):.1f}% of input)")
            print(f"  Billed cost: median={statistics.median(billed_costs):.0f} (after cache discount)")
            savings = 1 - (statistics.median(billed_costs) / statistics.median(wcs)) if statistics.median(wcs) > 0 else 0
            print(f"  Cache savings: {savings*100:.1f}% vs full-rate WC")
        print(f"  Tokens in:  median={statistics.median(tin_totals):.0f}")
        print(f"  Tokens out: median={statistics.median([r['total_tokens_out'] for r in runs]):.0f}")
        print(f"  Tool calls: median={statistics.median(calls):.0f}")
        print(f"  Turns:      median={statistics.median(turns):.0f}")
        print(f"  Tool bytes: median={statistics.median(tbs):.0f}")
        print(f"  History:    median={statistics.median(hbs):.0f} bytes")
        print(f"  Dead-ends:  median={statistics.median(deads):.0f}")
        print(f"  Wall time:  median={statistics.median(walls):.0f}s")
    
    if len(conditions) >= 2:
        a_wcs = [r["total_weighted_cost"] for r in all_runs if r.get("cond") == "A" and "error" not in r]
        b_wcs = [r["total_weighted_cost"] for r in all_runs if r.get("cond") == "B" and "error" not in r]
        if a_wcs and b_wcs:
            ratio = statistics.median(a_wcs) / statistics.median(b_wcs)
            print(f"\nA/B WC ratio: {ratio:.2f}x")
    
    print(f"\nOutput: {out_path}")


if __name__ == "__main__":
    main()