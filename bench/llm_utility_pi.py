"""Arc 28 Lever 6 — Pi-driven LLM utility bench.

Paired benchmark: A (Pi + grep) vs B (Pi + reliary MCP).

Mirrors /home/user/src/reliary-agent/scripts/bench_paired.py pattern:
- DEEPSEEK_API_KEY set explicitly via env (fallback to known key).
- subprocess.run([PI, "--model", "deepseek/deepseek-v4-flash",
                  "--mode", "json", "--session", sfile, "--print", task])
- Parse usage from message_end events.
- 600s timeout per task.

Constraint (per user):
- Direct DeepSeek via api.deepseek.com (NOT api.reliary.dev, NOT deepinfra).
- Reliary MCP via Pi extension + stdio subprocess (no daemon port).
- Use Pi agent per harness convention.

Usage:
  python3 bench/llm_utility_pi.py [--n N] [--corpus tokio|hyper] [--seed S] [--out PATH]
"""
import argparse
import json
import os
import random
import subprocess
import sys
import time
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from llm_conn import (PI_BIN, PI_SETTINGS, RELIARY_BIN, TOKIO_CORPUS, HYPER_CORPUS,
                       HOMONYMS_FIXTURE, mcp_call, set_pi_packages, DEEPSEEK_API_KEY_FALLBACK)

PI_DISABLE_HEARTBEAT = "1"
RESULTS_DIR = "/home/user/src/reliary8/bench/results"


def set_pi_ext(ext_path):
    """Mirror bench_paired.py: mutate ~/.pi/agent/settings.json extensions field."""
    base = os.path.expanduser("~/.pi/agent")
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


def load_anchors(corpus, max_gt_size=200):
    with open(HOMONYMS_FIXTURE) as f:
        data = json.load(f)
    if corpus == "tokio":
        anchors = data["anchors"]
        workdir = TOKIO_CORPUS
    elif corpus == "hyper":
        anchors = build_hyper_anchors()
        workdir = HYPER_CORPUS
    else:
        raise ValueError(f"Unknown corpus: {corpus}")
    anchors = [a for a in anchors if a.get("audit_status") != "unbenchable"]
    # Pre-compute GT sizes and filter to small ones (LLM must enumerate).
    filtered = []
    for a in anchors:
        try:
            gt = ground_truth_for_anchor(a, workdir, threshold=0.1)
            a["_gt_size"] = len(gt)
            if 5 <= len(gt) <= max_gt_size:
                filtered.append(a)
        except Exception:
            pass
    return filtered, workdir


def build_hyper_anchors():
    import sqlite3
    db_path = f"{HYPER_CORPUS}/.reliary/index.sqlite"
    if not os.path.exists(db_path):
        return []
    anchors = []
    try:
        conn = sqlite3.connect(db_path)
        cur = conn.cursor()
        cur.execute("""
            SELECT p.phrase, COUNT(*) AS c
            FROM occurrence o JOIN phrases p ON p.id = o.phrase_id
            WHERE o.is_def = 1
            GROUP BY p.phrase
            ORDER BY c DESC
            LIMIT 30
        """)
        rows = cur.fetchall()
        for stem, count in rows:
            cur.execute("""
                SELECT f.file_path, o.line, o.tag
                FROM occurrence o JOIN phrases p ON p.id = o.phrase_id
                JOIN file_map f ON f.id = o.file_id
                WHERE p.phrase = ? AND o.is_def = 1
                LIMIT 1
            """, (stem,))
            r = cur.fetchone()
            if r:
                use_label = ("function_def" if r[2] in (1, 3) else
                              "type_name" if r[2] == 2 else
                              "method_call" if r[2] == 0 else "function_def")
                anchors.append({
                    "id": f"hyper-{len(anchors)+1:03d}",
                    "stem": stem,
                    "anchor_file": r[0],
                    "anchor_line": r[1],
                    "use_label": use_label,
                })
        conn.close()
    except Exception as e:
        print(f"WARN: hyper anchor build failed: {e}")
    return anchors


def ground_truth_for_anchor(anchor, workdir, threshold=0.1):
    """Run reliary_find_references_type_flow as oracle."""
    rel_path = anchor["anchor_file"]
    if rel_path.startswith(workdir):
        rel_path = rel_path[len(workdir):].lstrip("/")
    r = mcp_call("reliary_find_references_type_flow",
                  {"name": anchor["stem"], "anchor_file": rel_path,
                   "anchor_line": anchor["anchor_line"], "path": workdir,
                   "threshold": threshold},
                  workdir=workdir, timeout=60)
    return [f"{h['file']}:{h['line']}" for h in r.get("hits", [])]


def build_pi_prompt(anchor, workdir, condition):
    """Build the task prompt for Pi. Embeds tool instructions inline."""
    rel_anchor = anchor["anchor_file"]
    if rel_anchor.startswith(workdir):
        rel_anchor = rel_anchor[len(workdir):].lstrip("/")
    if condition == "A":
        return (
            f"Task: Find all references to `{anchor['stem']}` in the codebase at "
            f"`{workdir}` that match the same class as the definition at "
            f"`{anchor['anchor_file']}:{anchor['anchor_line']}` "
            f"(label={anchor['use_label']}).\n\n"
            f"You have access to `bash` and `read` tools. Use "
            f"`grep -rn '<stem>' /tmp/tokio-corpus/tokio/src/ | head -100` to find references. "
            f"Read each candidate file to verify the reference is the same class.\n\n"
            f"REQUIRED OUTPUT FORMAT (single line, no other text):\n"
            f'{{"references": [{{"file": "<path relative to workdir>", "line": <int>}}, ...]}}\n\n'
            f"Anchor file: {rel_anchor}\n"
            f"Anchor line: {anchor['anchor_line']}\n"
        )
    else:
        return (
            f"Task: Find all references to `{anchor['stem']}` in the codebase at "
            f"`{workdir}` that match the same class as the definition at "
            f"`{anchor['anchor_file']}:{anchor['anchor_line']}` "
            f"(label={anchor['use_label']}).\n\n"
            f"You have access to reliary MCP tools and `bash`. "
            f"Use `reliary_find_references_type_flow` with:\n"
            f'  name: "{anchor["stem"]}"\n'
            f"  anchor_file: (relative path)\n"
            f"  anchor_line: (integer)\n"
            f"  path: (workdir)\n"
            f"  threshold: 0.3\n\n"
            f"REQUIRED OUTPUT FORMAT (single line, no other text):\n"
            f'{{"references": [{{"file": "<path relative to workdir>", "line": <int>}}, ...]}}\n\n'
            f"Anchor file: {rel_anchor}\n"
            f"Anchor line: {anchor['anchor_line']}\n"
        )


def parse_usage(stdout):
    """Mirror bench_paired.py parse_usage: count tokens from message_end events."""
    pt = ct = tc = 0
    for line in stdout.splitlines():
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
            if d.get("type") == "message_end":
                u = d.get("message", {}).get("usage", {})
                pt += u.get("input", 0)
                ct += u.get("output", 0)
                if "toolName" in d.get("message", {}):
                    tc += 1
            elif d.get("type") == "tool_execution_start":
                tc += 1
        except Exception:
            pass
    return pt, ct, tc


def extract_final_text(stdout):
    """Pull the last assistant text content from Pi's JSONL stream."""
    texts = []
    for line in stdout.splitlines():
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
            if d.get("type") == "message_end":
                content = d.get("message", {}).get("content", [])
                if isinstance(content, list):
                    for c in content:
                        if isinstance(c, dict) and c.get("type") == "text":
                            texts.append(c.get("text", ""))
        except Exception:
            pass
    return "\n".join(texts)


def parse_llm_json(content):
    content = (content or "").strip()
    if not content:
        return None
    try:
        return json.loads(content)
    except Exception:
        pass
    import re
    m = re.search(r"\{[\s\S]*\}", content)
    if m:
        try:
            return json.loads(m.group(0))
        except Exception:
            pass
    return None


def extract_references(parsed):
    if not isinstance(parsed, dict):
        return []
    refs = parsed.get("references", [])
    if not isinstance(refs, list):
        return []
    out = []
    for r in refs:
        if isinstance(r, dict):
            f = r.get("file", "")
            l = r.get("line", 0)
            if f and l:
                out.append(f"{f}:{l}")
    return out


def jaccard(predicted, ground_truth):
    if not predicted and not ground_truth:
        return 0.0
    p, g = set(predicted), set(ground_truth)
    if not (p | g):
        return 0.0
    return len(p & g) / len(p | g)


def run_pi_condition(task, workdir, condition, model, timeout=180):
    """Mirror bench_paired.py run_condition: subprocess.run with --print --mode json."""
    sfile = f"/tmp/llm-utility-{int(time.time()*1000)}-{condition}.json"
    if os.path.exists(sfile):
        os.remove(sfile)
    ext_path = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                             "reliary_mcp_pi_extension.js")
    if condition == "A":
        set_pi_ext(None)
    else:
        set_pi_ext(ext_path)

    env = os.environ.copy()
    env["PI_DISABLE_HEARTBEAT"] = PI_DISABLE_HEARTBEAT
    env["DEEPSEEK_API_KEY"] = DEEPSEEK_API_KEY_FALLBACK
    env.pop("RELIARY_PROXY_ACTIVE", None)
    env.pop("OPENAI_BASE_URL", None)
    env.pop("DEEPSEEK_BASE_URL", None)
    env.pop("RELIARY_MODE", None)
    env["RELIARY_BIN"] = RELIARY_BIN
    env["RELIARY_WORKDIR"] = workdir

    t0 = time.time()
    try:
        result = subprocess.run(
            [PI_BIN, "--model", model, "--mode", "json",
             "--session", sfile, "--print", task],
            cwd=workdir, capture_output=True, text=True,
            timeout=timeout, env=env,
        )
    except subprocess.TimeoutExpired:
        return {"condition": condition, "elapsed": time.time() - t0,
                "stdout": "", "stderr": "TIMEOUT", "returncode": -1,
                "parsed": None, "tokens_in": 0, "tokens_out": 0,
                "tool_calls": 0, "weighted_cost": 0}
    elapsed = time.time() - t0
    pt, ct, tc = parse_usage(result.stdout)
    final_text = extract_final_text(result.stdout)
    parsed = parse_llm_json(final_text)
    wc = pt + 4 * ct  # per project memory
    return {"condition": condition, "elapsed": elapsed,
            "stdout": result.stdout[-2000:], "stderr": result.stderr[-500:],
            "returncode": result.returncode, "parsed": parsed,
            "tokens_in": pt, "tokens_out": ct, "tool_calls": tc,
            "weighted_cost": wc, "final_text": final_text[:2000]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--n", type=int, default=30)
    parser.add_argument("--corpus", choices=["tokio", "hyper"], default="tokio")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--model", default="deepseek/deepseek-v4-flash")
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    if args.out is None:
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        out_path = os.path.join(RESULTS_DIR, f"llm_utility_{ts}.jsonl")
    else:
        out_path = args.out
    os.makedirs(os.path.dirname(out_path), exist_ok=True)

    rng = random.Random(args.seed)
    anchors, workdir = load_anchors(args.corpus)
    sample = rng.sample(anchors, min(args.n, len(anchors)))

    print(f"=== Arc 28 Lever 6 — LLM Utility Bench ===")
    print(f"Corpus: {args.corpus} ({len(anchors)} benchable)")
    print(f"Workdir: {workdir}")
    print(f"Tasks: {len(sample)} paired A/B interleaved")
    print(f"Model: {args.model}")
    print(f"Output: {out_path}\n")

    out_f = open(out_path, "w")
    stop = False
    for i, anchor in enumerate(sample):
        if i % 2 == 0:
            order = ["A", "B"]
        else:
            order = ["B", "A"]
        try:
            gt = ground_truth_for_anchor(anchor, workdir, threshold=0.1)
        except Exception as e:
            print(f"  task {i+1}/{len(sample)} anchor={anchor['id']} GT_ERROR: {e}")
            gt = []
        for cond in order:
            task_prompt = build_pi_prompt(anchor, workdir, cond)
            print(f"  task {i+1}/{len(sample)} anchor={anchor['id']} cond={cond} "
                  f"(gt={len(gt)}) ... ", end="", flush=True)
            run = run_pi_condition(task_prompt, workdir, cond, args.model,
                                    timeout=args.timeout)
            preds = extract_references(run.get("parsed"))
            jac = jaccard(preds, gt)
            run.update({"anchor_id": anchor["id"], "stem": anchor["stem"],
                        "use_label": anchor["use_label"],
                        "ground_truth_size": len(gt),
                        "predictions": preds, "jaccard": jac})
            out_f.write(json.dumps(run) + "\n")
            out_f.flush()
            status = "OK" if run["returncode"] == 0 else f"ERR({run['returncode']})"
            print(f"t={run['elapsed']:.1f}s jaccard={jac:.3f} preds={len(preds)} "
                  f"wc={run['weighted_cost']} tc={run['tool_calls']} [{status}]")
            if run.get("stderr") == "TIMEOUT":
                print("    TIMEOUT — stop")
                stop = True
                break
            if "rate limit" in run.get("stderr", "").lower():
                print("    RATE LIMIT — stop")
                stop = True
                break
        if stop:
            break
    out_f.close()
    print(f"\n=== Done. Output: {out_path} ===")


if __name__ == "__main__":
    main()