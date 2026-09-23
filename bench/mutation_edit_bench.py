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
TASK_TIMEOUT = int(os.environ.get("MUT_TIMEOUT", "600"))  # bounded per task
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
        "id": "m1_test_convention",
        "file": "crates/reliary-search/src/impact.rs",
        "old": '        || base.contains(".test.") || base.contains(".spec.")\n',
        "new": '',
        "target_test": "test_file_classification_covers_common_conventions",
        "verify_args": ["-p", "reliary-search", "--test", "symptom_checks", "test_file_classification_covers_common_conventions"],
        "crate_args": ["--workspace"],
        # Bare symptom: no test command, no assertion vocabulary.
        "symptom": (
            "The pre-edit impact report overstates risk: it counts review-only "
            "support files as production surface, so blast radius and risk come out "
            "too high. A reviewer looking at the same change would discount those "
            "files. The classification of what counts as production is incomplete. "
            "Fix it. This workspace has been stripped of tests — verify with "
            "`cargo check` only, do not run tests. When done, stop editing."
        ),
        "target_files": ["crates/reliary-search/src/impact.rs"],
    },
    {
        "id": "m2_pascal_drop",
        "file": "crates/reliary-search/src/keywords.rs",
        "old": (
            "    if raw_token.starts_with(|c: char| c.is_ascii_uppercase()) {\n"
            "        return false;\n"
            "    }\n"
            "    keywords().contains(stemmed)\n"
        ),
        "new": "    keywords().contains(stemmed)\n",
        "target_test": "test_symbol_names_round_trip_through_the_index",
        "verify_args": ["-p", "reliary-search", "--test", "symptom_checks", "test_symbol_names_round_trip_through_the_index"],
        "crate_args": ["--workspace"],
        "symptom": (
            "Looking a symbol up by its exact name sometimes returns nothing even "
            "though that symbol is plainly defined in the source. It only happens "
            "for a subset of names; most lookups work. Investigate why some defined "
            "names are not reachable by lookup and fix it. This workspace has been "
            "stripped of tests — verify with `cargo check` only, do not run tests. "
            "When done, stop editing."
        ),
        "target_files": ["crates/reliary-search/src/keywords.rs"],
    },
    {
        "id": "m3_method_line",
        "file": "crates/reliary-search/src/callgraph_v2.rs",
        "old": "                        line: child.start_line,\n",
        "new": "                        line: child.start_line + 1,\n",
        "target_test": "test_reported_method_lines_point_at_the_declaration",
        "verify_args": ["-p", "reliary-search", "--test", "symptom_checks", "test_reported_method_lines_point_at_the_declaration"],
        "crate_args": ["--workspace"],
        "symptom": (
            "Locations reported for members of a type are wrong: the line cited does "
            "not actually contain the member's declaration — it lands on the line "
            "below it. This makes every member location unusable for navigation. "
            "Fix it. This workspace has been stripped of tests — verify with "
            "`cargo check` only, do not run tests. When done, stop editing."
        ),
        "target_files": ["crates/reliary-search/src/callgraph_v2.rs"],
    },
    {
        "id": "m4_brace_line",
        "file": "crates/reliary-search/src/brace_graph.rs",
        "old": "        let line_no = (line_idx + 1) as i32;\n",
        "new": "        let line_no = line_idx as i32;\n",
        "target_test": "test_reported_method_lines_point_at_the_declaration",
        "verify_args": ["-p", "reliary-search", "--test", "symptom_checks", "test_reported_method_lines_point_at_the_declaration"],
        "crate_args": ["--workspace"],
        "symptom": (
            "Structural locations are unreliable: the number the tool reports for a "
            "construct is frequently lower than where the editor shows it, so "
            "navigation lands on the wrong line. The error is systematic, not "
            "random. Fix it. This workspace has been stripped of tests — verify "
            "with `cargo check` only, do not run tests. When done, stop editing."
        ),
        "target_files": ["crates/reliary-search/src/brace_graph.rs"],
    },
    {
        "id": "m5_visibility",
        "file": "crates/reliary-search/src/callgraph_v2.rs",
        "old": "        && (b[3] == b' ' || b[3] == b'\\t' || b[3] == b'(')\n",
        "new": "        && (b[3] == b' ' || b[3] == b'\\t')\n",
        "target_test": "test_visibility_reflects_all_public_forms",
        "verify_args": ["-p", "reliary-search", "--test", "symptom_checks", "test_visibility_reflects_all_public_forms"],
        "crate_args": ["--workspace"],
        "symptom": (
            "The API listing mislabels accessibility: some members that are meant "
            "to be usable from elsewhere are shown as internal, so the external "
            "surface looks smaller than it is. Only some declaration spellings are "
            "affected. Fix it. This workspace has been stripped of tests — verify "
            "with `cargo check` only, do not run tests. When done, stop editing."
        ),
        "target_files": ["crates/reliary-search/src/callgraph_v2.rs"],
    },
    {
        "id": "m6_dead_cross_file",
        "file": "crates/reliary-dead/src/lib.rs",
        "old": (
            "        let total_occ = *all_counts.get(name).unwrap_or(&0);\n"
            "        let def_occ = locations.len();\n"
            "        if total_occ > def_occ { continue; }\n"
        ),
        "new": (
            "        let total_occ = locations.len();\n"
            "        let def_occ = locations.len();\n"
            "        if total_occ > def_occ { continue; }\n"
        ),
        "target_test": "test_dead_function_cross_file",
        "verify_args": ["-p", "reliary-dead", "test_dead_function_cross_file"],
        "crate_args": ["--workspace"],
        "symptom": (
            "The unused-code report is not trustworthy: it lists things as unused "
            "that are demonstrably used elsewhere in the project, so every result "
            "needs manual re-checking. Fix the false positives. This workspace has "
            "been stripped of tests — verify with `cargo check` only, do not run "
            "tests. When done, stop editing."
        ),
        "target_files": ["crates/reliary-dead/src/lib.rs"],
    },
]


# ---------------------------------------------------------------- workdir
def setup_workdir(dest, strip_tests=False):
    if os.path.exists(dest):
        shutil.rmtree(dest)
    shutil.copytree(
        CORPUS_SRC, dest,
        ignore=shutil.ignore_patterns(
            "target", "node_modules", "dist", "build", ".git",
            "bench/results", ".reliary", "docs/archive"),
    )
    if strip_tests:
        strip_test_oracles(dest)
    subprocess.run(["git", "init", "-q"], cwd=dest, check=True)
    subprocess.run(["git", "config", "user.email", "bench@reliary"], cwd=dest, check=True)
    subprocess.run(["git", "config", "user.name", "bench"], cwd=dest, check=True)
    subprocess.run(["git", "add", "-A"], cwd=dest, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "initial"], cwd=dest, check=True)


def strip_test_oracles(root):
    """Remove every test from the agent's workspace — uniformly, all conditions.

    A runnable failing test is a localization oracle: the assertion names the
    subject and semantics, so `grep` on the assertion finds the fix site. To
    measure localization from a prose symptom, the agent's workspace must
    contain NO test that reveals the answer. The hidden scoring tests are
    injected only after the agent stops.

    Grammar-free: truncate each source file at its first `#[cfg(test)]` and
    delete `crates/*/tests/` directories.
    """
    import glob as _glob
    # `#[cfg(any())]` is always false: the test module is excluded from
    # compilation but the file stays syntactically valid (truncating at the
    # attribute can leave a dangling doc comment or break the module).
    pat = re.compile(r'^([ \t]*)#\[cfg\(test\)\]', re.M)
    for p in _glob.glob(os.path.join(root, "crates", "**", "*.rs"), recursive=True):
        try:
            s = open(p).read()
        except OSError:
            continue
        s2 = pat.sub(r'\1#[cfg(any())]', s)
        if s2 != s:
            open(p, "w").write(s2)
    for d in _glob.glob(os.path.join(root, "crates", "*", "tests")):
        shutil.rmtree(d, ignore_errors=True)


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
    #
    # The agent's workdir has ALL tests stripped (uniformly, every condition):
    # a runnable failing test is a localization oracle, so it is withheld. The
    # symptom in the prompt is the only clue. Scoring runs on a separate
    # pristine tree with the agent's source patch applied and the real tests
    # restored.
    workdir = os.path.join(WORKDIR_BASE, f"work_{mut['id']}")
    setup_workdir(workdir, strip_tests=True)
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

    # Capture the agent's source patch (no tests exist in its workdir).
    diff = subprocess.run(["git", "diff", "mutbase", "--no-color"],
                          capture_output=True, text=True, cwd=workdir, timeout=60).stdout
    files_touched = [l for l in subprocess.run(
        ["git", "diff", "mutbase", "--name-only"], capture_output=True,
        text=True, cwd=workdir, timeout=30).stdout.splitlines() if l.strip()]

    # Score on a pristine tree: real tests present, mutation applied, then the
    # agent's patch re-applied on top.
    score_tree = os.path.join(WORKDIR_BASE, f"score_{mut['id']}")
    setup_workdir(score_tree, strip_tests=False)
    if not apply_mutation(score_tree, mut):
        return {"id": mut["id"], "cond": cond, "seed": seed, "score": 0,
                "error": "mutation failed to apply on score tree"}
    patch_ok = True
    if diff.strip():
        p = subprocess.run(["git", "apply", "--3way", "-"],
                           input=diff, capture_output=True, text=True, cwd=score_tree)
        patch_ok = p.returncode == 0

    post_passed, post_out = run_tests(score_tree, mut, env)
    compiles = "error[" not in (post_out or "") and "could not compile" not in (post_out or "")
    crate_ok = crate_tests_pass(score_tree, mut, env) if post_passed else False

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
        "patch_applied": patch_ok,
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
