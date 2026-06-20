"""Paired benchmark: baseline vs gate interleaved, with session capture and edit diffing.
3 runs randomized per condition. Captures full session JSON for post-hoc analysis.

Usage: python3 bench_paired.py [runs=3]
"""
import json, os, subprocess, sys, time, random

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from bench_lib import cwd_prefix

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
GATE = os.path.expanduser("~/src/reliary-agent/pi/gate.js")
RELIARY_BIN = os.path.expanduser("~/.local/bin/reliary-agent")
REPO = os.path.expanduser("~/src/stria")
OUT = os.path.expanduser("~/bench_paired_results.json")

def reset_bug():
    subprocess.run(["git", "stash"], capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"], capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "master", "--", "."], capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"], capture_output=True, cwd=REPO)
    subprocess.run(["rm", "-rf", ".stria"], capture_output=True, cwd=REPO)

def set_ext(ext_path):
    with open(SETTINGS, "w") as f:
        if ext_path:
            json.dump({"version": 1, "packages": [ext_path], "extensions": [ext_path]}, f, indent=2)
        else:
            json.dump({"version": 1, "packages": [], "extensions": []}, f, indent=2)

def parse_usage(stdout):
    pt = ct = tc = 0
    for line in stdout.splitlines():
        if not line.startswith("{"): continue
        try:
            d = json.loads(line)
            if d.get("type") == "message_end":
                u = d.get("message", {}).get("usage", {})
                pt += u.get("input", 0)
                ct += u.get("output", 0)
                if "toolName" in d.get("message", {}): tc += 1
            elif d.get("type") == "tool_execution_start": tc += 1
        except: pass
    return pt, ct, tc

def extract_edits(session_file):
    """Extract edit oldText/newText pairs from a session JSON file."""
    edits = []
    if not os.path.exists(session_file): return edits
    with open(session_file) as f:
        for line in f:
            try:
                d = json.loads(line)
            except: continue
            # Look for tool_call with edit
            if d.get("type") == "tool_call" and d.get("toolName") == "edit":
                inp = d.get("input", {})
                file_path = inp.get("path", inp.get("file", "?"))
                edits_list = inp.get("edits", [inp])
                for ed in edits_list:
                    old = ed.get("oldText", "")
                    new = ed.get("newText", "")
                    if old or new:
                        edits.append({"file": file_path, "oldText": old, "newText": new})
    return edits

def run_condition(cond, run_idx):
    sfile = f"/tmp/bench-paired-{cond}-r{run_idx}.json"
    if os.path.exists(sfile): os.remove(sfile)

    reset_bug()

    if cond == "baseline":
        set_ext(None)
        env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1"}
    else:
        set_ext(GATE)
        env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1", "RELIARY_MODE": "fast"}

    prompts = [
        "Read src/zone.rs. Understand the line_zone function — how does it classify prose vs code lines?",
        "Run 'cargo test --bin stria -- zone --quiet 2>&1' and list all failures.",
        "Fix line_zone so all zone tests pass. The bug is in the prose classification thresholds.",
        "Run 'cargo test --bin stria -- zone --quiet 2>&1' to verify all tests pass.",
    ]

    total_pt = total_ct = total_tc = total_wt = 0.0
    for ti, task in enumerate(prompts):
        if ti == 0:
            task = cwd_prefix(REPO) + task
        t0 = time.time()
        model = os.environ.get("BENCH_MODEL", "deepseek/deepseek-v4-flash")
        r = subprocess.run([PI, "--model", model,
            "--mode", "json", "--session", sfile, "--print", task],
            capture_output=True, text=True, timeout=600, env=env, cwd=REPO)
        wt = time.time() - t0
        pt, ct, tc = parse_usage(r.stdout)
        total_pt += pt
        total_ct += ct
        total_tc += tc
        total_wt += wt

    # Verify tests pass
    r2 = subprocess.run(["cargo", "test", "--bin", "stria", "--", "zone", "--quiet"],
        capture_output=True, text=True, timeout=60, cwd=REPO)
    ok = "FAILED" not in r2.stdout

    # Get actual file diff
    diff = subprocess.run(["git", "diff", "src/zone.rs"], capture_output=True, text=True, cwd=REPO)
    added_lines = [l[1:] for l in diff.stdout.splitlines() if l.startswith("+") and not l.startswith("+++")]

    # Extract edits from session
    edits = extract_edits(sfile)

    wc = total_pt + 4 * total_ct
    return {
        "condition": cond,
        "run": run_idx,
        "pt": int(total_pt), "ct": int(total_ct), "tc": int(total_tc),
        "wc": int(wc), "wt": round(total_wt, 1),
        "ok": ok,
        "fix_lines": added_lines,
        "edit_count": len(edits),
        "edits": edits,
        "session_file": sfile,
    }

if __name__ == "__main__":
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 3
    conditions = sum(([("baseline", "gate")] * runs), ())  # paired: baseline then gate each run
    random.shuffle(list(conditions))  # shuffle within runs actually no - keep paired

    # Actually: interleave within each run
    trials = []
    for ri in range(runs):
        trial_conds = ["baseline", "gate"]
        random.shuffle(trial_conds)
        for cond in trial_conds:
            trials.append((cond, ri))

    print(f"Paired benchmark: {runs} runs, {len(trials)} conditions ({runs} baseline, {runs} gate)")
    print(f"Repo: {REPO}")
    print()

    # Pre-setup
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"], capture_output=True, cwd=REPO)

    all_results = []

    for cond, ri in trials:
        print(f"  [{ri+1}/{runs}] {cond}...")
        m = run_condition(cond, ri)
        v = "+OK" if m["ok"] else "FAIL"
        fix_preview = m["fix_lines"][0][:60] if m["fix_lines"] else "(none)"
        print(f"    pt={m['pt']} ct={m['ct']} tc={m['tc']} {m['wt']}s wc={m['wc']} {v}")
        print(f"    edits={m['edit_count']} fix={fix_preview}")
        all_results.append(m)

    # Report
    print()
    print("=" * 70)
    base = [r for r in all_results if r["condition"] == "baseline"]
    gate = [r for r in all_results if r["condition"] == "gate"]

    from statistics import mean
    for name, lst in [("Baseline", base), ("Gate", gate)]:
        if not lst: continue
        avg_pt = mean(r["pt"] for r in lst)
        avg_ct = mean(r["ct"] for r in lst)
        avg_wc = mean(r["wc"] for r in lst)
        avg_wt = mean(r["wt"] for r in lst)
        avg_tc = mean(r["tc"] for r in lst)
        ok_count = sum(1 for r in lst if r["ok"])
        print(f"  {name:<10} pt={avg_pt:<6.0f} ct={avg_ct:<6.0f} wc={avg_wc:<8.0f} wt={avg_wt:<5.0f}s tc={avg_tc:<4.0f} acc={ok_count}/{len(lst)}")

    if base and gate:
        b_wc = mean(r["wc"] for r in base)
        g_wc = mean(r["wc"] for r in gate)
        delta = (g_wc - b_wc) / max(b_wc, 1) * 100
        print(f"\n  Weighted cost change: {delta:+.1f}%")

    # Edit diff analysis
    print("\n  --- Edit comparison ---")
    for ri in range(runs):
        b_edits = [r for r in base if r["run"] == ri]
        g_edits = [r for r in gate if r["run"] == ri]
        b_fix = b_edits[0]["fix_lines"] if b_edits else []
        g_fix = g_edits[0]["fix_lines"] if g_edits else []
        b_str = "; ".join(b_fix[:3])[:80] if b_fix else "(none)"
        g_str = "; ".join(g_fix[:3])[:80] if g_fix else "(none)"
        b_ok = b_edits[0]["ok"] if b_edits else False
        g_ok = g_edits[0]["ok"] if g_edits else False
        match = "MATCH" if b_fix == g_fix else "DIFF" if (b_fix and g_fix) else "N/A"
        print(f"  Run {ri+1}: baseline={b_str}")
        print(f"           gate={g_str}  [{match}] ok={g_ok}")

        # Show full file diff for analysis
        if match == "DIFF" and b_ok and g_ok:
            print(f"  Both pass but fixes differ — manual review recommended")

    with open(OUT, "w") as f:
        json.dump(all_results, f, indent=2, default=str)
    print(f"\nSaved to {OUT}")
