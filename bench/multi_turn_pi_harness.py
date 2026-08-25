"""Arc 43 v2 — Pi-driven multi-turn benchmark.

Architecture:
  Python harness -> Pi subprocess (--print --mode json --session FILE)
  Pi internally spawns MCP servers ONCE per session
    Cond A: reliary extension (one persistent reliary mcp process)
    Cond B: altbackend extension (one persistent altbackend mcp process)

Both extensions use the ensureProc() pattern — ONE subprocess per session,
NOT per call. This is how a real user interacts with Pi + MCP.

Reuses TASKS and score_answer from multi_turn_harness.py.

Arc 43 v2 — single source of truth: session file. Pi writes incrementally
and survives timeouts. Tokens come from message_end events (same pattern as
reliary-agent's bench_paired.py:69-82).
"""

import argparse
import json
import os
import random
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from multi_turn_harness import TASKS, score_answer
from llm_conn import PI_BIN, PI_SETTINGS, TOKIO_CORPUS, DEEPSEEK_API_KEY_FALLBACK

RESULTS_DIR = SCRIPT_DIR / "results"
SEEDS = [42, 123, 789]
PI_DISABLE_HEARTBEAT = "1"

# ============================================================
# Extension paths
# ============================================================
EXT_RELIARY = str(SCRIPT_DIR / "reliary_mcp_pi_extension.js")
EXT_ALTBACKEND = str(SCRIPT_DIR / "altbackend_pi_extension.js")


def set_pi_ext(ext_path):
    """Mutate ~/.pi/agent/settings.json so Pi loads the right extension."""
    with open(PI_SETTINGS) as f:
        d = json.load(f)
    if ext_path:
        d["extensions"] = [ext_path]
        d["packages"] = [ext_path]
    else:
        d["extensions"] = []
        d["packages"] = []
    with open(PI_SETTINGS, "w") as f:
        json.dump(d, f, indent=2)


def condition_ext(cond):
    """Return the extension path to load for a condition, or None to unload all."""
    if cond == "A":
        return EXT_RELIARY
    elif cond == "B":
        return EXT_ALTBACKEND
    return None


# ============================================================
# Tool instructions — appended to user prompt
# ============================================================
RELIARY_INSTRUCTIONS = """
CRITICAL: You have a strict budget. Make AT MOST 5 tool calls total.

After 5 tool calls, STOP calling tools and write your final answer in plain text.
Do not call tools after writing your answer.

Your tools (the ONLY tools available):
- reliary_find_references(name) — find all references to a symbol
- reliary_callgraph(name) — callers/callees of a function
- reliary_methods_on(type_name) — list methods on a type
- reliary_goto_def(name) — find definition site
- reliary_search(query) — BM25 full-text search
"""

ALTBACKEND_INSTRUCTIONS = """
CRITICAL: You have a strict budget. Make AT MOST 5 tool calls total.

After 5 tool calls, STOP calling tools and write your final answer in plain text.
Do not call tools after writing your answer.

Your tools (the ONLY tools available):
- altbackend_search_graph(query) — search symbols by name
- altbackend_get_code_snippet(qualified_name) — read source by qualified name
- altbackend_trace_path(function_name, direction="both") — callers/callees
- altbackend_get_architecture() — codebase structure overview
"""


# ============================================================
# Single source of truth: session file (Arc 43 v2)
# ============================================================
def read_session(sfile):
    """Read Pi's session JSONL file. Returns dict with:
    final_answer, turns, tool_calls, prompt_tokens, completion_tokens.

    Pi v0.78 emits:
      - "message" events with role=user|assistant|toolResult and message.usage
      - "tool_execution_start" events for each tool call
      - NO "message_end" event (usage is on every message, not just the last)

    Per-message usage accumulates to total prompt+completion tokens.
    """
    out = {
        "final_answer": "",
        "turns": 0,
        "tool_calls": 0,
        "prompt_tokens": 0,
        "completion_tokens": 0,
    }
    if not os.path.exists(sfile):
        return out
    last_text = ""
    with open(sfile) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
            except Exception:
                continue
            t = d.get("type", "")
            msg = d.get("message", {})
            # Tokens accumulate from every message's usage field
            u = msg.get("usage")
            if isinstance(u, dict):
                out["prompt_tokens"] += u.get("input", 0)
                out["completion_tokens"] += u.get("output", 0)
            # Each tool invocation = one tool_execution_start
            if t == "tool_execution_start":
                out["tool_calls"] += 1
            # Assistant turn count + final text capture
            if t == "message" and msg.get("role") == "assistant":
                out["turns"] += 1
                for c in msg.get("content", []):
                    if isinstance(c, dict) and c.get("type") == "text":
                        text = c.get("text", "").strip()
                        if text:
                            last_text = text
    out["final_answer"] = last_text
    return out


# ============================================================
# Single-condition runner (Arc 43 v2)
# ============================================================
def run_condition(task, cond, model, seed, timeout_total=120):
    """Run one task under one condition via Pi. Returns metrics dict.

    Wall-time cap = timeout_total. After timeout, read whatever was written
    to the session file (Pi writes incrementally, survives timeout).
    """
    set_pi_ext(condition_ext(cond))

    sfile = f"/tmp/arc43-{int(time.time()*1000)}-{cond}-{seed}.json"
    if os.path.exists(sfile):
        os.remove(sfile)

    if cond == "A":
        tool_instructions = RELIARY_INSTRUCTIONS
    elif cond == "B":
        tool_instructions = ALTBACKEND_INSTRUCTIONS
    else:
        tool_instructions = ""
    full_prompt = f"{task['question']}\n{tool_instructions}"

    env = os.environ.copy()
    env["PI_DISABLE_HEARTBEAT"] = PI_DISABLE_HEARTBEAT
    env["DEEPSEEK_API_KEY"] = DEEPSEEK_API_KEY_FALLBACK
    env.pop("RELIARY_PROXY_ACTIVE", None)
    env.pop("OPENAI_BASE_URL", None)
    env.pop("DEEPSEEK_BASE_URL", None)
    env.pop("RELIARY_MODE", None)

    metrics = {
        "task_id": task["id"],
        "cond": cond,
        "cond_name": {"A": "reliary", "B": "altbackend"}.get(cond, cond),
        "model": model,
        "seed": seed,
        "wall_time": 0,
        "tool_calls": 0,
        "turns": 0,
        "tokens_in": 0,
        "tokens_out": 0,
        "weighted_cost": 0,
        "final_answer": "",
        "task_score": 0,
        "error": None,
        "timed_out": False,
        "ext_loaded": condition_ext(cond) or "(none)",
    }

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
    metrics["final_answer"] = s["final_answer"][:2000]
    metrics["task_score"] = score_answer(task, s["final_answer"])
    return metrics


# ============================================================
# Main
# ============================================================
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--tasks", type=int, default=5)
    parser.add_argument("--model", default="deepseek/deepseek-v4-flash")
    parser.add_argument("--seeds", type=int, nargs="+", default=SEEDS)
    parser.add_argument("--conditions", default="A,B")
    parser.add_argument("--out", default=None)
    parser.add_argument("--timeout", type=int, default=120)
    args = parser.parse_args()

    tasks = TASKS[:args.tasks]
    conditions = args.conditions.split(",")
    seeds = args.seeds

    if args.out is None:
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        out_path = RESULTS_DIR / f"arc43_pi_{ts}.jsonl"
    else:
        out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    n_runs = len(tasks) * len(conditions) * len(seeds)
    print(f"=== Arc 43 v2 Pi-Driven Benchmark ===")
    print(f"Tasks: {len(tasks)} x Conditions: {len(conditions)} x Seeds: {len(seeds)} = {n_runs} runs")
    print(f"Model: {args.model}")
    print(f"Output: {out_path}")
    print(f"Timeout: {args.timeout}s per run\n")

    all_results = []
    out_f = open(out_path, "w")

    existing = set()
    if out_path.exists():
        with open(out_path) as f:
            for line in f:
                try:
                    d = json.loads(line)
                    existing.add((d['task_id'], d['cond'], d['seed']))
                except Exception:
                    pass

    try:
        for task_idx, task in enumerate(tasks):
            print(f"Task {task_idx+1}/{len(tasks)}: {task['id']}")
            for seed in seeds:
                if seeds.index(seed) % 2 == 0:
                    order = conditions
                else:
                    order = list(reversed(conditions))
                for cond in order:
                    key = (task['id'], cond, seed)
                    if key in existing:
                        print(f"  skip {cond} seed={seed} (already in file)")
                        continue
                    print(f"  seed={seed} cond={cond} ...", end=" ", flush=True)
                    metrics = run_condition(task, cond, args.model, seed, args.timeout)
                    out_f.write(json.dumps(metrics) + "\n")
                    out_f.flush()
                    print(f"score={metrics['task_score']} t={int(metrics['wall_time'])}s "
                          f"wc={metrics['weighted_cost']} calls={metrics['tool_calls']} "
                          f"turns={metrics['turns']}")
                    all_results.append(metrics)
    finally:
        set_pi_ext(None)
        out_f.close()

    print(f"\n=== SUMMARY ===")
    by_cond = {"A": [], "B": [], "C": []}
    for r in all_results:
        by_cond.setdefault(r["cond"], []).append(r)
    for cond, rs in by_cond.items():
        if not rs:
            continue
        scores = sorted([r["task_score"] for r in rs])
        wc = sorted([r["weighted_cost"] for r in rs])
        wt = sorted([r["wall_time"] for r in rs])
        name = rs[0]["cond_name"]
        med = lambda xs: xs[len(xs)//2]
        print(f"  {cond} ({name}): n={len(rs)}")
        print(f"    score median={med(scores)} mean={sum(scores)/len(scores):.2f}")
        print(f"    wc median={med(wc)} mean={sum(wc)/len(wc):.0f}")
        print(f"    wall median={int(med(wt))}s mean={sum(wt)/len(wt):.1f}s")
    print(f"\nDone. Output: {out_path}")


if __name__ == "__main__":
    main()