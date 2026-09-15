"""V71: Mutation edit bench — does code-intel accuracy convert to better edits?

Method: take the current reliary8 codebase, apply one validated one-line
mutation from a real fixed-bug class, confirm a specific test FAILS, then ask
a Pi Agent (same model, same prompt shape) to fix the symptom. The prompt
names the symptom, never the file/symbol/line — localization is part of the
task.

Conditions (same agent, only the code-intel layer differs):
- A: reliary MCP extension
- B: altbackend MCP extension
- C: no extensions (bash+grep+read+edit)

Scoring (mechanical):
  3 = target test passes AND full crate tests pass AND compiles
  2 = target test passes, compiles, but other tests regress
  1 = partial (compiles, target still failing)
  0 = no fix / broken build
Plus: wrong-file edits, tokens/cost from the Pi session file.

Usage:
  python3 bench/mutation_edit_bench.py --conds A,B,C --seed 42
  python3 bench/mutation_edit_bench.py --conds A --tasks m1_impact_tests
"""
import os
import sys
import re
import json
import time
import glob
import shutil
import argparse
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CORPUS_SRC = os.environ.get("MUT_CORPUS", os.path.join(ROOT))
WORKDIR_BASE = "/tmp/mutation_bench"
# Project name registered in altbackend for the current task workdir.
ALTBACKEND_PROJECT_FOR_TASK = {}
PI_BIN = os.environ.get("PI_BIN", os.path.expanduser("~/.local/bin/pi"))
RELIARY_EXT = os.path.join(HERE, "reliary_mcp_pi_extension.js")
ALTBACKEND_EXT = os.path.join(HERE, "altbackend_pi_extension.js")
ALTBACKEND_BIN = os.environ.get("ALTBACKEND_BIN", os.path.expanduser("~/.local/bin/codebase-memory-mcp"))
DEEPSEEK_MODEL = "deepseek/deepseek-v4-flash"
TASK_TIMEOUT = 900  # 15 min per task
# Shared target dir so dependency compilation is amortized across runs.
BENCH_TARGET = "/tmp/mutation_bench_target"
# Clean, isolated Pi config dir: prevents the user's settings.json extensions
# (reliary ext + gate.js) from contaminating conditions B/C.
BENCH_PI_DIR = "/tmp/mutation_bench_pi"


# ---------------------------------------------------------------- mutations
# Each mutation: file, old, new, failing_test (cargo filter), verify_pkg,
# symptom (what the agent is told), target_hint (for wrong-file metric;
# NEVER shown to the agent).
MUTATIONS = [
    {
        "id": "m1_impact_tests",
        "file": "crates/reliary-search/src/impact.rs",
        "old": '        || base.contains(".test.") || base.contains(".spec.")\n',
        "new": '        || base.contains(".spec.")\n',
        "target_test": "test_path_detection_positive",
        "verify_args": ["-p", "reliary-search", "--lib", "impact::"],
        "crate_args": ["-p", "reliary-search", "--lib"],
        "symptom": (
            "Some test files are no longer detected by the pre-edit impact tool: "
            "files following the JavaScript `foo.test.js` convention are classified as "
            "production code, so impact reports miss them. "
            "Find the cause and fix it. Run `cargo test -p reliary-search --lib impact::` "
            "to verify, then run `cargo test -p reliary-search --lib` to make sure nothing "
            "else broke. When done, commit with `git add -A && git commit -m fix`."
        ),
        "target_files": ["crates/reliary-search/src/impact.rs"],
    },
    {
        "id": "m2_verify_backticks",
        "file": "crates/reliary-agent/src/verify.rs",
        "old": '    let text = text.replace(\'`\', "");\n',
        "new": "",
        "target_test": "backticks_stripped",
        "verify_args": ["-p", "reliary-agent", "--bin", "reliary", "verify::"],
        "crate_args": ["-p", "reliary-agent", "--bin", "reliary"],
        "symptom": (
            "The claim verifier no longer recognises claims written with markdown "
            "backticks, e.g. \"`foo_bar` at `structural.rs:31`\". It should extract the "
            "same claims whether or not backticks are present. "
            "Find the cause and fix it. Run `cargo test -p reliary-agent --bin reliary verify::` "
            "to verify, then run `cargo test -p reliary-agent --bin reliary` to make sure "
            "nothing else broke. When done, commit with `git add -A && git commit -m fix`."
        ),
        "target_files": ["crates/reliary-agent/src/verify.rs"],
    },
    {
        "id": "m3_pascal_def",
        "file": "crates/reliary-search/src/structural.rs",
        "old": "    name.as_bytes().first().map(|&b| b.is_ascii_uppercase()).unwrap_or(false)\n",
        "new": "    name.as_bytes().first().map(|&b| b.is_ascii_alphabetic()).unwrap_or(false)\n",
        "target_test": "test_python_class",
        "verify_args": ["-p", "reliary-search", "--lib", "structural::tests"],
        "crate_args": ["-p", "reliary-search", "--lib"],
        "symptom": (
            "Python function detection regressed: lowercase `def foo(...):` lines are "
            "being classified as class/type definitions instead of functions. "
            "Only names that are actually class-like (start with an uppercase letter) "
            "should be typed as classes. "
            "Find the cause and fix it. Run `cargo test -p reliary-search --lib structural::tests` "
            "to verify, then run `cargo test -p reliary-search --lib` to make sure nothing "
            "else broke. When done, commit with `git add -A && git commit -m fix`."
        ),
        "target_files": ["crates/reliary-search/src/structural.rs"],
    },
    {
        "id": "m4_testplan_mirror",
        "file": "crates/reliary-search/src/test_plan.rs",
        "old": '        out.push(format!("tests/{}", tail_base));\n        out.push(format!("test/{}", tail_base));\n',
        "new": '        out.push(format!("test/{}", tail_base));\n',
        "target_test": "mirror_candidates_src_layout",
        "verify_args": ["-p", "reliary-search", "--lib", "test_plan::"],
        "crate_args": ["-p", "reliary-search", "--lib"],
        "symptom": (
            "The test-plan tool misses the conventional tests location: for a source "
            "file under `src/foo.rs`, it no longer suggests a mirror test file under "
            "`tests/foo.rs` — only the less common `test/` directory is suggested. "
            "Find the cause and fix it. Run `cargo test -p reliary-search --lib test_plan::` "
            "to verify, then run `cargo test -p reliary-search --lib` to make sure nothing "
            "else broke. When done, commit with `git add -A && git commit -m fix`."
        ),
        "target_files": ["crates/reliary-search/src/test_plan.rs"],
    },
]


# ---------------------------------------------------------------- workdir
def setup_workdir(dest):
    if os.path.exists(dest):
        shutil.rmtree(dest)
    shutil.copytree(
        CORPUS_SRC, dest,
        ignore=shutil.ignore_patterns(
            "target", "node_modules", "dist", "build", ".git",
            "bench/results", ".reliary", "docs/archive"),
    )
    subprocess.run(["git", "init", "-q"], cwd=dest, check=True)
    subprocess.run(["git", "config", "user.email", "bench@reliary"], cwd=dest, check=True)
    subprocess.run(["git", "config", "user.name", "bench"], cwd=dest, check=True)
    subprocess.run(["git", "add", "-A"], cwd=dest, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "initial"], cwd=dest, check=True)


def apply_mutation(workdir, mut):
    p = os.path.join(workdir, mut["file"])
    s = open(p).read()
    if mut["old"] not in s:
        return False
    open(p, "w").write(s.replace(mut["old"], mut["new"], 1))
    subprocess.run(["git", "add", "-A"], cwd=workdir, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "mutate"], cwd=workdir, check=True)
    subprocess.run(["git", "tag", "-f", "mutbase"], cwd=workdir, check=True)
    return True


def run_tests(workdir, mut, extra_env):
    """Run the target test filter. Returns (target_passed, target_output)."""
    r = subprocess.run(
        ["cargo", "test", "--release"] + mut["verify_args"],
        capture_output=True, text=True, cwd=workdir, timeout=900, env=extra_env,
    )
    out = r.stdout + r.stderr
    passed = r.returncode == 0
    if not passed:
        # distinguish compile error from test failure
        if "error[" in out or "error: could not compile" in out:
            return False, out
    return passed, out


def crate_tests_pass(workdir, mut, extra_env):
    r = subprocess.run(
        ["cargo", "test", "--release"] + mut.get("crate_args", ["-p", "reliary-search", "--lib"]),
        capture_output=True, text=True, cwd=workdir, timeout=900, env=extra_env,
    )
    return r.returncode == 0


def altbackend_index(workdir):
    """Index the workdir in altbackend; return the project name it assigned."""
    try:
        r = subprocess.run(
            [ALTBACKEND_BIN, "cli", "index_repository",
             json.dumps({"repo_path": workdir, "mode": "fast"})],
            capture_output=True, text=True, timeout=900,
        )
        for line in reversed((r.stdout or "").splitlines()):
            try:
                d = json.loads(line)
            except Exception:
                continue
            if isinstance(d, dict) and d.get("project"):
                return d["project"]
    except Exception as e:
        print(f"[altbackend index] {e}", flush=True)
    return None


def setup_bench_pi_dir():
    """Create an isolated Pi agent dir with auth+models but no extensions."""
    os.makedirs(BENCH_PI_DIR, exist_ok=True)
    real = os.path.expanduser("~/.pi/agent")
    for fname in ("auth.json", "models.json", "models-store.json"):
        src = os.path.join(real, fname)
        if os.path.exists(src):
            shutil.copy(src, os.path.join(BENCH_PI_DIR, fname))
    with open(os.path.join(BENCH_PI_DIR, "settings.json"), "w") as f:
        json.dump({"version": 1}, f)
    return BENCH_PI_DIR


def build_pi_args(cond):
    base = [PI_BIN, "-p", "--model", DEEPSEEK_MODEL, "--mode", "json",
            "--approve", "--thinking", "off"]
    if cond == "A":
        base += ["--extension", RELIARY_EXT]
    elif cond == "B":
        base += ["--extension", ALTBACKEND_EXT]
    elif cond == "C":
        base += ["--no-extensions"]
    return base


def parse_session_usage(session_dir):
    """Sum input/output/cache/cost across the Pi session jsonl."""
    tot = {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0,
           "cost": 0.0, "records": 0}
    files = glob.glob(os.path.join(session_dir, "*.jsonl"))
    if not files:
        return tot
    p = max(files, key=os.path.getmtime)
    for line in open(p):
        try:
            d = json.loads(line)
        except Exception:
            continue
        m = d.get("message") or {}
        u = m.get("usage") if isinstance(m, dict) else None
        if u:
            tot["records"] += 1
            for k in ("input", "output", "cacheRead", "cacheWrite"):
                tot[k] += u.get(k, 0)
            c = u.get("cost") or {}
            tot["cost"] += c.get("total", 0) if isinstance(c, dict) else 0
    return tot


def count_tool_calls(session_dir):
    """Count assistant tool-use records (rough turn proxy)."""
    files = glob.glob(os.path.join(session_dir, "*.jsonl"))
    if not files:
        return 0
    p = max(files, key=os.path.getmtime)
    n = 0
    for line in open(p):
        try:
            d = json.loads(line)
        except Exception:
            continue
        m = d.get("message") or {}
        if not isinstance(m, dict):
            continue
        if m.get("role") == "assistant":
            content = m.get("content") or []
            if isinstance(content, list):
                n += sum(1 for c in content if isinstance(c, dict) and c.get("type") == "toolCall")
            # pi sometimes records tool calls under 'toolCalls'
            n += len(m.get("toolCalls") or [])
    return n


def changed_files(workdir):
    r = subprocess.run(["git", "diff", "mutbase", "--name-only", "--no-color"],
                       capture_output=True, text=True, cwd=workdir, timeout=30)
    return [l for l in r.stdout.split("\n") if l.strip()]


def run_task(mut, cond, seed):
    # Fixed path per task: altbackend's index is keyed by path, so the workdir
    # location must be stable across conditions. Re-copied fresh each run.
    workdir = os.path.join(WORKDIR_BASE, f"work_{mut['id']}")
    setup_workdir(workdir)
    if not apply_mutation(workdir, mut):
        return {"id": mut["id"], "cond": cond, "seed": seed, "score": 0,
                "error": "mutation failed to apply"}

    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = BENCH_TARGET
    env["NO_RELIARY_WATCHER"] = "1"
    env["RELIARY_WORKDIR"] = workdir
    env["RELIARY_BIN"] = os.path.join(ROOT, "target/release/reliary")
    env["ALTBACKEND_BIN"] = ALTBACKEND_BIN
    env["PI_CODING_AGENT_DIR"] = BENCH_PI_DIR
    if ALTBACKEND_PROJECT_FOR_TASK.get(mut["id"]):
        env["ALTBACKEND_PROJECT"] = ALTBACKEND_PROJECT_FOR_TASK[mut["id"]]

    # Confirm the target test FAILS pre-fix (pipeline gate).
    pre_passed, pre_out = run_tests(workdir, mut, env)
    if pre_passed:
        return {"id": mut["id"], "cond": cond, "seed": seed, "score": -1,
                "error": "mutation did not break target test (harness bug)"}

    session_dir = os.path.join(WORKDIR_BASE, f"sessions_{mut['id']}_{cond}_{seed}")
    os.makedirs(session_dir, exist_ok=True)
    pi_args = build_pi_args(cond) + ["--session-dir", session_dir, mut["symptom"]]

    t0 = time.time()
    timed_out = False
    try:
        r = subprocess.run(pi_args, capture_output=True, text=True,
                           timeout=TASK_TIMEOUT, cwd=workdir, env=env)
        rc = r.returncode
    except subprocess.TimeoutExpired:
        timed_out = True
        rc = -1
    wall = time.time() - t0

    usage = parse_session_usage(session_dir)
    calls = count_tool_calls(session_dir)
    files_touched = changed_files(workdir) if not timed_out else []

    # Auto-commit any uncommitted edits so tests see them.
    subprocess.run(["git", "add", "-A"], cwd=workdir, capture_output=True)
    subprocess.run(["git", "commit", "-q", "-m", "agent"], cwd=workdir, capture_output=True)

    post_passed, post_out = run_tests(workdir, mut, env)
    compiles = "error[" not in (post_out or "") and "could not compile" not in (post_out or "")
    crate_ok = crate_tests_pass(workdir, mut, env) if post_passed else False

    if post_passed and crate_ok:
        score = 3
    elif post_passed:
        score = 2
    elif compiles and not timed_out:
        score = 1
    else:
        score = 0

    wrong_file = any(f not in mut["target_files"] for f in files_touched) if files_touched else False
    # Same weighted-cost unit as long_session_bench: uncached input + 10% of
    # cached input + 4x output (token-equivalents; DeepSeek input:output 1:2
    # in dollars but the historical bench unit is 1:4 — consistent here).
    tokens_in_total = usage["input"] + usage["cacheRead"]
    billed = usage["input"] + int(usage["cacheRead"] * 0.1) + 4 * usage["output"]
    return {
        "id": mut["id"], "cond": cond, "seed": seed,
        "score": score,
        "target_test_passed": post_passed,
        "crate_tests_pass": crate_ok,
        "compiles": compiles,
        "timed_out": timed_out,
        "files_touched": files_touched,
        "wrong_file": wrong_file,
        "turns_calls": calls,
        "tokens_in": usage["input"],
        "tokens_out": usage["output"],
        "cache_read": usage["cacheRead"],
        "cost_usd": round(usage["cost"], 6),
        "tokens_in_total": tokens_in_total,
        "billed_cost_tokens": billed,
        "wall": round(wall, 1),
        "rc": rc,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--conds", default="A,B,C")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--tasks", default="",
                    help="comma-separated task ids (default: all 4)")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    conds = args.conds.split(",")
    want = set(t.strip() for t in args.tasks.split(",") if t.strip())
    tasks = [m for m in MUTATIONS if not want or m["id"] in want]

    os.makedirs(WORKDIR_BASE, exist_ok=True)
    os.makedirs(BENCH_TARGET, exist_ok=True)
    setup_bench_pi_dir()

    results = []
    for mut in tasks:
        # One-time: index the (mutated) task workdir in altbackend so
        # condition B has a matching project for all conditions of this task.
        prep_dir = os.path.join(WORKDIR_BASE, f"work_{mut['id']}")
        setup_workdir(prep_dir)
        if apply_mutation(prep_dir, mut):
            proj = altbackend_index(prep_dir)
            if proj:
                ALTBACKEND_PROJECT_FOR_TASK[mut["id"]] = proj
                print(f"[altbackend] task {mut['id']} indexed as '{proj}'", flush=True)
        else:
            print(f"[WARN] prep mutation failed for {mut['id']}", flush=True)
        for cond in conds:
            print(f"\n=== {mut['id']} | cond {cond} | seed {args.seed} ===", flush=True)
            r = run_task(mut, cond, args.seed)
            results.append(r)
            print(json.dumps({k: v for k, v in r.items()
                              if k in ("score", "target_test_passed", "crate_tests_pass",
                                       "wrong_file", "files_touched", "turns_calls",
                                       "tokens_in", "tokens_out", "cost_usd", "wall", "error")}),
                  flush=True)

    out = args.out or os.path.join(HERE, "results", "mutation_bench.json")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(results, f, indent=1)
    print(f"\nWrote {out}")

    # Summary
    print("\n=== SUMMARY ===")
    by_cond = {}
    for r in results:
        by_cond.setdefault(r["cond"], []).append(r)
    for cond in sorted(by_cond):
        rs = by_cond[cond]
        mean = sum(r.get("score", 0) for r in rs) / len(rs)
        f2p = sum(1 for r in rs if r.get("target_test_passed")) / len(rs)
        billed = sum(r.get("billed_cost_tokens", 0) for r in rs)
        calls = sum(r.get("turns_calls", 0) for r in rs)
        wall = sum(r.get("wall", 0) for r in rs)
        wrong = sum(1 for r in rs if r.get("wrong_file"))
        print(f"  {cond}: score {mean:.2f}/3 | f2p {f2p:.0%} | billed {billed} | wrong-file {wrong} | calls {calls} | wall {wall:.0f}s")


if __name__ == "__main__":
    main()
