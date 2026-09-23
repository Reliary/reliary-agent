#!/usr/bin/env python3
"""Format-neutral LLM judge for the familiarity experiment.

Scores every answer with deepseek-v4-pro against mechanically-derived ground
truth, so the comparison does not reward either backend's citation style.
The claim-verified F1 rewards reliai's `symbol at file:line` shape; this does
not.

Usage:
  python3 bench/judge_familiarity.py --orig bench/results/fam3_orig.jsonl \
      --orig-gt /tmp/orig_q_fixed.json \
      --obf bench/results/fam4_obf.jsonl --obf-gt /tmp/obf_q_fixed2.json
"""
import argparse
import json
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from llm_conn import deepseek_chat, DEEPSEEK_MODEL_PRO  # noqa: E402

PROMPT = """You are a code intelligence evaluator. Score the answer for correctness against the ground truth.

Question: {question}

Ground truth (symbol, file, line facts that a correct answer should contain):
{gt}

Model's answer:
{answer}

Scoring:
- 3/3: correctly identifies the right symbols/files/relationships; minor formatting differences OK.
- 2/3: mostly correct, misses 1-2 facts or has one wrong file.
- 1/3: some relevant facts but significant errors or misses most.
- 0/3: wrong, empty, or hallucinated.

Respond with ONLY {{"score": <0-3>, "reason": "<one sentence>"}}"""


def gt_prose(q):
    return "; ".join(
        f"{g['sym']} at {os.path.basename(g['file'])}:{g['line']}"
        for g in q.get("gt", []))


def load_gt(path):
    with open(path) as fh:
        return {q["query_id"]: q for q in json.load(fh)["questions"]}


def judge(question, gt, answer):
    prompt = PROMPT.format(question=question, gt=gt,
                           answer=(answer or "(empty)")[:2000])
    try:
        resp = deepseek_chat([{"role": "user", "content": prompt}],
                             model=DEEPSEEK_MODEL_PRO, max_tokens=200)
        text = resp["choices"][0]["message"]["content"] if isinstance(resp, dict) and "choices" in resp else str(resp)
        m = re.search(r"\{[^}]+\}", text)
        if m:
            return int(json.loads(m.group()).get("score", 0))
        m = re.search(r"(\d)", text)
        return int(m.group(1)) if m else 0
    except Exception as e:
        print("  judge error:", e, file=sys.stderr)
        return 0


def score_arm(path, gtpath, label):
    gt = load_gt(gtpath)
    rows = [json.loads(l) for l in open(path)]
    totals = {}
    for r in rows:
        if "error" in r:
            continue
        cond = r["cond"]
        for q in r.get("queries", []):
            qid = q["query_id"]
            if qid not in gt:
                continue
            s = judge(gt[qid].get("question", qid), gt_prose(gt[qid]),
                      q.get("answer", ""))
            totals.setdefault(cond, []).append(s)
            time.sleep(0.4)
    print(f"\n=== {label} (judge, out of 3 per query)")
    for c in sorted(totals):
        v = totals[c]
        print(f"  {c}: mean={sum(v)/len(v):.2f}  n={len(v)}  total={sum(v)}/{len(v)*3}")
    return totals


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--orig", required=True)
    ap.add_argument("--orig-gt", required=True)
    ap.add_argument("--obf", required=True)
    ap.add_argument("--obf-gt", required=True)
    args = ap.parse_args()
    score_arm(args.orig, args.orig_gt, "ORIGINAL tokio")
    score_arm(args.obf, args.obf_gt, "OBFUSCATED tokio")


if __name__ == "__main__":
    main()
