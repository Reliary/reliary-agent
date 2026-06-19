"""Hard bug + long session benchmark.

3 conditions × 3 runs interleaved (9 total sessions).
Each session is 9 turns to exercise guard + compression compounding.

b1: baseline  — no gate, direct API
g1: gate-only — gate.js with RELIARY_MODE=fast, direct API
r1: recommended — gate + proxy + guard, anti disabled

Usage: python3 bench_hard.py
Requires: daemon running on :9090 with RELIARY_UPSTREAM_URL set
"""
import json, os, subprocess, sys, time, random, shutil

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "pi", "gate.js"))
REPO = "/tmp/bench_repo"

SETTINGS_BAK = SETTINGS + ".hardbak"
MODELS_BAK = MODELS + ".hardbak"

CONDITIONS = [
    {"label": "baseline",    "needs_proxy": False, "needs_gate": False, "env": {}},
    {"label": "gate-only",   "needs_proxy": False, "needs_gate": True,
     "env": {"RELIARY_MODE": "fast"}},
    {"label": "recommended", "needs_proxy": True,  "needs_gate": True,
     "env": {"RELIARY_MODE": "strict",
             "RELIARY_FEATURES": "compress,convWindow,readEnrichment,healEdit"}},
]

TURNS = [
    "Read src/core.py, src/pipeline.py, src/analysis.py, src/storage.py and explain what each function does and how they depend on each other.",
    "Run 'python3 -m pytest tests/ -v' and report every failure line.",
    "Fix the bug in src/pipeline.py — run_pipeline does not use calculate_score correctly. The score argument to process_record should be the result of calculate_score(rec, weights).",
    "Run 'python3 -m pytest tests/ -v' and verify the pipeline test now passes.",
    "Fix the bug in src/analysis.py — normalize_output computes wrong values. The effective_score should be divided by max_score, not just multiplied by 100.",
    "Run 'python3 -m pytest tests/ -v' and verify the analysis test now passes.",
    "Fix the bug in src/storage.py — save_results calls 'validate_results' which does not exist. Remove that call.",
    "Run 'python3 -m pytest tests/ -v' and verify all 8 tests pass.",
    "Print the final content of src/storage.py.",
]

def save_configs():
    for src, dst in [(SETTINGS, SETTINGS_BAK), (MODELS, MODELS_BAK)]:
        if os.path.exists(src): shutil.copy2(src, dst)

def restore_configs():
    for src, dst in [(SETTINGS_BAK, SETTINGS), (MODELS_BAK, MODELS)]:
        if os.path.exists(dst): shutil.move(dst, src)

def route_pi_through_proxy(enable):
    with open(MODELS) as f: cfg = json.load(f)
    for pname, pdata in cfg.get("providers", {}).items():
        if enable and "deep" in pname.lower():
            pdata["baseUrl"] = "http://127.0.0.1:9090/v1"
        elif not enable and "deep" in pname.lower():
            pdata["baseUrl"] = "https://api.deepseek.com/v1"
    with open(MODELS, "w") as f: json.dump(cfg, f, indent=2)

def reset_repo():
    # Clean reset to bench-bug state
    subprocess.run(["git", "checkout", "-f", "bench-bug"], capture_output=True, cwd=REPO)
    subprocess.run(["git", "clean", "-fd"], capture_output=True, cwd=REPO)
    # Remove python caches that could mask stale refs
    subprocess.run(["rm", "-rf", "src/__pycache__", "tests/__pycache__", ".pytest_cache"],
                   capture_output=True, cwd=REPO)

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
        except Exception:
            pass
    return pt, ct, tc

def scrape_stdout(stdout):
    """Scrape assistant text responses from Pi JSON-stream output."""
    texts = []
    for line in stdout.splitlines():
        if not line.startswith("{"): continue
        try:
            d = json.loads(line)
            if d.get("type") == "text" and d.get("text", "").strip():
                texts.append(d["text"])
        except Exception:
            pass
    return texts

def check_test_status(repo):
    """Run tests and return (all_pass: bool, stdout_str)."""
    r = subprocess.run(["python3", "-m", "pytest", "tests/", "-v"],
                       capture_output=True, text=True, timeout=60, cwd=repo)
    return "FAILED" not in r.stdout and r.returncode == 0, r.stdout

def run_turn(pid, sfile, prompt, env, cwd):
    t0 = time.time()
    r = subprocess.run(
        [pid, "--model", "deepseek/deepseek-v4-flash",
         "--mode", "json", "--session", sfile, "--print", prompt],
        capture_output=True, text=True, timeout=600, env=env, cwd=cwd)
    wt = time.time() - t0
    pt, ct, tc = parse_usage(r.stdout)
    texts = scrape_stdout(r.stdout)
    return pt, ct, tc, wt, texts

def run_condition(cond, run_idx):
    sfile = f"/tmp/bench-hard-{cond['label']}-r{run_idx}.json"
    if os.path.exists(sfile): os.remove(sfile)

    reset_repo()
    route_pi_through_proxy(cond["needs_proxy"])
    set_ext(GATE if cond["needs_gate"] else None)

    env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1"}
    env.update(cond["env"])

    total_pt = total_ct = total_tc = 0
    total_wt = 0.0
    turn_results = []

    for ti, prompt in enumerate(TURNS):
        pt, ct, tc, wt, texts = run_turn(PI, sfile, prompt, env, REPO)
        total_pt += pt
        total_ct += ct
        total_tc += tc
        total_wt += wt
        turn_results.append({
            "turn": ti + 1,
            "pt": pt, "ct": ct, "tc": tc,
            "wt": round(wt, 1),
            "response_preview": texts[0][:100] if texts else "(no text)",
        })

    all_pass, test_out = check_test_status(REPO)

    # Check what guard did — look for guard warnings in response texts
    guard_fired = any("guard" in t.get("response_preview", "").lower()
                      or "validate_results" in t.get("response_preview", "")
                      for t in turn_results)

    wc = total_pt + 4 * total_ct
    return {
        "feature": cond["label"],
        "run": run_idx,
        "pt": total_pt,
        "ct": total_ct,
        "tc": total_tc,
        "wc": wc,
        "wt": round(total_wt, 1),
        "turns": len(TURNS),
        "ok": all_pass,
        "guard_fired": guard_fired,
        "turn_results": turn_results,
        "test_output": test_out[:200],
    }

if __name__ == "__main__":
    runs = 3

    import urllib.request
    try:
        r = urllib.request.urlopen("http://127.0.0.1:9090/health", timeout=3)
        assert r.status == 200
    except Exception:
        print("ERROR: Daemon not reachable on :9090 — start it with RELIARY_UPSTREAM_URL")
        sys.exit(1)

    save_configs()
    print(f"Hard-bug/long-session bench: {runs} runs × {len(CONDITIONS)} conditions (interleaved)")
    print(f"Repo: {REPO}  Turns: {len(TURNS)}")
    print()

    all_trials = []
    try:
        for ri in range(1, runs + 1):
            order = list(CONDITIONS)
            random.shuffle(order)
            for cond in order:
                print(f"  [r{ri}] {cond['label']}: ", end="", flush=True)
                t0 = time.time()
                result = run_condition(cond, ri)
                elapsed = time.time() - t0
                ok_mark = "+OK" if result["ok"] else "FAIL"
                guard_mark = " GUARD" if result["guard_fired"] else ""
                print(f"pt={result['pt']} ct={result['ct']} tc={result['tc']} {result['wt']:.0f}s wc={result['wc']} {ok_mark}{guard_mark}  ({elapsed:.0f}s wall)")
                all_trials.append(result)
    finally:
        restore_configs()

    # Print summary
    print("\n" + "=" * 100)
    hdr = f"{'Condition':<14s} {'PT':>8s} {'CT':>8s} {'WC':>10s} {'WT':>7s} {'TC':>5s} {'Acc':>5s} {'Δ%':>7s} {'Guard':>6s}"
    print(hdr)
    print("-" * 100)

    baseline_trials = [t for t in all_trials if t["feature"] == "baseline"]
    if baseline_trials:
        bar_wc = sum(t["wc"] for t in baseline_trials) / len(baseline_trials)
        print(f"  {'baseline':<12s}  {sum(t['pt'] for t in baseline_trials)//len(baseline_trials):<8d}  {sum(t['ct'] for t in baseline_trials)//len(baseline_trials):<8d}  {bar_wc:<10.0f}  {sum(t['wt'] for t in baseline_trials)/len(baseline_trials):<6.1f}s  {sum(t['tc'] for t in baseline_trials)//len(baseline_trials):<5d}  {sum(1 for t in baseline_trials if t['ok'])}/{len(baseline_trials):<2}  —       {sum(1 for t in baseline_trials if t['guard_fired'])}/{len(baseline_trials):<2}")
    else:
        bar_wc = 1

    for cond in CONDITIONS:
        if cond["label"] == "baseline": continue
        trials = [t for t in all_trials if t["feature"] == cond["label"]]
        if not trials: continue
        avg_wc = sum(t["wc"] for t in trials) / len(trials)
        delta = (avg_wc - bar_wc) / bar_wc * 100
        ok_cnt = sum(1 for t in trials if t["ok"])
        guard_cnt = sum(1 for t in trials if t["guard_fired"])
        print(f"  {cond['label']:<12s}  {sum(t['pt'] for t in trials)//len(trials):<8d}  {sum(t['ct'] for t in trials)//len(trials):<8d}  {avg_wc:<10.0f}  {sum(t['wt'] for t in trials)/len(trials):<6.1f}s  {sum(t['tc'] for t in trials)//len(trials):<5d}  {ok_cnt}/{len(trials):<2}  {delta:>+6.1f}%  {guard_cnt}/{len(trials):<2}")

    # Per-run breakdown
    print("\nPer-run breakdown:")
    for t in all_trials:
        print(f"  {t['run']} {t['feature']:<12s} pt={t['pt']} ct={t['ct']} wc={t['wc']} {t['wt']:>5.0f}s ok={t['ok']} guard={t['guard_fired']}")

    with open("/tmp/bench_hard_results.json", "w") as f:
        json.dump(all_trials, f, indent=2)
    print(f"\nRaw: /tmp/bench_hard_results.json")
