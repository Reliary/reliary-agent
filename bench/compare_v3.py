"""Arc 28 Lever 6 v3 — Direct DeepSeek comparison with INDEPENDENT grep oracle.

Per workspace WORKFLOW_RULES: interleaved A/B per task.
Per project memory: weighted cost = prompt + 4 × completion tokens.

INDEPENDENT ORACLE (fixes v2's circular measurement):
  GT = grep -rEn "\\b{stem}\\b" <corpus> --include=*.rs
       filtered for: non-test files, non-doc-comments, non-anchor-line

Single-turn direct DeepSeek via api.deepseek.com (NOT reliary.dev).
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
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from llm_conn import (deepseek_chat, mcp_call, TOKIO_CORPUS,
                       HOMONYMS_FIXTURE)

RESULTS_DIR = Path("/home/user/src/reliary8/bench/results")
ALTBACKEND_BIN = "/home/user/.local/bin/altbackend-mcp"


def normalize(p):
    if not p:
        return ""
    p = str(p).strip()
    if p.startswith(TOKIO_CORPUS):
        p = p[len(TOKIO_CORPUS):].lstrip("/")
    return p


def is_doc_comment(line_text):
    s = line_text.strip()
    return (s.startswith("///") or s.startswith("//!")
            or s.startswith("//"))


def is_in_test(file_path):
    return ("/tests/" in file_path or "_test.rs" in file_path
            or file_path.endswith("tests.rs"))


def strict_grep_oracle(stem, anchor_file=None, anchor_line=None):
    """Independent oracle. NOT derived from either backend."""
    try:
        r = subprocess.run(
            ["grep", "-rEn", rf"\b{stem}\b", TOKIO_CORPUS, "--include=*.rs"],
            capture_output=True, text=True, timeout=15)
    except Exception:
        return []
    refs = []
    for line in r.stdout.split("\n"):
        parts = line.split(":", 2)
        if len(parts) < 3:
            continue
        try:
            ln = int(parts[1])
        except ValueError:
            continue
        file_path = normalize(parts[0])
        line_text = parts[2]
        if is_in_test(file_path):
            continue
        if is_doc_comment(line_text):
            continue
        if anchor_file and anchor_line:
            anchor_path = normalize(anchor_file)
            if file_path == anchor_path and ln == anchor_line:
                continue
        refs.append(f"{file_path}:{ln}")
    seen = set()
    return [r for r in refs if not (r in seen or seen.add(r))]


def build_task_suite(min_gt=10, max_gt=80):
    """Build find_references tasks with independent grep oracle."""
    with open(HOMONYMS_FIXTURE) as f:
        data = json.load(f)
    anchors = [a for a in data["anchors"]
               if a.get("audit_status") != "unbenchable"]
    tasks = []
    for a in anchors:
        gt = strict_grep_oracle(a["stem"], a["anchor_file"], a["anchor_line"])
        if min_gt <= len(gt) <= max_gt:
            tasks.append({
                "id": a["id"],
                "category": "find_references",
                "stem": a["stem"],
                "anchor_file": a["anchor_file"],
                "anchor_line": a["anchor_line"],
                "use_label": a["use_label"],
                "ground_truth": gt,
            })
    return tasks


def run_altbackend_cli(tool, args, timeout=30):
    r = subprocess.run([ALTBACKEND_BIN, "cli", tool, json.dumps(args)],
                        capture_output=True, text=True, timeout=timeout)
    try:
        return json.loads(r.stdout)
    except Exception:
        return {"error": r.stdout[-300:]}


def fetch_reliary_output(stem, anchor_file, anchor_line, workdir=TOKIO_CORPUS,
                          with_source=False, max_hits=20):
    """Fetch hits with (file, line, similarity). If with_source=True,
    also include the actual source line text — LLM-native format."""
    rel = normalize(anchor_file)
    if with_source:
        tool = "reliary_find_references_with_source"
        r = mcp_call(tool,
                      {"name": stem, "anchor_file": rel,
                       "anchor_line": anchor_line, "path": workdir,
                       "threshold": 0.1, "context": 0},
                      workdir=workdir, timeout=60)
        hits = r.get("hits", [])[:max_hits]
        return "\n".join(f"  {normalize(h.get('file'))}:{h.get('line', 0)}  sim={h.get('similarity', 0):.3f}  | {h.get('source', '').strip()}"
                          for h in hits)
    r = mcp_call("reliary_find_references_type_flow",
                  {"name": stem, "anchor_file": rel,
                   "anchor_line": anchor_line, "path": workdir,
                   "threshold": 0.1},
                  workdir=workdir, timeout=60)
    hits = r.get("hits", [])[:max_hits]
    return "\n".join(f"  {normalize(h.get('file'))}:{h.get('line', 0)}  sim={h.get('similarity', 0):.3f}"
                      for h in hits)


def fetch_altbackend_output(stem, workdir=TOKIO_CORPUS):
    r = run_altbackend_cli("search_graph", {
        "project": "tmp-tokio-corpus-tokio-src",
        "query": stem,
        "limit": 50,
    })
    results = r.get("results", [])
    return "\n".join(f"  {normalize(res.get('file_path'))}:{res.get('start_line', 0)}"
                      for res in results[:50])


SYSTEM_PROMPT = """You are a precise code analyst. Given tool output listing file paths, line numbers, and source code snippets, identify the references that match the same ROLE as the anchor definition.

Output a single JSON object on one line, no other text:
{"references": [{"file": "<relative path>", "line": <int>}, ...]}

Rules:
- Drop test files (paths containing /tests/ or _test.rs).
- Drop doc comment lines (lines starting with ///, //!, //).
- Drop the anchor's own line.
- Match the role of the anchor (function_def vs method_call vs type_name).
- IMPORTANT: Include ALL references that share the anchor's ROLE, even if the surrounding code looks different. Trust the tool's similarity ranking — if a hit is in the top-50, it's likely a valid reference.
- When source is provided, use it ONLY to disambiguate role, not to filter by style."""


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
             parsed.get("files") or [])
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
    p, g = set(pred), set(gt)
    if not (p | g):
        return 0.0
    return len(p & g) / len(p | g)


def precision(pred, gt):
    if not pred:
        return 0.0
    return len(set(pred) & set(gt)) / len(set(pred))


def recall(pred, gt):
    if not gt:
        return 0.0
    return len(set(pred) & set(gt)) / len(set(gt))


def call_llm(system, user, model="deepseek-chat"):
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


def run_task(task, condition, model):
    """Run a single task with the given backend's pre-fetched output."""
    stem = task["stem"]
    anchor_file = task["anchor_file"]
    anchor_line = task["anchor_line"]
    use_label = task["use_label"]
    gt = task["ground_truth"]

    if condition == "A":
        tool_output = fetch_reliary_output(stem, anchor_file, anchor_line,
                                            with_source=True, max_hits=50)
        tool_name = "reliary_find_references_with_source (top-50)"
    else:
        tool_output = fetch_altbackend_output(stem)
        tool_name = "altbackend_search_graph"

    user = (
        f"Anchor: `{anchor_file}:{anchor_line}` (label={use_label}, "
        f"stem=`{stem}`).\n\n"
        f"{tool_name} output:\n{tool_output}\n\n"
        f"Output a single JSON object on one line."
    )

    run = call_llm(SYSTEM_PROMPT, user, model)
    run["gt_size"] = len(gt)
    run["gt_sample"] = gt[:3]
    run["tool_output_size"] = len(tool_output)
    run["tool_name"] = tool_name
    return run


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--n", type=int, default=10)
    parser.add_argument("--model", default="deepseek-chat")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--min-gt", type=int, default=10)
    parser.add_argument("--max-gt", type=int, default=80)
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    # Build or load task suite.
    tasks = build_task_suite(min_gt=args.min_gt, max_gt=args.max_gt)
    rng = random.Random(args.seed)
    rng.shuffle(tasks)
    tasks = tasks[:args.n]
    suite_path = RESULTS_DIR / "compare_tasks_v3.json"
    with open(suite_path, "w") as f:
        json.dump(tasks, f, indent=2)

    if args.out is None:
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        out_path = RESULTS_DIR / f"compare_v3_{ts}.jsonl"
    else:
        out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    print(f"=== Arc 28 Lever 6 v3 — Independent Grep Oracle ===")
    print(f"Tasks: {len(tasks)} (built with GT size {args.min_gt}-{args.max_gt})")
    print(f"Model: {args.model}")
    print(f"Output: {out_path}\n")

    out_f = open(out_path, "w")
    for i, task in enumerate(tasks):
        if i % 2 == 0:
            order = ["A", "B"]
        else:
            order = ["B", "A"]
        gt = task["ground_truth"]
        print(f"  task {i+1}/{len(tasks)} id={task['id']} stem={task['stem']} "
              f"label={task['use_label']} (gt_size={len(gt)})")
        for cond in order:
            print(f"    cond={cond} ... ", end="", flush=True)
            run = run_task(task, cond, args.model)
            preds = run.get("predictions", [])
            jac = jaccard(preds, gt)
            prec = precision(preds, gt)
            rec = recall(preds, gt)
            print(f"t={run['elapsed']:.1f}s j={jac:.3f} p={prec:.3f} "
                  f"r={rec:.3f} preds={len(preds)} wc={run['weighted_cost']} "
                  f"pt={run['tokens_in']} ct={run['tokens_out']}")
            run.update({"task_id": task["id"], "stem": task["stem"],
                        "use_label": task["use_label"], "condition": cond,
                        "jaccard": jac, "precision": prec, "recall": rec})
            if "content" in run:
                run["content"] = run["content"][:1000]
            out_f.write(json.dumps(run) + "\n")
            out_f.flush()
            if "response" in run and "rate limit" in str(run["response"]).lower():
                print("    RATE LIMITED — stop")
                out_f.close()
                return 1
    out_f.close()
    print(f"\n=== Done. Output: {out_path} ===")


if __name__ == "__main__":
    main()