"""Arc 28 Lever 6 v2 — Direct DeepSeek comparison.

For each task, pre-fetch tool output from BOTH reliary8 and altbackend-mcp.
Feed each tool's output to the LLM in a single-turn prompt. LLM extracts references.
This isolates "which backend gives the LLM better raw material" from "LLM tool-calling skill".

Per workspace WORKFLOW_RULES: interleaved A/B per task.

Constraints:
- Direct DeepSeek via api.deepseek.com (NOT reliary.dev).
- No Pi, no MCP, no multi-turn.
- Single-turn LLM call (~3-10s per task).
"""
import argparse
import json
import os
import random
import re
import subprocess
import sys
import time
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from llm_conn import (deepseek_chat, mcp_call, TOKIO_CORPUS, HOMONYMS_FIXTURE)

ALTBACKEND_BIN = "/home/user/.local/bin/altbackend-mcp"

RESULTS_DIR = "/home/user/src/reliary8/bench/results"


def run_altbackend_cli(tool, args, timeout=30):
    """Run altbackend-mcp via CLI."""
    r = subprocess.run([ALTBACKEND_BIN, "cli", tool, json.dumps(args)],
                        capture_output=True, text=True, timeout=timeout)
    try:
        return json.loads(r.stdout)
    except Exception:
        return {"error": r.stdout[-300:]}


def normalize(p):
    if not p:
        return ""
    p = str(p).strip()
    if p.startswith(TOKIO_CORPUS):
        p = p[len(TOKIO_CORPUS):].lstrip("/")
    return p


def grep_ground_truth(stem, anchor_file=None, anchor_line=None):
    """Grep-based GT: all file:line where stem appears as a whole word.
    Used as a sanity-check oracle alongside the type-flow oracle."""
    try:
        r = subprocess.run(
            ["grep", "-rEn", rf"\b{stem}\b", TOKIO_CORPUS, "--include=*.rs"],
            capture_output=True, text=True, timeout=15)
    except Exception:
        return []
    refs = []
    for line in r.stdout.split("\n"):
        parts = line.split(":", 2)
        if len(parts) >= 3:
            try:
                ln = int(parts[1])
                refs.append(f"{normalize(parts[0])}:{ln}")
            except ValueError:
                pass
    return refs


def fetch_reliary_hits(task):
    """Run reliary_find_references_type_flow for the anchor in this task."""
    a = task.get("anchor")
    if not a:
        return []
    rel = a["anchor_file"]
    if rel.startswith(TOKIO_CORPUS):
        rel = rel[len(TOKIO_CORPUS):].lstrip("/")
    r = mcp_call("reliary_find_references_type_flow",
                  {"name": a["stem"], "anchor_file": rel,
                   "anchor_line": a["anchor_line"], "path": TOKIO_CORPUS,
                   "threshold": 0.1},
                  workdir=TOKIO_CORPUS, timeout=60)
    return r.get("hits", [])


def fetch_altbackend_search(task):
    """Run altbackend_search_graph for the stem."""
    a = task.get("anchor")
    if not a:
        return []
    return run_altbackend_cli("search_graph", {
        "project": "tmp-tokio-corpus-tokio-src",
        "query": a["stem"],
        "label": "Function",
        "limit": 50,
    }, timeout=30)


def fetch_reliary_string(stem, anchor_file=None, anchor_line=None,
                           workdir=TOKIO_CORPUS, limit=50):
    """Return top-N (file, line, similarity) hits as a compact string.
    Uses reliary_find_references_type_flow if anchor is given (returns lines).
    Falls back to reliary_search (files only) otherwise."""
    if anchor_file and anchor_line is not None:
        rel = normalize(anchor_file)
        r = mcp_call("reliary_find_references_type_flow",
                      {"name": stem, "anchor_file": rel,
                       "anchor_line": anchor_line, "path": workdir,
                       "threshold": 0.1},
                      workdir=workdir, timeout=60)
        hits = r.get("hits", [])
        return "\n".join(f"  {normalize(h.get('file'))}:{h.get('line', 0)}  sim={h.get('similarity', 0):.3f}"
                          for h in hits[:limit])
    # Fallback to grep-based output for non-anchored search
    try:
        r = subprocess.run(
            ["grep", "-rEn", rf"\b{stem}\b", workdir, "--include=*.rs"],
            capture_output=True, text=True, timeout=15)
        return r.stdout[:5000]
    except Exception:
        return ""


def fetch_altbackend_string(stem, workdir=TOKIO_CORPUS, limit=50):
    """Return altbackend_search_graph results as a compact string."""
    r = run_altbackend_cli("search_graph", {
        "project": "tmp-tokio-corpus-tokio-src",
        "query": stem,
        "limit": limit,
    })
    results = r.get("results", [])
    return "\n".join(f"  {normalize(res.get('file_path'))}:{res.get('start_line', 0)}"
                      for res in results[:limit])


def fetch_reliary_trace(function_name):
    """Callers via reliary (no direct trace tool, fall back to grep)."""
    try:
        r = subprocess.run(
            ["grep", "-rln", f"\\b{function_name}\\b", TOKIO_CORPUS,
              "--include=*.rs"],
            capture_output=True, text=True, timeout=15)
        return r.stdout[:5000]
    except Exception:
        return ""


def fetch_altbackend_trace(function_name):
    """Callers via altbackend_trace_path."""
    r = run_altbackend_cli("trace_path", {
        "project": "tmp-tokio-corpus-tokio-src",
        "function_name": function_name,
        "direction": "inbound",
        "depth": 2,
    })
    callers = r.get("callers", [])
    return "\n".join(f"  {normalize(c.get('qualified_name', ''))}"
                      for c in callers[:15])


def parse_llm_json(content):
    content = (content or "").strip()
    if not content:
        return None
    content = re.sub(r"^```(?:json)?\s*", "", content)
    content = re.sub(r"\s*```\s*$", "", content)
    content = content.strip()
    try:
        return json.loads(content)
    except Exception:
        pass
    for m in re.finditer(r"\{[\s\S]*?\}", content):
        try:
            return json.loads(m.group(0))
        except Exception:
            continue
    return None


def extract_refs(parsed):
    if not isinstance(parsed, dict):
        return []
    refs = (parsed.get("references") or parsed.get("callers") or
             parsed.get("dead") or parsed.get("files") or [])
    out = []
    for r in refs:
        if isinstance(r, dict):
            f = normalize(r.get("file", ""))
            l = r.get("line", 0)
            if f:
                out.append(f"{f}:{l}" if l else f)
        elif isinstance(r, str):
            out.append(normalize(r))
    return out


def jaccard(pred, gt):
    p = set(pred)
    g = set(gt)
    if not (p | g):
        return 0.0
    return len(p & g) / len(p | g)


def call_llm(system, user, model="deepseek-v4-flash"):
    """Single-turn direct DeepSeek call."""
    messages = [{"role": "system", "content": system},
                {"role": "user", "content": user}]
    t0 = time.time()
    resp = deepseek_chat(messages, model=model, max_tokens=600, timeout=30)
    elapsed = time.time() - t0
    if "error" in resp:
        return {"elapsed": elapsed, "response": resp, "tokens_in": 0,
                "tokens_out": 0, "weighted_cost": 0, "content": "",
                "parsed": None, "predictions": []}
    msg = resp.get("choices", [{}])[0].get("message", {})
    content = msg.get("content", "")
    if not content and "reasoning_content" in msg:
        content = msg.get("reasoning_content", "")
    usage = resp.get("usage", {})
    pt = usage.get("prompt_tokens", 0)
    ct = usage.get("completion_tokens", 0)
    parsed = parse_llm_json(content)
    return {"elapsed": elapsed, "content": content,
            "tokens_in": pt, "tokens_out": ct, "weighted_cost": pt + 4 * ct,
            "parsed": parsed, "predictions": extract_refs(parsed),
            "model": resp.get("model", model)}


SYSTEM_PROMPT = """You are a precise code analyst. Given tool output listing file paths and line numbers, extract references that match the same class as the anchor definition.

Output a single JSON object on one line, no other text:
{"references": [{"file": "<relative path>", "line": <int>}, ...]}

Rules:
- Drop test files (paths containing /tests/ or _test.rs).
- Drop doc comments (lines starting with /// or //).
- Only include matches whose context matches the anchor's role and class."""


def run_task(task, condition, workdir, model):
    """Run a single task with the given backend's pre-fetched output."""
    cat = task["category"]
    # Normalize GT to relative paths for fair comparison.
    gt_raw = task.get("ground_truth", [])
    gt = []
    for g in gt_raw:
        n = normalize(g.split(":")[0]) if ":" in g else normalize(g)
        if ":" in g:
            n = f"{n}:{g.split(':')[1]}"
        gt.append(n)
    a = task.get("anchor", {})

    if cat == "find_references":
        stem = a.get("stem", "")
        anchor_file = a.get("anchor_file", "")
        anchor_line = a.get("anchor_line", 0)
        use_label = a.get("use_label", "")
        if condition == "A":
            tool_output = fetch_reliary_string(stem, anchor_file, anchor_line, workdir)
            tool_name = "reliary_find_references_type_flow"
        else:
            tool_output = fetch_altbackend_string(stem, workdir)
            tool_name = "altbackend_search_graph"
        user = (
            f"Anchor: `{anchor_file}:{anchor_line}` (label={use_label}, "
            f"stem=`{stem}`).\n\n"
            f"{tool_name} output:\n{tool_output}\n\n"
            f"Output a single JSON object on one line."
        )
    elif cat == "call_graph":
        fn = task.get("function", "spawn")
        if condition == "A":
            tool_output = fetch_reliary_trace(fn)
            tool_name = "grep"
        else:
            tool_output = fetch_altbackend_trace(fn)
            tool_name = "altbackend_trace_path"
        user = (
            f"Who calls `{fn}` in {workdir}?\n\n"
            f"{tool_name} output:\n{tool_output}\n\n"
            f"Output a single JSON object on one line: "
            f'{{"callers": [{{"file": "<path>", "line": <int>}}, ...]}}'
        )
    elif cat == "search":
        pattern = task.get("pattern", "")
        if condition == "A":
            tool_output = fetch_reliary_string(pattern, workdir=workdir)
            tool_name = "reliary_find_references_type_flow (fallback to grep)"
        else:
            tool_output = fetch_altbackend_string(pattern, workdir)
            tool_name = "altbackend_search_graph"
        user = (
            f"Find files matching `{pattern}` in {workdir}.\n\n"
            f"{tool_name} output:\n{tool_output}\n\n"
            f"Output a single JSON object on one line: "
            f'{{"files": ["<path>", ...]}}'
        )
    else:
        return None

    return call_llm(SYSTEM_PROMPT, user, model)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--tasks", default=os.path.join(RESULTS_DIR, "compare_tasks.json"))
    parser.add_argument("--n", type=int, default=None)
    parser.add_argument("--model", default="deepseek-chat")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    if not os.path.exists(args.tasks):
        print(f"Tasks file not found: {args.tasks}")
        return 1
    with open(args.tasks) as f:
        tasks = json.load(f)
    if args.n:
        tasks = tasks[:args.n]

    if args.out is None:
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        out_path = os.path.join(RESULTS_DIR, f"compare_v2_{ts}.jsonl")
    else:
        out_path = args.out
    os.makedirs(os.path.dirname(out_path), exist_ok=True)

    print(f"=== Arc 28 Lever 6 v2 — Direct LLM Compare ===")
    print(f"Tasks: {len(tasks)} from {args.tasks}")
    print(f"Model: {args.model}")
    print(f"Output: {out_path}\n")

    rng = random.Random(args.seed)
    rng.shuffle(tasks)

    out_f = open(out_path, "w")
    for i, task in enumerate(tasks):
        # Interleave per WORKFLOW_RULES.
        if i % 2 == 0:
            order = ["A", "B"]
        else:
            order = ["B", "A"]
        # Normalize GT to relative paths.
        gt_raw = task.get("ground_truth", [])
        gt = []
        for g in gt_raw:
            n = normalize(g.split(":")[0]) if ":" in g else normalize(g)
            if ":" in g:
                n = f"{n}:{g.split(':')[1]}"
            gt.append(n)
        print(f"  task {i+1}/{len(tasks)} id={task['id']} cat={task['category']} "
              f"(gt_size={len(gt)})")
        for cond in order:
            print(f"    cond={cond} ... ", end="", flush=True)
            run = run_task(task, cond, TOKIO_CORPUS, args.model)
            preds = run.get("predictions", [])
            jac = jaccard(preds, gt)
            ok = "OK" if not run.get("parsed", None) is None or run.get("content") else "?"
            print(f"t={run['elapsed']:.1f}s jaccard={jac:.3f} preds={len(preds)} "
                  f"wc={run['weighted_cost']} pt={run['tokens_in']} "
                  f"ct={run['tokens_out']} [{ok}]")
            run.update({"task_id": task["id"], "task_category": task["category"],
                        "condition": cond,
                        "ground_truth_size": len(gt), "jaccard": jac})
            # Truncate content to save space
            if "content" in run:
                run["content"] = run["content"][:1000]
            out_f.write(json.dumps(run) + "\n")
            out_f.flush()
    out_f.close()
    print(f"\n=== Done. Output: {out_path} ===")


if __name__ == "__main__":
    main()