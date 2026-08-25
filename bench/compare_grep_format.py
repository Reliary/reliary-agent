"""Arc 40 — Grep-format LLM utility bench.

Same apples-to-apples framework as compare_cost.py, but reliary uses
`format: "grep"` which returns plain `file:line: code` lines (top-10)
instead of JSON. The LLM processes this identically to how it processes
`grep -rn` output — a format it has seen billions of times in training.

Three conditions:
- A: reliary grep-format (1 call, top-10, type-flow ranked)
- B: altbackend JSON (their normal output)
- C: bash grep (real `grep -rn` output, alphabetical)

Same prompt for all three: "Filter to find references matching anchor's role."
Any difference in jaccard is due to RANKING QUALITY, not prompt engineering.
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
from llm_conn import deepseek_chat, mcp_call, TOKIO_CORPUS, HOMONYMS_FIXTURE

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
    """Independent oracle (same as compare_cost.py)."""
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


def build_questions(n=20):
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


# === Grep-style prompt: works with grep-format output naturally ===

COMMON_PROMPT = """You are a precise code analyst.

The tool output below is in grep format: `relative/path:line: code`. Each line is one candidate reference. The candidates are ranked — relevant hits are usually near the top, but you must read the source code (after the second `:`) to decide which references match the anchor's role.

Output a single JSON object on one line, no other text:
{"references": [{"file": "<relative path>", "line": <int>}]}

Include all references that match the anchor's role. Do not include the anchor line itself in your output."""


def run_condition_reliary_grep(question, model):
    """cond A: reliary in grep format."""
    stem = question["stem"]
    anchor_file = normalize(question["anchor_file"])
    anchor_line = question["anchor_line"]
    # Pre-fetch in grep format
    r = mcp_call("reliary_find_references_with_source",
                  {"name": stem, "anchor_file": anchor_file,
                   "anchor_line": anchor_line, "path": ".",
                   "threshold": 0.1, "format": "grep", "limit": 50},
                  workdir=TOKIO_CORPUS, timeout=60)
    text = r.get("content", [{}])[0].get("text", "") if r else ""
    if not text:
        return _empty("no tool output")
    user = (
        f"Question: {question['question']}\n\n"
        f"Tool output (grep format):\n{text}\n\n"
        f"Output a single JSON object on one line."
    )
    return _call_llm(COMMON_PROMPT, user, model, tool_output_size=len(text))


def run_condition_altbackend(question, model):
    """cond B: altbackend tools (multi-turn, JSON)."""
    stem = question["stem"]
    anchor_file = normalize(question["anchor_file"])
    # Pre-fetch
    search = subprocess.run([ALTBACKEND_BIN, "cli", "search_graph",
                              json.dumps({"project": "tmp-tokio-corpus-tokio-src",
                                          "query": stem, "limit": 20})],
                             capture_output=True, text=True, timeout=30)
    try:
        search_data = json.loads(search.stdout)
    except Exception:
        search_data = {"results": []}
    search_text = json.dumps(search_data, indent=2)[:3000]

    sys = (
        f"You are a precise code analyst. You have access to altbackend tools. "
        f"Output a single JSON object on one line, no other text: "
        f'{{"references": [{{"file": "<relative path>", "line": <int>}}, ...]}}'
    )
    user = (
        f"Question: {question['question']}\n\n"
        f"altbackend search_graph results:\n{search_text}\n\n"
        f"Pick references matching the anchor's role. Output a single JSON object on one line."
    )
    return _call_llm(sys, user, model, tool_output_size=len(search_text))


def run_condition_grep(question, model):
    """cond C: bash grep."""
    stem = question["stem"]
    r = subprocess.run(
        ["grep", "-rEn", rf"\b{stem}\b", TOKIO_CORPUS, "--include=*.rs"],
        capture_output=True, text=True, timeout=15)
    raw_lines = r.stdout.strip().split("\n")[:50]
    formatted = []
    for line in raw_lines:
        parts = line.split(":", 2)
        if len(parts) >= 3:
            file_path = normalize(parts[0])
            ln = parts[1]
            formatted.append(f"{file_path}:{ln}: {parts[2].rstrip()}")
    text = "\n".join(formatted)
    if not text:
        return _empty("no grep output")
    user = (
        f"Question: {question['question']}\n\n"
        f"grep output:\n{text}\n\n"
        f"Output a single JSON object on one line."
    )
    return _call_llm(COMMON_PROMPT, user, model, tool_output_size=len(text))


def _empty(reason):
    return {"elapsed": 0, "content": "", "tokens_in": 0, "tokens_out": 0,
            "weighted_cost": 0, "parsed": None, "predictions": [],
            "tool_calls": 0, "tool_output_size": 0, "error": reason}


def _call_llm(sys, user, model, tool_output_size):
    messages = [{"role": "system", "content": sys},
                {"role": "user", "content": user}]
    t0 = time.time()
    resp = deepseek_chat(messages, model=model, max_tokens=2000, timeout=60,
                          disable_thinking=True)
    elapsed = time.time() - t0
    if "error" in resp:
        return {"elapsed": elapsed, "response": resp,
                "tokens_in": 0, "tokens_out": 0, "weighted_cost": 0,
                "content": "", "parsed": None, "predictions": [],
                "tool_calls": 1, "tool_output_size": tool_output_size}
    msg = resp.get("choices", [{}])[0].get("message", {})
    content = msg.get("content", "") or msg.get("reasoning_content", "")
    usage = resp.get("usage", {})
    pt = usage.get("prompt_tokens", 0)
    ct = usage.get("completion_tokens", 0)
    parsed = _parse_llm_json(content)
    return {"elapsed": elapsed, "content": content,
            "tokens_in": pt, "tokens_out": ct,
            "weighted_cost": pt + 4 * ct,
            "parsed": parsed, "predictions": _extract_refs(parsed),
            "tool_calls": 1, "tool_output_size": tool_output_size}


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
    ("A", "reliary_grep", run_condition_reliary_grep),
    ("B", "altbackend", run_condition_altbackend),
    ("C", "grep", run_condition_grep),
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--n", type=int, default=20)
    parser.add_argument("--model", default="deepseek-v4-flash")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    questions = build_questions(args.n)
    rng = random.Random(args.seed)
    rng.shuffle(questions)

    if args.out is None:
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        out_path = RESULTS_DIR / f"arc40-grep-format-{ts}.jsonl"
    else:
        out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    print(f"=== Arc 40 Grep-Format LLM Utility Bench ===")
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
            preds = run.get("predictions", [])
            jac = jaccard(preds, gt)
            print(f"t={run['elapsed']:.1f}s j={jac:.3f} preds={len(preds)} "
                  f"wc={run['weighted_cost']} pt={run['tokens_in']} "
                  f"ct={run['tokens_out']} tosz={run['tool_output_size']}")
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