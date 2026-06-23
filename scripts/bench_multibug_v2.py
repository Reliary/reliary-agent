"""Multi-bug benchmark: 6 Python files, 6 turns, real tests.

Each turn: LLM fixes one specific bug, run pytest to verify.
Both conditions must pass all 6 tests to count as success.
"""
import json, os, subprocess, sys, time, random, shutil

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
GATE = os.path.expanduser("~/src/reliary-agent/pi/gate.js")
REPO = "/tmp/bench_multibug"

PROMPTS = [
    "Fix rate_limiter.py: allow() always returns True even when _tokens is 0. It should return False when _tokens <= 0.",
    "Fix sort_utils.py: merge() has an infinite loop with <= instead of < in the while condition.",
    "Fix config_reader.py: read_config() doesn't handle empty values - keys with empty strings are skipped.",
    "Fix cache.py: LRUCache put() silently fails when re-adding a key not in self.order.",
    "Fix validator.py: validate_phone() returns True for non-digit input, validate_age() returns True for None.",
    "Fix formatter.py: indent_code() decrements depth on } even when it's not at the start of a line.",
]

def reset_repo():
    subprocess.run(["git", "checkout", "."], capture_output=True, cwd=REPO)
    subprocess.run(["git", "clean", "-fd"], capture_output=True, cwd=REPO)
    subprocess.run(["rm", "-rf", "__pycache__", ".pytest_cache", ".reliary"], capture_output=True, cwd=REPO)

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
            elif d.get("type") == "tool_execution_start":
                tc += 1
        except Exception: pass
    return pt, ct, tc

def check_tests():
    r = subprocess.run(["python3", "-m", "pytest", "tests.py", "-v", "--tb=no"],
                       capture_output=True, text=True, timeout=60, cwd=REPO)
    # Count passed tests
    passed = r.stdout.count("PASSED")
    failed = "FAILED" in r.stdout or r.returncode != 0
    passed = passed if not failed else 0
    return passed, failed

def run_condition(cond, run_idx):
    sfile = f"/tmp/bench-mt-{cond}-r{run_idx}.json"
    if os.path.exists(sfile): os.remove(sfile)
    reset_repo()

    if cond == "baseline":
        set_ext(None)
        env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1", "DEEPSEEK_API_KEY": os.environ.get("DEEPSEEK_API_KEY", "")}
    else:
        set_ext(GATE)
        env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1", "RELIARY_MODE": "fast",
               "DEEPSEEK_API_KEY": os.environ.get("DEEPSEEK_API_KEY", "")}

    total_pt = total_ct = total_tc = 0
    total_wt = 0.0
    turn_results = []

    for ti, prompt in enumerate(PROMPTS):
        t0 = time.time()
        r = subprocess.run(
            [PI, "--model", "deepseek/deepseek-v4-flash",
             "--mode", "json", "--session", sfile, "--print", prompt],
            capture_output=True, text=True, timeout=600, env=env, cwd=REPO)
        wt = time.time() - t0
        pt, ct, tc = parse_usage(r.stdout)
        total_pt += pt
        total_ct += ct
        total_tc += tc
        total_wt += wt
        turn_results.append({"turn": ti+1, "pt": pt, "ct": ct, "tc": tc, "wt": round(wt, 1)})

    # Final test verification
    passed, failed = check_tests()
    return {
        "condition": cond, "run": run_idx,
        "pt": total_pt, "ct": total_ct, "tc": total_tc,
        "wc": total_pt + 2*total_ct,  # 1:2 ratio for V4 Flash
        "wt": round(total_wt, 1),
        "passed": passed, "ok": not failed,
        "turns": turn_results,
    }

if __name__ == "__main__":
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 3
    print(f"Multi-bug bench: {runs} runs x 2 conditions (baseline, gate)")
    print(f"Repo: {REPO}")
    print()

    trials = []
    for ri in range(runs):
        cs = ["baseline", "gate"]
        random.shuffle(cs)
        for c in cs:
            trials.append((c, ri))

    all_results = []
    for cond, ri in trials:
        print(f"  [{ri+1}/{runs}] {cond}...", end="", flush=True)
        m = run_condition(cond, ri)
        v = "OK" if m["ok"] else f"FAIL({m['passed']}/6 pass)"
        print(f" pt={m['pt']} ct={m['ct']} {m['wt']}s wc={m['wc']} {v}")
        all_results.append(m)

    # Summary
    print()
    print("=" * 70)
    base = [r for r in all_results if r["condition"] == "baseline"]
    gate = [r for r in all_results if r["condition"] == "gate"]
    for name, lst in [("Baseline", base), ("Gate", gate)]:
        if not lst: continue
        avg_pt = sum(r["pt"] for r in lst) / len(lst)
        avg_ct = sum(r["ct"] for r in lst) / len(lst)
        avg_wc = sum(r["wc"] for r in lst) / len(lst)
        avg_wt = sum(r["wt"] for r in lst) / len(lst)
        ok = sum(1 for r in lst if r["ok"])
        print(f"  {name:<10} pt={avg_pt:<6.0f} ct={avg_ct:<6.0f} wc={avg_wc:<8.0f} wt={avg_wt:<5.0f}s acc={ok}/{len(lst)}")
    if base and gate:
        b_wc = sum(r["wc"] for r in base) / len(base)
        g_wc = sum(r["wc"] for r in gate) / len(gate)
        delta = (g_wc - b_wc) / max(b_wc, 1) * 100
        print(f"\n  Weighted cost change: {delta:+.1f}%")

    out = "/tmp/bench_multibug_v2_results.json"
    with open(out, "w") as f:
        json.dump(all_results, f, indent=2, default=str)
    print(f"\nSaved to {out}")
