"""DOA bench: 3 conditions × 5 runs interleaved.

Conditions:
  baseline    — no gate.js, direct API
  gate-only   — gate.js REALT_MODE=fast, direct API
  recommended — gate.js REALT_MODE=strict + proxy:
                PROXY_ANTI_DISABLE=1
                FEATURES=compress,convWindow,readEnrichment,healEdit

Usage: python3 bench_doa.py
"""
import json, os, subprocess, sys, time, random, shutil

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "pi", "gate.js"))
REPO = os.path.expanduser("~/src/stria")

SETTINGS_BAK = SETTINGS + ".doabak"
MODELS_BAK = MODELS + ".doabak"

CONDITIONS = [
    {
        "label": "baseline",
        "needs_proxy": False,
        "needs_gate": False,
        "env": {},
    },
    {
        "label": "gate-only",
        "needs_proxy": False,
        "needs_gate": True,
        "env": {"RELIARY_MODE": "fast"},
    },
    {
        "label": "recommended",
        "needs_proxy": True,
        "needs_gate": True,
        "env": {
            "RELIARY_MODE": "strict",
            "RELIARY_FEATURES": "compress,convWindow,readEnrichment,healEdit",
        },
    },
]

def save_configs():
    for src, dst in [(SETTINGS, SETTINGS_BAK), (MODELS, MODELS_BAK)]:
        if os.path.exists(src):
            shutil.copy2(src, dst)

def restore_configs():
    for src, dst in [(SETTINGS_BAK, SETTINGS), (MODELS_BAK, MODELS)]:
        if os.path.exists(src):
            if os.path.exists(dst):
                shutil.move(dst, src)

def route_pi_through_proxy(enable):
    with open(MODELS) as f:
        cfg = json.load(f)
    for pname, pdata in cfg.get("providers", {}).items():
        if enable and "deep" in pname.lower():
            pdata["baseUrl"] = "http://127.0.0.1:9090/v1"
        elif not enable and "deep" in pname.lower():
            pdata["baseUrl"] = "https://api.deepseek.com/v1"
    with open(MODELS, "w") as f:
        json.dump(cfg, f, indent=2)

def reset_repo():
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"],
        capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "master", "--", "."],
        capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"],
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
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
            if d.get("type") == "message_end":
                u = d.get("message", {}).get("usage", {})
                pt += u.get("input", 0)
                ct += u.get("output", 0)
                if "toolName" in d.get("message", {}):
                    tc += 1
            elif d.get("type") == "tool_execution_start":
                tc += 1
        except Exception:
            pass
    return pt, ct, tc

def run_condition(cond, run_idx):
    sfile = f"/tmp/bench-doa-{cond['label']}-r{run_idx}.json"
    if os.path.exists(sfile):
        os.remove(sfile)
    reset_repo()

    route_pi_through_proxy(cond["needs_proxy"])
    set_ext(GATE if cond["needs_gate"] else None)

    env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1"}
    env.update(cond["env"])

    prompts = [
        "Read src/zone.rs. Understand the line_zone function — how does it classify prose vs code lines?",
        "Run 'cargo test --bin stria -- zone --quiet 2>&1' and list all failures.",
        "Fix line_zone so all zone tests pass. The bug is in the prose classification thresholds.",
        "Run 'cargo test --bin stria -- zone --quiet 2>&1' to verify all tests pass.",
    ]

    total_pt = total_ct = total_tc = total_wt = 0.0
    for task in prompts:
        t0 = time.time()
        r = subprocess.run(
            [PI, "--model", "deepseek/deepseek-v4-flash",
             "--mode", "json", "--session", sfile, "--print", task],
            capture_output=True, text=True, timeout=600, env=env, cwd=REPO)
        wt = time.time() - t0
        pt, ct, tc = parse_usage(r.stdout)
        total_pt += pt
        total_ct += ct
        total_tc += tc
        total_wt += wt

    r2 = subprocess.run(["cargo", "test", "--bin", "stria", "--", "zone", "--quiet"],
        capture_output=True, text=True, timeout=60, cwd=REPO)
    ok = "FAILED" not in r2.stdout

    diff = subprocess.run(["git", "diff", "src/zone.rs"],
        capture_output=True, text=True, cwd=REPO)
    added_lines = [l[1:] for l in diff.stdout.splitlines()
                   if l.startswith("+") and not l.startswith("+++")]

    wc = total_pt + 4 * total_ct
    return {
        "feature": cond["label"],
        "run": run_idx,
        "pt": int(total_pt),
        "ct": int(total_ct),
        "tc": int(total_tc),
        "wc": int(wc),
        "wt": round(total_wt, 1),
        "ok": ok,
        "fix_lines": added_lines[:3],
    }

if __name__ == "__main__":
    runs = 5

    # Verify daemon is up for proxy-routed conditions
    import urllib.request
    try:
        r = urllib.request.urlopen("http://127.0.0.1:9090/health", timeout=3)
        assert r.status == 200
    except Exception as e:
        print(f"ERROR: Daemon not reachable on :9090 — start it with RELIARY_UPSTREAM_URL")
        sys.exit(1)

    save_configs()

    print(f"DOA bench: {runs} runs × {len(CONDITIONS)} conditions (interleaved)")
    print(f"Repo: {REPO}")
    print()

    trials = []
    try:
        for ri in range(1, runs + 1):
            order = list(CONDITIONS)
            random.shuffle(order)
            for cond in order:
                print(f"  [r{ri}] {cond['label']}...", end=" ", flush=True)
                r = run_condition(cond, ri)
                trials.append(r)
                ok_mark = "+OK" if r["ok"] else "FAIL"
                print(f"pt={r['pt']} ct={r['ct']} tc={r['tc']} {r['wt']}s wc={r['wc']} {ok_mark}")
    finally:
        restore_configs()

    baseline_trials = [t for t in trials if t["feature"] == "baseline"]
    bar_wc = sum(t["wc"] for t in baseline_trials) / len(baseline_trials) if baseline_trials else 1

    print("\n" + "=" * 80)
    print(f"{'Condition':<16s}  {'PT':>8s}  {'CT':>8s}  {'WC':>10s}  {'WT':>7s}  {'TC':>5s}  {'Acc':>5s}  {'Δ%':>7s}  Fix")
    print("-" * 80)
    for cond in CONDITIONS:
        ct = [t for t in trials if t["feature"] == cond["label"]]
        if not ct:
            continue
        avg_pt = sum(t["pt"] for t in ct) / runs
        avg_ct = sum(t["ct"] for t in ct) / runs
        avg_wc = sum(t["wc"] for t in ct) / runs
        avg_wt = sum(t["wt"] for t in ct) / runs
        avg_tc = sum(t["tc"] for t in ct) / runs
        ok_cnt = sum(1 for t in ct if t["ok"])
        delta = (avg_wc - bar_wc) / bar_wc * 100
        fix = ct[0].get("fix_lines", [])
        fix_str = fix[0][:50] if fix else "(none)"
        print(f"  {cond['label']:<14s}  {avg_pt:<8.0f}  {avg_ct:<8.0f}  {avg_wc:<10.0f}  {avg_wt:<6.1f}s  {avg_tc:<5.0f}  {ok_cnt}/{runs:<2}  {delta:>+6.1f}%  {fix_str}")

    with open("/tmp/bench_doa_results.json", "w") as f:
        json.dump(trials, f, indent=2)
    print(f"\nRaw results: /tmp/bench_doa_results.json")
