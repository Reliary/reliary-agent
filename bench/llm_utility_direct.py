"""Arc 28 Lever 6 — Direct DeepSeek with FUNCTION CALLING.

Paired benchmark: A (grep output → LLM → JSON) vs B (reliary output → LLM → JSON).

Per workspace WORKFLOW_RULES:
- Interleaved A/B per task (NOT sequential) to control 2.7× LLM variance.
- Per project memory: weighted cost = prompt + 4 × completion tokens.

Approach (mirror stria's MCP-style LLM loop):
1. Pre-fetch tool output server-side (grep OR reliary).
2. Send LLM the tool output as part of the prompt.
3. LLM extracts references from output → emits JSON.
4. We measure: LLM's reported references vs ground truth.

Why this is honest: the LLM gets the SAME data either way; we're testing
whether the LLM can correctly process grep output vs reliary output to
find symbol references.

Constraint (per user):
- Direct DeepSeek via api.deepseek.com (NOT api.reliary.dev, NOT deepinfra).
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
from llm_conn import (deepseek_chat, mcp_call, TOKIO_CORPUS, HYPER_CORPUS,
                       HOMONYMS_FIXTURE, RELIARY_BIN)

RESULTS_DIR = "/home/user/src/reliary8/bench/results"


def load_anchors(corpus, max_gt_size=300):
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
    rel_path = anchor["anchor_file"]
    if rel_path.startswith(workdir):
        rel_path = rel_path[len(workdir):].lstrip("/")
    r = mcp_call("reliary_find_references_type_flow",
                  {"name": anchor["stem"], "anchor_file": rel_path,
                   "anchor_line": anchor["anchor_line"], "path": workdir,
                   "threshold": threshold},
                  workdir=workdir, timeout=60)
    return [f"{h['file']}:{h['line']}" for h in r.get("hits", [])]


# ───── Tool output fetchers (server-side) ─────

def run_grep(stem, workdir, limit=100):
    """Run grep server-side, return up to `limit` matches as list of file:line."""
    try:
        r = subprocess.run(
            ["grep", "-rn", f"\\b{stem}\\b", workdir, "--include=*.rs"],
            capture_output=True, text=True, timeout=30,
        )
        lines = r.stdout.split("\n")
        # Format: path:line:content → (path, line)
        results = []
        for line in lines[:limit * 2]:
            m = re.match(r"^([^:]+):(\d+):", line)
            if m:
                fp = m.group(1)
                if fp.startswith(workdir):
                    fp = fp[len(workdir):].lstrip("/")
                results.append((fp, int(m.group(2))))
            if len(results) >= limit:
                break
        return results
    except Exception as e:
        return [("ERROR", str(e))]


def run_reliary(stem, anchor_file_rel, anchor_line, workdir, threshold=0.1, limit=50):
    """Run reliary_find_references_type_flow server-side, return top-N hits."""
    r = mcp_call("reliary_find_references_type_flow",
                  {"name": stem, "anchor_file": anchor_file_rel,
                   "anchor_line": anchor_line, "path": workdir,
                   "threshold": threshold},
                  workdir=workdir, timeout=60)
    hits = r.get("hits", [])[:limit]
    results = []
    for h in hits:
        fp = h.get("file", "")
        if fp.startswith(workdir):
            fp = fp[len(workdir):].lstrip("/")
        results.append((fp, h.get("line", 0), h.get("similarity", 0.0),
                         h.get("is_def", False)))
    return results


# ───── LLM extraction ─────

GREP_SYSTEM = """You are a precise code analyst. Your task: from a list of grep matches,
identify which ones are references to a symbol of the same class as the anchor.

Output JSON only:
{"references": [{"file": "<path>", "line": <int>}, ...]}

Rules:
- Same class means: same role (function_def vs method_call vs type_name) and same intent.
- Drop matches in test files (paths containing /tests/ or _test.rs).
- Drop matches in doc comments (lines starting with /// or //!).
- Drop impl declarations (lines starting with "impl").
- Only include matches whose stem is used (defined OR called), not just mentioned.
"""


RELIARY_SYSTEM = """You are a precise code analyst. Your task: from a list of reliary type-flow hits
(ranked by similarity score), identify which ones are references to the same class as the anchor.

Output JSON only:
{"references": [{"file": "<path>", "line": <int>}, ...]}

Rules:
- Top-similarity hits (>0.5) are usually correct.
- Drop hits in test files (paths containing /tests/ or _test.rs).
- Drop hits in doc comments (lines starting with /// or //!).
- Drop impl declarations (lines starting with "impl").
- is_def=1 hits are usually the definition site itself; include if anchor isn't that line.
"""


def extract_from_grep(anchor, workdir, model="deepseek-v4-flash"):
    print(f"    grep: fetching {anchor['stem']}...", flush=True)
    matches = run_grep(anchor["stem"], workdir, limit=80)
    print(f"    grep: got {len(matches)} matches", flush=True)
    rel_anchor = anchor["anchor_file"]
    if rel_anchor.startswith(workdir):
        rel_anchor = rel_anchor[len(workdir):].lstrip("/")
    user = (
        f"Anchor: {anchor['anchor_file']}:{anchor['anchor_line']} "
        f"(label={anchor['use_label']}, stem=`{anchor['stem']}`)\n\n"
        f"Grep matches ({len(matches)} total, showing file:line):\n"
        + "\n".join(f"  {fp}:{line}" for fp, line in matches[:80])
    )
    print(f"    grep: calling LLM...", flush=True)
    return _call_llm(GREP_SYSTEM, user, anchor, model, matches_count=len(matches))


def extract_from_reliary(anchor, workdir, model="deepseek-v4-flash"):
    rel_anchor = anchor["anchor_file"]
    if rel_anchor.startswith(workdir):
        rel_anchor = rel_anchor[len(workdir):].lstrip("/")
    print(f"    reliary: fetching {anchor['stem']}...", flush=True)
    hits = run_reliary(anchor["stem"], rel_anchor, anchor["anchor_line"],
                        workdir, threshold=0.1, limit=50)
    print(f"    reliary: got {len(hits)} hits", flush=True)
    user = (
        f"Anchor: {anchor['anchor_file']}:{anchor['anchor_line']} "
        f"(label={anchor['use_label']}, stem=`{anchor['stem']}`)\n\n"
        f"Reliary type-flow hits (similarity desc, is_def):\n"
        + "\n".join(f"  {fp}:{line}  sim={sim:.3f}  is_def={is_d}"
                     for fp, line, sim, is_d in hits)
    )
    print(f"    reliary: calling LLM...", flush=True)
    return _call_llm(RELIARY_SYSTEM, user, anchor, model, matches_count=len(hits))


def _call_llm(system, user, anchor, model, matches_count=0):
    messages = [
        {"role": "system", "content": system},
        {"role": "user", "content": user},
    ]
    t0 = time.time()
    resp = deepseek_chat(messages, model=model, max_tokens=600, timeout=30)
    elapsed = time.time() - t0
    if "error" in resp:
        return {"elapsed": elapsed, "response": resp, "parsed": None,
                "tokens_in": 0, "tokens_out": 0, "weighted_cost": 0,
                "predictions": [], "matches_provided": matches_count}
    msg = resp.get("choices", [{}])[0].get("message", {})
    content = msg.get("content", "")
    # Save full response for debugging.
    import os
    debug_path = f"/tmp/llm_debug_{int(time.time()*1000)}.json"
    try:
        with open(debug_path, "w") as f:
            json.dump(resp, f, indent=2)
    except Exception:
        pass
    if not content and "reasoning_content" in msg:
        content = msg.get("reasoning_content", "")
    usage = resp.get("usage", {})
    pt = usage.get("prompt_tokens", 0)
    ct = usage.get("completion_tokens", 0)
    parsed = parse_llm_json(content)
    preds = extract_references(parsed)
    return {"elapsed": elapsed, "raw_content": content[:500],
            "parsed": parsed,
            "tokens_in": pt, "tokens_out": ct,
            "weighted_cost": pt + 4 * ct,
            "predictions": preds, "matches_provided": matches_count,
            "model": resp.get("model", model),
            "_debug_response_path": debug_path}


def parse_llm_json(content):
    content = (content or "").strip()
    if not content:
        return None
    try:
        return json.loads(content)
    except Exception:
        pass
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


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--n", type=int, default=30)
    parser.add_argument("--corpus", choices=["tokio", "hyper"], default="tokio")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--model", default="deepseek-v4-flash")
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    if args.out is None:
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        out_path = os.path.join(RESULTS_DIR, f"llm_utility_funcall_{ts}.jsonl")
    else:
        out_path = args.out
    os.makedirs(os.path.dirname(out_path), exist_ok=True)

    rng = random.Random(args.seed)
    anchors, workdir = load_anchors(args.corpus)
    sample = rng.sample(anchors, min(args.n, len(anchors)))

    print(f"=== Arc 28 Lever 6 — Direct LLM (Function-Calling Style) ===")
    print(f"Corpus: {args.corpus} ({len(anchors)} benchable with GT≤300)")
    print(f"Workdir: {workdir}")
    print(f"Tasks: {len(sample)} paired A/B interleaved")
    print(f"Model: {args.model}")
    print(f"Output: {out_path}\n")

    out_f = open(out_path, "w")
    a_wins = b_wins = 0
    stop = False
    for i, anchor in enumerate(sample):
        if i % 2 == 0:
            order = ["A", "B"]
        else:
            order = ["B", "A"]
        gt = ground_truth_for_anchor(anchor, workdir, threshold=0.1)
        for cond in order:
            print(f"  task {i+1}/{len(sample)} anchor={anchor['id']} cond={cond} "
                  f"(gt={len(gt)}) ... ", end="", flush=True)
            if cond == "A":
                run = extract_from_grep(anchor, workdir, args.model)
            else:
                run = extract_from_reliary(anchor, workdir, args.model)
            preds = run["predictions"]
            jac = jaccard(preds, gt)
            run.update({"anchor_id": anchor["id"], "stem": anchor["stem"],
                        "use_label": anchor["use_label"], "condition": cond,
                        "ground_truth_size": len(gt), "jaccard": jac})
            out_f.write(json.dumps(run) + "\n")
            out_f.flush()
            ok = "OK" if "error" not in run.get("response", {}) else "ERR"
            print(f"t={run['elapsed']:.1f}s jaccard={jac:.3f} preds={len(preds)} "
                  f"wc={run['weighted_cost']} pt={run['tokens_in']} "
                  f"ct={run['tokens_out']} provided={run['matches_provided']} [{ok}]")
            if "error" in run.get("response", {}):
                err = str(run["response"]["error"])[:100]
                print(f"    ERROR: {err}")
                if "rate limit" in err.lower():
                    stop = True
                    break
        if stop:
            break
    out_f.close()
    print(f"\n=== Done. Output: {out_path} ===")


if __name__ == "__main__":
    main()