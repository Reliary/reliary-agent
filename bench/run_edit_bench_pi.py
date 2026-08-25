"""V53: Edit-task benchmark using Pi Agent as the edit engine.

For each task: copy corpus to fresh workdir → invoke Pi non-interactively with
the task as a prompt → Pi uses its tool surface (reliary/altbackend/bash) to
make edits → capture git diff and run cargo check → score.

Conditions:
- A (reliary): Pi loads reliary_mcp_pi_extension.js via --extension flag
- B (altbackend): Pi loads /tmp/altbackend_reliary_pi_extension.js
- C (grep): Pi uses --no-extensions, model uses only bash+grep+read+edit
"""
import os
import sys
import subprocess
import json
import shutil
import time

# Config
CORPUS_SRC = "$HOME/src/reliary8"
WORKDIR_BASE = "/tmp/edit_bench_workdir"
PI_BIN = "$HOME/.local/bin/pi"
RELIARY_EXT = "$HOME/src/reliary8/bench/reliary_mcp_pi_extension.js"
ALTBACKEND_EXT = "/tmp/altbackend_reliary_pi_extension.js"
DEEPSEEK_MODEL = "deepseek/deepseek-v4-flash"

MAX_TURNS_TIMEOUT = 600  # 10 min hard cap per task

# 3 edit tasks on reliary8 source
EDIT_TASKS = [
    {
        "id": "t1_add_doc_comment",
        "question": (
            "Add a doc comment `/// Classifies a line of source code into structural categories.` "
            "to the `classify_structural` function in `crates/reliary-search/src/structural.rs`. "
            "Use the edit tool to insert the doc comment immediately before the `pub fn classify_structural` "
            "line. Do not change anything else. When done, commit your changes with `git add -A && git commit`."
        ),
        "expected_markers": ["/// Classifies a line of source code into structural categories."],
        "target_file": "crates/reliary-search/src/structural.rs",
    },
    {
        "id": "t2_add_import",
        "question": (
            "Find the file in `crates/reliary-search/src/` that defines a function or type "
            "using the word `HashMap` but does NOT currently `use std::collections::HashMap` or "
            "`use rustc_hash::FxHashMap`. Add the missing `use` statement at the top of that file. "
            "Use the read tool to inspect files. Use the edit tool to add the import. "
            "After the edit, run `cargo check -p reliary-search` to verify it compiles. "
            "When done, commit your changes with `git add -A && git commit`."
        ),
        "expected_markers": ["use rustc_hash::FxHashMap"],
        "target_file": None,  # any file in crates/reliary-search/src/
        "min_files_changed": 1,
    },
    {
        "id": "t3_rename_in_function",
        "question": (
            "In `crates/reliary-search/src/lazy_occurrence.rs`, find every occurrence of the local "
            "variable `phrase_cache` inside the function `load_phrase_cache` AND the function "
            "`ensure_occurrence_for_file_impl`. Replace all of them with `pc`. "
            "DO NOT rename the function signature parameter in `ensure_occurrence_for_file_impl` "
            "(the parameter NAME in the signature must stay `phrase_cache`). "
            "Use sed via bash or multiple edit calls. After the rename, verify with `cargo check -p reliary-search`. "
            "When done, commit your changes with `git add -A && git commit`."
        ),
        "expected_markers": [],
        "min_replacements": {"from": "phrase_cache", "to": "pc", "min": 5},
        "target_file": "crates/reliary-search/src/lazy_occurrence.rs",
    },
    # === HARDER TASKS (t4-t6) — exercise large bash output ===
    {
        "id": "t4_multi_file_rename",
        "question": (
            "The function `scan_identifiers` is defined in `crates/reliary-search/src/lib.rs:106` "
            "and called from multiple files (ft_weight.rs, compat.rs, ingest.rs). "
            "Rename it to `extract_identifiers` everywhere. "
            "Use `grep -rn 'scan_identifiers' crates/` to find all occurrences. "
            "Update both the definition and all call sites. "
            "After the rename, run `cargo check -p reliary-search` to verify it compiles. "
            "When done, commit your changes with `git add -A && git commit`."
        ),
        "expected_markers": [],
        "min_replacements": {"from": "scan_identifiers", "to": "extract_identifiers", "min": 4},
        "target_file": "crates/reliary-search/src/lib.rs",
    },
    {
        "id": "t5_remove_debug_eprintln",
        "question": (
            "Find all `eprintln!` calls in `crates/reliary-search/src/` that are debug/profiling "
            "output (containing strings like '[debug]', '[profile]', '[ingest]', '[callgraph_v2]', '[symbol]', "
            "'[structural]', or starting with `H5_string:`). "
            "Remove these debug eprintln! calls entirely. "
            "First run `grep -rn 'eprintln!' crates/reliary-search/src/` to find them, "
            "then use the edit tool or sed to delete each debug eprintln! line. "
            "After the edits, run `cargo check -p reliary-search` to verify it still compiles. "
            "When done, commit your changes with `git add -A && git commit`."
        ),
        "expected_markers": [],
        "min_files_changed": 3,
        "target_file": None,  # any file in crates/reliary-search/src/
    },
    {
        "id": "t6_fix_unused_imports",
        "question": (
            "Find all `use` statements in `crates/reliary-search/src/` that import items "
            "which are no longer used in the file. "
            "First run `cargo check -p reliary-search` to see the warnings, "
            "then run `cargo build -p reliary-search 2>&1 | grep 'unused import'` to find the specific "
            "unused imports. Remove each unused `use` line using the edit tool. "
            "After the edits, run `cargo check -p reliary-search` to verify the warnings are gone. "
            "When done, commit your changes with `git add -A && git commit`."
        ),
        "expected_markers": [],
        "min_files_changed": 1,
        "target_file": None,
    },
]


def setup_workdir(corpus_src, dest):
    """Copy corpus to fresh workdir with git init."""
    if os.path.exists(dest):
        shutil.rmtree(dest)
    shutil.copytree(corpus_src, dest,
                    ignore=shutil.ignore_patterns(
                        "target", "node_modules", "dist", "build", ".git",
                        "bench/results", ".reliary"))
    # Initialize git so we can capture diffs
    subprocess.run(["git", "init", "-q"], cwd=dest, check=True)
    subprocess.run(["git", "config", "user.email", "bench@reliary"],
                   cwd=dest, check=True)
    subprocess.run(["git", "config", "user.name", "bench"],
                   cwd=dest, check=True)
    subprocess.run(["git", "add", "-A"], cwd=dest, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "initial"],
                   cwd=dest, check=True)


def get_diff(workdir):
    """Capture git diff vs HEAD~ (last commit) or staged changes."""
    try:
        r = subprocess.run(
            ["git", "diff", "HEAD~1", "HEAD", "--no-color"],
            capture_output=True, text=True, timeout=10,
            cwd=workdir,
        )
        return r.stdout
    except Exception:
        return ""


def cargo_check(workdir):
    """Run cargo check, return True if compiles."""
    try:
        r = subprocess.run(
            ["cargo", "check", "-p", "reliary-search", "--offline"],
            capture_output=True, text=True, cwd=workdir, timeout=180,
        )
        if r.returncode == 0:
            return True
        # Fallback without --offline
        r = subprocess.run(
            ["cargo", "check", "-p", "reliary-search"],
            capture_output=True, text=True, cwd=workdir, timeout=180,
        )
        return r.returncode == 0
    except Exception:
        return False


def score_diff(diff, task):
    """Score 0-3: file changed, markers found, compile check (returned separately)."""
    score = 0
    if not diff or len(diff) < 10:
        return 0, False

    # File changed check
    if task.get("target_file"):
        if task["target_file"] in diff:
            score += 1
    elif task.get("min_files_changed"):
        # Any file in the crate
        changed_files = [line[6:] for line in diff.split("\n")
                         if line.startswith("+++ b/")]
        score += 1 if changed_files else 0

    if task.get("expected_markers"):
        all_present = all(m in diff for m in task["expected_markers"])
        if all_present:
            score += 1
    elif task.get("min_replacements"):
        spec = task["min_replacements"]
        from_count = sum(1 for line in diff.split("\n")
                         if line.startswith("-") and spec["from"] in line)
        to_count = sum(1 for line in diff.split("\n")
                       if line.startswith("+") and spec["to"] in line)
        if from_count >= spec["min"] and to_count >= spec["min"]:
            score += 1

    return score, False


def build_pi_args(cond):
    """Build per-condition Pi CLI args."""
    base = [
        PI_BIN, "-p",
        "--model", DEEPSEEK_MODEL,
        "--mode", "json",
        "--no-session",
        "--approve",
    ]
    if cond == "A":
        # Reliary extension (provides reliary tools)
        base += ["--extension", RELIARY_EXT]
    elif cond == "B":
        # Altbackend extension (provides altbackend tools)
        base += ["--extension", ALTBACKEND_EXT]
    elif cond == "C":
        # No code-intel extensions, model uses bash+grep+read+edit
        base += ["--no-extensions"]
    return base


def run_edit_task(task, cond, seed, sift=False):
    """Run a single edit task via Pi Agent."""
    workdir = f"{WORKDIR_BASE}_{cond}_{seed}_{task['id']}"
    setup_workdir(CORPUS_SRC, workdir)

    pi_args = build_pi_args(cond) + [task["question"]]

    # Pass RELIARY_SIFT_BASH env var to Pi subprocess (gate.js reads it).
    # When sift=True: RELIARY_SIFT_BASH=1 (gate.js intercepts bash and wraps)
    # When sift=False: RELIARY_SIFT_BASH=0 (gate.js passes bash through unchanged)
    env = os.environ.copy()
    env["RELIARY_SIFT_BASH"] = "1" if sift else "0"

    t_start = time.time()
    try:
        result = subprocess.run(
            pi_args,
            capture_output=True, text=True, timeout=MAX_TURNS_TIMEOUT,
            cwd=workdir,
            env=env,
        )
        stdout = result.stdout
        stderr = result.stderr
        returncode = result.returncode
    except subprocess.TimeoutExpired:
        return {
            "id": task["id"], "cond": cond, "seed": seed,
            "score": 0, "compiled": False, "turns": 0,
            "target_changed": False, "markers_found": False,
            "diff_lines": 0, "wall": MAX_TURNS_TIMEOUT,
            "error": "timeout",
        }
    except Exception as e:
        return {
            "id": task["id"], "cond": cond, "seed": seed,
            "score": 0, "compiled": False, "turns": 0,
            "target_changed": False, "markers_found": False,
            "diff_lines": 0, "wall": time.time() - t_start,
            "error": str(e)[:200],
        }

    # Capture diff
    diff = get_diff(workdir)
    raw_score, _ = score_diff(diff, task)
    compiled = cargo_check(workdir) if diff else False
    score = raw_score + (1 if compiled and raw_score >= 2 else 0)
    score = min(score, 3)

# Compute target_changed
    if task.get("target_file"):
        target_changed = task["target_file"] in diff
    else:
        target_changed = len(diff) > 10  # any change

    return {
        "id": task["id"], "cond": cond, "seed": seed,
        "score": score, "compiled": compiled,
        "target_changed": target_changed,
        "markers_found": raw_score >= 2,
        "diff_lines": len(diff.split("\n")),
        "wall": time.time() - t_start,
        "returncode": returncode,
    }


def main():
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument("--conds", default="A,B,C", help="Comma-separated conditions")
    parser.add_argument("--seeds", default="42,17", help="Comma-separated seeds")
    parser.add_argument("--sift", choices=["on", "off"], default="off",
                        help="Toggle RELIARY_SIFT_BASH for bash compression (RTK parity)")
    parser.add_argument("--tasks", choices=["easy", "hard", "all"], default="easy",
                        help="easy=t1-t3, hard=t4-t6, all=t1-t6")
    args = parser.parse_args()

    conds = [c.strip() for c in args.conds.split(",")]
    seeds = [int(s.strip()) for s in args.seeds.split(",")]
    all_results = []

    # Filter tasks by difficulty
    if args.tasks == "easy":
        tasks_to_run = [t for t in EDIT_TASKS if t["id"] in ("t1_add_doc_comment", "t2_add_import", "t3_rename_in_function")]
    elif args.tasks == "hard":
        tasks_to_run = [t for t in EDIT_TASKS if t["id"] in ("t4_multi_file_rename", "t5_remove_debug_eprintln", "t6_fix_unused_imports")]
    else:
        tasks_to_run = EDIT_TASKS

    sift_label = "sift-ON" if args.sift == "on" else "sift-OFF"
    print(f"=== Edit Bench ({sift_label}, {args.tasks}) ===\n", flush=True)

    for cond in conds:
        for seed in seeds:
            for task in tasks_to_run:
                print(f"[{cond} seed={seed} {sift_label}] {task['id']}...", flush=True)
                r = run_edit_task(task, cond, seed, sift=args.sift == "on")
                all_results.append(r)
                print(f"  score={r['score']}/3 compiled={r['compiled']} "
                      f"changed={r['target_changed']} wall={r['wall']:.0f}s "
                      f"err={r.get('error', '')}", flush=True)

    # Aggregate
    print(f"\n=== Edit Bench Results ({sift_label}) ===")
    cond_names = {"A": "reliary", "B": "altbackend", "C": "grep"}
    for cond in conds:
        cond_rs = [r for r in all_results if r["cond"] == cond]
        if not cond_rs:
            continue
        avg = sum(r["score"] for r in cond_rs) / len(cond_rs)
        compile_count = sum(1 for r in cond_rs if r["compiled"])
        avg_wall = sum(r["wall"] for r in cond_rs) / len(cond_rs)
        print(f"{cond} ({cond_names[cond]}): avg {avg:.2f}/3 "
              f"compile={compile_count}/{len(cond_rs)} "
              f"wall_avg={avg_wall:.0f}s")

    out_path = f"$HOME/src/reliary8/bench/results/edit_bench_pi_{sift_label}_seed{seeds}.json"
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "w") as f:
        json.dump(all_results, f, indent=2)
    print(f"\nResults saved to {out_path}")


if __name__ == "__main__":
    main()