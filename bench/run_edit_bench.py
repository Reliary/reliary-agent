"""V53: Edit-task benchmark — tests whether models can MAKE code changes.

Each task: clean workdir + task description → LLM uses tools (reliary or bash/edit) →
verify diff against expected markers → cargo check to verify compilation.
"""
import os
import sys
import subprocess
import json
import shutil
import time

sys.path.insert(0, os.path.dirname(__file__))

import llm_conn
import multi_turn_harness as mth

CORPUS_SRC = "$HOME/src/reliary8"
WORKDIR = "/tmp/edit_bench_workdir"
MAX_TURNS = 15

DEEPSEEK_MODEL = os.environ.get("DEEPSEEK_MODEL", "deepseek-chat")

# 3 edit tasks on reliary8 source. Each has a question and expected diff markers.
EDIT_TASKS = [
    {
        "id": "t1_add_doc_comment",
        "question": (
            "Add a doc comment `/// Classifies a line of source code into structural categories.` "
            "to the `classify_structural` function in `crates/reliary-search/src/structural.rs`. "
            "The function starts at line 31. Use the edit tool to insert the doc comment immediately "
            "before the `pub fn classify_structural` line. Do not change anything else."
        ),
        "expected_markers": ["/// Classifies a line of source code into structural categories."],
        "target_file": "crates/reliary-search/src/structural.rs",
    },
    {
        "id": "t2_fix_off_by_one",
        "question": (
            "In `crates/reliary-search/src/symbol.rs`, function `block_id_at` at line 36 has an off-by-one bug. "
            "The query `start_line <= ?2` uses the `line` parameter directly, but the block table uses "
            "0-indexed line numbers. Fix the bug by subtracting 1 before the SQL query. "
            "Use the edit tool. Verify the file compiles with `cargo check -p reliary-search`."
        ),
        "expected_markers": ["saturating_sub(1)"],
        "target_file": "crates/reliary-search/src/symbol.rs",
    },
    {
        "id": "t3_rename_var",
        "question": (
            "In `crates/reliary-search/src/symbol.rs`, rename the local variable `phrase_cache` to `pc` "
            "everywhere it appears INSIDE the function `ensure_occurrence_for_file_impl`. "
            "DO NOT rename the parameter name in the function signature, DO NOT rename in other functions. "
            "Use multiple edit calls or sed via bash. Verify with `cargo check -p reliary-search`."
        ),
        "expected_markers": [],
        "min_replacements": {"from": "phrase_cache", "to": "pc", "min": 5},
        "target_file": "crates/reliary-search/src/symbol.rs",
    },
]

CONDITION_NAMES = {"A": "reliary", "B": "altbackend", "C": "grep"}


def setup_workdir(src, dest):
    """Copy corpus to fresh workdir."""
    if os.path.exists(dest):
        shutil.rmtree(dest)
    shutil.copytree(src, dest, ignore=shutil.ignore_patterns(
        "target", "node_modules", "dist", "build", ".git", "bench/results"
    ))


def get_diff(workdir):
    """Capture git diff in workdir."""
    try:
        r = subprocess.run(
            ["git", "-C", workdir, "diff", "--no-color"],
            capture_output=True, text=True, timeout=10
        )
        return r.stdout
    except Exception:
        return ""


def cargo_check(workdir):
    """Run cargo check, return True if compiles."""
    try:
        r = subprocess.run(
            ["cargo", "check", "-p", "reliary-search", "--offline"],
            capture_output=True, text=True, cwd=workdir, timeout=180
        )
        if r.returncode == 0:
            return True
        # Try without --offline if offline cache misses
        r = subprocess.run(
            ["cargo", "check", "-p", "reliary-search"],
            capture_output=True, text=True, cwd=workdir, timeout=180
        )
        return r.returncode == 0
    except Exception:
        return False


def score_diff(diff, task):
    """Score 0-3: file changed, markers present, compile check."""
    score = 0
    if not diff or len(diff) < 10:
        return 0

    # Score 1: diff has changes in the target file
    if task["target_file"] in diff:
        score += 1

    # Score 2: all expected markers present
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

    return score


def run_edit_session(task, cond, seed):
    """Run a single edit task in a fresh workdir."""
    setup_workdir(CORPUS_SRC, WORKDIR)

    # System prompt + question
    sys_prompt = mth.SYSTEM_PROMPTS[cond] if cond in mth.SYSTEM_PROMPTS else ""
    messages = [
        {"role": "system", "content": sys_prompt + (
            "\n\nYou are editing a real codebase. The working directory is "
            f"{WORKDIR}. Use bash, read, and edit tools to make changes. "
            "When done, respond with `{\"final\": true, \"answer\": \"DONE\"}`."
        )},
        {"role": "user", "content": task["question"]},
    ]

    mth._ensure_sessions(cond)
    t_start = time.time()
    turns = 0
    tool_calls = 0
    for turn in range(MAX_TURNS):
        if time.time() - t_start > 600:  # 10 min hard cap
            break
        resp = llm_conn.deepseek_chat(messages, model=DEEPSEEK_MODEL,
                                       max_tokens=2000, timeout=120,
                                       disable_thinking=True)
        if "error" in resp:
            break
        msg = resp.get("choices", [{}])[0].get("message", {})
        content = (msg.get("content", "") or msg.get("reasoning_content", "")).strip()
        if not content:
            break
        turns += 1

        action_type, action = mth.parse_llm_response(content)

        if action_type == "final":
            messages.append({"role": "assistant", "content": content})
            break
        elif action_type == "tool":
            tool_name = action.get("tool", action.get("name", ""))
            tool_args = action.get("args", action.get("arguments", action))
            if isinstance(tool_args, str):
                try:
                    tool_args = json.loads(tool_args) if tool_args.strip().startswith("{") else {"query": tool_args}
                except Exception:
                    tool_args = {"query": tool_args}

            result = mth.execute_tool(cond, tool_name, tool_args)
            if isinstance(result, tuple):
                output = result[0]
            else:
                output = result
            tool_calls += 1
            messages.append({"role": "assistant", "content": content})
            messages.append({"role": "user", "content": f"Tool result:\n{output[:4000]}"})
        else:
            # Unknown action — treat as final answer
            messages.append({"role": "assistant", "content": content})
            break

    # Capture diff and check compilation
    diff = get_diff(WORKDIR)
    compiled = cargo_check(WORKDIR) if diff else False
    raw_score = score_diff(diff, task)
    # Bonus point for compiling
    score = raw_score + (1 if compiled and raw_score >= 2 else 0)

    return {
        "id": task["id"],
        "cond": cond,
        "seed": seed,
        "turns": turns,
        "tool_calls": tool_calls,
        "diff_lines": len(diff.split("\n")),
        "target_changed": task["target_file"] in diff,
        "markers_found": raw_score >= 2,
        "compiled": compiled,
        "score": min(score, 3),
        "wall": time.time() - t_start,
    }


def main():
    seeds = [42, 17]
    conds = ["A", "B", "C"]
    all_results = []

    for cond in conds:
        for seed in seeds:
            for task in EDIT_TASKS:
                print(f"[{cond} seed={seed}] {task['id']}...", flush=True)
                r = run_edit_session(task, cond, seed)
                all_results.append(r)
                print(f"  score={r['score']}/3 compiled={r['compiled']} "
                      f"changed={r['target_changed']} turns={r['turns']}", flush=True)

    # Aggregate
    print("\n=== Edit Bench Results ===")
    for cond in conds:
        cond_rs = [r for r in all_results if r["cond"] == cond]
        avg = sum(r["score"] for r in cond_rs) / len(cond_rs) if cond_rs else 0
        compile_count = sum(1 for r in cond_rs if r["compiled"])
        print(f"{cond} ({CONDITION_NAMES[cond]}): avg {avg:.2f}/3 "
              f"compile={compile_count}/{len(cond_rs)} "
              f"wall_avg={sum(r['wall'] for r in cond_rs)/len(cond_rs):.0f}s")

    out_path = "$HOME/src/reliary8/bench/results/edit_bench.json"
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "w") as f:
        json.dump(all_results, f, indent=2)
    print(f"\nResults saved to {out_path}")


if __name__ == "__main__":
    main()