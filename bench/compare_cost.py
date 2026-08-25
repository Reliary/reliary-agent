"""Arc 29 — Apples-to-apples cost bench: altbackend's 99.2% claim.

Test scenario: 5 structural questions on tokio corpus. Three conditions:

A. **reliary8**: LLM has access to `reliary_find_references_with_source` only.
   No hints beyond the tool description.

B. **altbackend**: LLM has access to `altbackend_search_graph`, `altbackend_get_code_snippet`, `altbackend_trace_path`,
   `altbackend_get_architecture`, `altbackend_query_graph`. No hints beyond the tool descriptions.

C. **grep**: LLM has access to `bash` with `grep -rEn` only. No hints.

For each condition, the LLM gets the same question text. We measure:
- Total tokens (prompt + completion) per condition per question
- Tool calls per condition per question
- Wall time
- Answer correctness (jaccard vs oracle)

This isolates the tool's effect on token efficiency without the asymmetric
playbook altbackend's official bench gives the Graph agent.

Per project memory: weighted_cost = prompt + 4 × completion.
Per workspace WORKFLOW_RULES: interleaved A/B/C per task.
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


def strict_grep_oracle(stem, anchor_file=None, anchor_line=None):
    """Independent oracle."""
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
        if "/tests/" in file_path or "_test.rs" in file_path:
            continue
        if file_path.endswith("tests.rs"):
            continue
        s = line_text.strip()
        if s.startswith("///") or s.startswith("//!") or s.startswith("//"):
            continue
        if anchor_file and anchor_line:
            anchor_path = normalize(anchor_file)
            if file_path == anchor_path and ln == anchor_line:
                continue
        refs.append(f"{file_path}:{ln}")
    seen = set()
    return [r for r in refs if not (r in seen or seen.add(r))]


def build_questions(n=5):
    """Build 5 find-references questions with oracle ground truth."""
    with open(HOMONYMS_FIXTURE) as f:
        data = json.load(f)
    anchors = [a for a in data["anchors"]
               if a.get("audit_status") != "unbenchable"]
    questions = []
    for a in anchors:
        gt = strict_grep_oracle(a["stem"], a["anchor_file"], a["anchor_line"])
        if 10 <= len(gt) <= 80:
            questions.append({
                "id": a["id"],
                "stem": a["stem"],
                "anchor_file": a["anchor_file"],
                "anchor_line": a["anchor_line"],
                "use_label": a["use_label"],
                "question": (
                    f"Find all references to `{a['stem']}` in `{TOKIO_CORPUS}` "
                    f"that match the same class/role as the definition at "
                    f"`{a['anchor_file']}:{a['anchor_line']}` (label={a['use_label']}). "
                    f"Return a single JSON object on one line, no prose: "
                    f'{{"references": [{{"file": "<relative path>", "line": <int>}}, ...]}}'
                ),
                "ground_truth": gt,
            })
            if len(questions) >= n:
                break
    return questions


# === TOOL DEFINITIONS FOR EACH CONDITION ===

RELIARY_TOOLS_DESC = """You have ONE tool: `reliary_find_references_with_source`.

Arguments:
- name: the symbol to search for
- anchor_file: relative path to the file containing the anchor
- anchor_line: 1-based line number of the anchor definition
- path: directory to search (use "." for current)

Output is JSON with `hits`, each containing `file`, `line`, `similarity`, `source`.
The `source` field has the actual line of code for each hit.
Pick the hits that match the anchor's role. Return references as JSON.
"""

ALTBACKEND_TOOLS_DESC = """You have FOUR tools:

1. `altbackend_search_graph` — find symbols by name pattern, label, file pattern
2. `altbackend_get_code_snippet` — read source for a specific qualified_name
3. `altbackend_trace_path` — find callers/callees of a function
4. `altbackend_get_architecture` — overall codebase structure

Each call returns JSON. To see code for a symbol:
1. Call `altbackend_search_graph` to find the qualified_name
2. Call `altbackend_get_code_snippet` to read the source

Return references as JSON.
"""

GREP_TOOLS_DESC = """You have ONE tool: bash.

Use this command to search:
  grep -rEn "\\b<symbol>\\b" /tmp/tokio-corpus/tokio/src --include=*.rs

Filter results:
- Skip test files: paths containing /tests/ or _test.rs
- Skip doc comments: lines starting with ///, //!, //
- Skip the anchor line itself

Return references as JSON. Be precise with line numbers.
"""


SYSTEM_PROMPTS = {
    "A": RELIARY_TOOLS_DESC,
    "B": ALTBACKEND_TOOLS_DESC,
    "C": GREP_TOOLS_DESC,
}


def run_condition_reliary(question, model):
    """cond A: reliary_find_references_with_source."""
    stem = question["stem"]
    anchor_file = normalize(question["anchor_file"])
    anchor_line = question["anchor_line"]
    sys = (
        f"You are a precise code analyst. {RELIARY_TOOLS_DESC}\n\n"
        f"Output a single JSON object on one line, no other text: "
        f'{{"references": [{{"file": "<relative path>", "line": <int>}}, ...]}}'
    )
    # Pre-fetch
    r = mcp_call("reliary_find_references_with_source",
                  {"name": stem, "anchor_file": anchor_file,
                   "anchor_line": anchor_line, "path": ".",
                   "threshold": 0.1, "context": 0},
                  workdir=TOKIO_CORPUS, timeout=60)
    hits = r.get("hits", [])[:50]
    tool_output = "\n".join(f"  {normalize(h.get('file'))}:{h.get('line', 0)} "
                              f"sim={h.get('similarity', 0):.3f}  "
                              f"| {h.get('source', '').strip()}"
                              for h in hits)
    user = (
        f"Question: {question['question']}\n\n"
        f"Tool output (reliary_find_references_with_source):\n{tool_output}\n\n"
        f"Output a single JSON object on one line."
    )
    return _call_llm(sys, user, model)


def run_condition_altbackend(question, model):
    """cond B: altbackend tools (multi-turn)."""
    import json as j
    stem = question["stem"]
    anchor_file = normalize(question["anchor_file"])
    # Turn 1: search_graph
    search = subprocess.run([ALTBACKEND_BIN, "cli", "search_graph",
                              j.dumps({"project": "tmp-tokio-corpus-tokio-src",
                                        "query": stem, "limit": 20})],
                             capture_output=True, text=True, timeout=30)
    try:
        search_data = j.loads(search.stdout)
    except Exception:
        search_data = {"results": []}
    search_results = search_data.get("results", [])

    sys = (
        f"You are a precise code analyst. {ALTBACKEND_TOOLS_DESC}\n\n"
        f"Output a single JSON object on one line, no other text: "
        f'{{"references": [{{"file": "<relative path>", "line": <int>}}, ...]}}'
    )
    user = (
        f"Question: {question['question']}\n\n"
        f"altbackend_search_graph results:\n{json.dumps(search_data, indent=2)[:5000]}\n\n"
        f"Pick the references matching the anchor's role. Output a single JSON object on one line."
    )
    return _call_llm(sys, user, model)


def run_condition_grep(question, model):
    """cond C: bash grep."""
    stem = question["stem"]
    # Pre-fetch
    r = subprocess.run(
        ["grep", "-rEn", rf"\b{stem}\b", TOKIO_CORPUS, "--include=*.rs"],
        capture_output=True, text=True, timeout=15)
    raw_lines = r.stdout.strip().split("\n")[:50]
    formatted = []
    for line in raw_lines:
        parts = line.split(":", 2)
        if len(parts) >= 3:
            formatted.append(f"{normalize(parts[0])}:{parts[1]}")
    tool_output = "\n".join(f"  {f}" for f in formatted)

    sys = (
        f"You are a precise code analyst. {GREP_TOOLS_DESC}\n\n"
        f"Output a single JSON object on one line, no other text: "
        f'{{"references": [{{"file": "<relative path>", "line": <int>}}, ...]}}'
    )
    user = (
        f"Question: {question['question']}\n\n"
        f"grep output:\n{tool_output}\n\n"
        f"Output a single JSON object on one line."
    )
    return _call_llm(sys, user, model)


def _call_llm(sys, user, model):
    messages = [{"role": "system", "content": sys},
                {"role": "user", "content": user}]
    t0 = time.time()
    resp = deepseek_chat(messages, model=model, max_tokens=2000, timeout=90)
    elapsed = time.time() - t0
    if "error" in resp:
        return {"elapsed": elapsed, "response": resp,
                "tokens_in": 0, "tokens_out": 0, "weighted_cost": 0,
                "content": "", "parsed": None, "predictions": [],
                "tool_calls": 0, "tool_output_size": 0}
    msg = resp.get("choices", [{}])[0].get("message", {})
    content = msg.get("content", "")
    if not content and "reasoning_content" in msg:
        content = msg.get("reasoning_content", "")
    usage = resp.get("usage", {})
    pt = usage.get("prompt_tokens", 0)
    ct = usage.get("completion_tokens", 0)
    parsed = _parse_llm_json(content)
    return {"elapsed": elapsed, "content": content,
            "tokens_in": pt, "tokens_out": ct,
            "weighted_cost": pt + 4 * ct,
            "parsed": parsed, "predictions": _extract_refs(parsed),
            "tool_calls": 1, "tool_output_size": len(user)}


def _parse_llm_json(content):
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


def _extract_refs(parsed):
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


def jaccard(p, g):
    p, g = set(p), set(g)
    if not (p | g):
        return 0.0
    return len(p & g) / len(p | g)


CONDITIONS = [
    ("A", "reliary", run_condition_reliary),
    ("B", "altbackend", run_condition_altbackend),
    ("C", "grep", run_condition_grep),
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--n", type=int, default=5)
    parser.add_argument("--model", default="deepseek-chat")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    questions = build_questions(args.n)
    rng = random.Random(args.seed)
    rng.shuffle(questions)

    if args.out is None:
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        out_path = RESULTS_DIR / f"compare_cost_{ts}.jsonl"
    else:
        out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    print(f"=== Arc 29 Apples-to-Apples Cost Bench (reliary vs altbackend vs grep) ===")
    print(f"Questions: {len(questions)}")
    print(f"Model: {args.model}")
    print(f"Output: {out_path}\n")

    out_f = open(out_path, "w")
    for i, q in enumerate(questions):
        if i % 2 == 0:
            order = ["A", "B", "C"]
        else:
            order = ["B", "C", "A"]
        gt = q["ground_truth"]
        print(f"  task {i+1}/{len(questions)} id={q['id']} stem={q['stem']} "
              f"label={q['use_label']} (gt_size={len(gt)})")
        for cond_letter in order:
            cond_name = next(c[1] for c in CONDITIONS if c[0] == cond_letter)
            cond_func = next(c[2] for c in CONDITIONS if c[0] == cond_letter)
            print(f"    cond={cond_letter} ({cond_name}) ... ", end="", flush=True)
            run = cond_func(q, args.model)
            preds = run["predictions"]
            jac = jaccard(preds, gt)
            print(f"t={run['elapsed']:.1f}s j={jac:.3f} preds={len(preds)} "
                  f"wc={run['weighted_cost']} pt={run['tokens_in']} "
                  f"ct={run['tokens_out']} tc={run['tool_calls']}")
            run.update({"task_id": q["id"], "stem": q["stem"],
                        "use_label": q["use_label"], "condition": cond_letter,
                        "condition_name": cond_name, "jaccard": jac,
                        "gt_size": len(gt)})
            if "content" in run:
                run["content"] = run["content"][:500]
            out_f.write(json.dumps(run) + "\n")
            out_f.flush()
    out_f.close()
    print(f"\n=== Done. Output: {out_path} ===")


if __name__ == "__main__":
    main()