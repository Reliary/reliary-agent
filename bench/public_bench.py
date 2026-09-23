#!/usr/bin/env python3
"""Public, repo-agnostic benchmark.

Generates comprehension questions + ground truth from ANY reliary index
(auto_questions.py), runs the 3-way comparison (reliary/altbackend/grep) on
that corpus, then scores with the deterministic claim verifier.

Usage:
  python3 bench/public_bench.py --index /path/to/.reliary/index.sqlite \
      --corpus /path/to/repo --bin /path/to/reliary \
      --seeds 42 17 --out results/public.jsonl

Steps:
  1. auto_questions.py -> questions.json (repo-derived, no hardcoded symbols)
  2. convert to long_session_bench.SESSION_QUERIES format (rubric = accept = GT syms)
  3. run_snapshot_bench.py --conds A,B,C (or A only with --conds A)
  4. deterministic_verify.py on the results (needs the GT facts — we regenerate
     from auto_questions so the verifier has the same facts to check)
"""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def main():
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--index", required=True, help="path to .reliary/index.sqlite")
    ap.add_argument("--corpus", required=True, help="repo root (cwd for the agent)")
    ap.add_argument("--bin", required=True, help="reliary binary")
    ap.add_argument("--seeds", nargs="+", type=int, default=[42, 17])
    ap.add_argument("--conds", default="A,B,C")
    ap.add_argument("--out", default="results/public.jsonl")
    ap.add_argument("--altbackend-project", default=None)
    ap.add_argument("--seed", type=int, default=42, help="question-gen seed")
    ap.add_argument("--questions", default=None,
                    help="use this auto_questions-format JSON instead of generating "
                         "(for corpus pairs that must receive identical tasks)")
    ap.add_argument("--gt", default=None,
                    help="GT file for the verifier (default: the generated one)")
    ap.add_argument("--no-bench", action="store_true", help="only generate questions + verify existing out")
    args = ap.parse_args()

    # 1. generate questions (or take a pre-built, translated set)
    q_json = os.path.join(HERE, "results", "public_questions.json")
    os.makedirs(os.path.dirname(q_json), exist_ok=True)
    if args.questions:
        with open(args.questions) as fh:
            qdata = json.load(fh)
        with open(q_json, "w") as fh:
            json.dump(qdata, fh, indent=2)
    else:
        subprocess.run(
            [sys.executable, os.path.join(HERE, "auto_questions.py"), args.index, str(args.seed)],
            check=True, stdout=open(q_json, "w"),
        )
        with open(q_json) as fh:
            qdata = json.load(fh)
    questions = qdata["questions"]
    print(f"[public_bench] {len(questions)} questions generated from {qdata['repo']} "
          f"({qdata.get('n_files', '?')} files)")

    if not questions:
        print("[public_bench] no questions generated — corpus too sparse or unsupported")
        return 1

    # 2. write harness-compatible queries + GT for the verifier
    harness_q = []
    for q in questions:
        syms = sorted({g["sym"] for g in q["gt"]})
        harness_q.append({
            "id": q["query_id"],
            "question": q["question"],
            "rubric": {"accept": syms, "accept_keywords": syms, "min_count": 1},
        })
    # patch long_session_bench's SESSION_QUERIES
    sys.path.insert(0, HERE)
    import importlib
    lsb = importlib.import_module("long_session_bench")
    lsb.SESSION_QUERIES = harness_q

    # Pre-register a fake `reliary_bench` module so run_snapshot_bench's
    # `from reliary_bench import SESSION_QUERIES` gets OUR questions instead of
    # the hardcoded reliary corpus ones.
    import types
    fake = types.ModuleType("reliary_bench")
    fake.SESSION_QUERIES = harness_q
    sys.modules["reliary_bench"] = fake

    # write GT facts for the verifier
    gt_path = os.path.join(HERE, "results", "public_gt.json")
    if args.gt and os.path.abspath(args.gt) != os.path.abspath(gt_path):
        with open(args.gt) as fh:
            gt_data = json.load(fh)
    else:
        gt_data = {"repo": qdata["repo"], "questions": questions}
    with open(gt_path, "w") as fh:
        json.dump(gt_data, fh, indent=2)
    print(f"[public_bench] GT facts -> {gt_path}")

    if args.no_bench:
        return 0

    # 3. run the 3-way
    harness_q_path = os.path.join(HERE, "results", "public_harness_questions.json")
    with open(harness_q_path, "w") as fh:
        json.dump(harness_q, fh, indent=2)
    env = dict(os.environ)
    env["RELIARY_QUESTIONS"] = harness_q_path
    cmd = [
        sys.executable, os.path.join(HERE, "run_snapshot_bench.py"),
        "--bin", args.bin, "--corpus", args.corpus,
        "--conds", args.conds, "--seeds"] + [str(s) for s in args.seeds] + \
        ["--out", args.out]
    if args.altbackend_project:
        cmd += ["--altbackend-project", args.altbackend_project]
    print("[public_bench] running:", " ".join(cmd[:8]), "...")
    subprocess.run(cmd, check=True, env=env)

    # 4. verify deterministically (use auto GT — deterministic_verify reads
    #    bench/reliary_judge_gt.py by default; we pass the GT via --gt)
    print("[public_bench] run deterministic_verify.py on", args.out)
    # `--out` is relative to the repo root (parent of bench/), matching how
    # run_snapshot_bench receives it. Resolve it the same way; joining HERE
    # again produced bench/bench/results/... and broke the verify step.
    out_path = args.out if os.path.isabs(args.out) else os.path.join(
        os.path.dirname(HERE), args.out)
    subprocess.run(
        [sys.executable, os.path.join(HERE, "deterministic_verify.py"),
         "--input", out_path, "--corpus", args.corpus,
         "--gt", gt_path],
        check=True, cwd=HERE,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())