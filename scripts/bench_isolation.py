"""Isolation benchmark: tests each daemon feature individually via proxy.

All non-baseline conditions route Pi through :9090 proxy so proxy-level
features (who_calls, edit_cache, cooccur) actually fire.

Usage: python3 bench_isolation.py [runs=2]
Requires: daemon running on :9090 with RELIARY_UPSTREAM_URL set
"""
import json, os, subprocess, sys, time, random, shutil

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "pi", "gate.js"))
REPO = os.path.expanduser("~/src/stria")

# Backups for restore after bench
SETTINGS_BAK = SETTINGS + ".bak"
MODELS_BAK = MODELS + ".bak"

FEATURES = {
    "baseline":    {},                                            # no gate, direct API
    "gate-only":   {"RELIARY_MODE": "fast"},                      # gate.js only, no proxy
    "all-on":      {"RELIARY_MODE": "strict"},                    # gate + all proxy features
    "no-who-calls": {"RELIARY_MODE": "strict",
                     "RELIARY_PROXY_DISABLE_WHO_CALLS": "1"},     # all except who_calls
    "no-edit-cache": {"RELIARY_MODE": "strict",
                      "RELIARY_PROXY_DISABLE_EDIT_CACHE": "1"},   # all except edit_cache
    "no-cooccur":    {"RELIARY_MODE": "strict",
                      "RELIARY_PROXY_DISABLE_COOCCUR": "1"},      # all except cooccur
}

def save_configs():
    for src, dst in [(SETTINGS, SETTINGS_BAK), (MODELS, MODELS_BAK)]:
        if os.path.exists(src):
            shutil.copy2(src, dst)

def restore_configs():
    for src, dst in [(SETTINGS_BAK, SETTINGS), (MODELS_BAK, MODELS)]:
        if os.path.exists(src):
            shutil.move(src, dst)

def route_pi_through_proxy(enable):
    """Set Pi's DeepSeek baseUrl to proxy or back to direct."""
    with open(MODELS) as f:
        cfg = json.load(f)
    for pname, pdata in cfg.get("providers", {}).items():
        # Match both "deepseek" and "deepinfra" provider names
        if enable and "deep" in pname.lower():
            cfg["providers"][pname]["baseUrl"] = "http://127.0.0.1:9090/v1"
        elif not enable and "deep" in pname.lower():
            cfg["providers"][pname]["baseUrl"] = "https://api.deepseek.com/v1"
    with open(MODELS, "w") as f:
        json.dump(cfg, f, indent=2)

def reset_repo():
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

def run_condition(feature_name, run_idx, env_overrides):
    sfile = f"/tmp/bench-iso-{feature_name}-r{run_idx}.json"
    if os.path.exists(sfile): os.remove(sfile)
    reset_repo()

    # Route Pi through proxy for all non-baseline conditions
    route_pi_through_proxy(feature_name != "baseline")

    if feature_name == "baseline":
        set_ext(None)
    else:
        set_ext(GATE)

    env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1"}
    env.update(env_overrides)

    prompts = [
        "Read src/zone.rs. Understand the line_zone function — how does it classify prose vs code lines?",
        "Run 'cargo test --bin stria -- zone --quiet 2>&1' and list all failures.",
        "Fix line_zone so all zone tests pass. The bug is in the prose classification thresholds.",
        "Run 'cargo test --bin stria -- zone --quiet 2>&1' to verify all tests pass.",
    ]

    total_pt = total_ct = total_tc = total_wt = 0.0
    for ti, task in enumerate(prompts):
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

    diff = subprocess.run(["git", "diff", "src/zone.rs"], capture_output=True, text=True, cwd=REPO)
    added_lines = [l[1:] for l in diff.stdout.splitlines() if l.startswith("+") and not l.startswith("+++")]

    wc = total_pt + 4 * total_ct
    return {
        "feature": feature_name, "run": run_idx,
        "pt": int(total_pt), "ct": int(total_ct), "tc": int(total_tc),
        "wc": int(wc), "wt": round(total_wt, 1),
        "ok": ok, "fix_lines": added_lines,
    }

if __name__ == "__main__":
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 2
    feature_order = list(FEATURES.keys())

    # Verify daemon is up
    import urllib.request
    try:
        r = urllib.request.urlopen("http://127.0.0.1:9090/health", timeout=3)
        assert r.status == 200
    except Exception as e:
        print(f"ERROR: Daemon not reachable on :9090 — start it first with RELIARY_UPSTREAM_URL set")
        sys.exit(1)

    save_configs()

    print(f"Isolation benchmark: {runs} runs × {len(feature_order)} features (proxy-routed)")
    print(f"Repo: {REPO}")
    print()

    trials = []
    try:
        for ri in range(runs):
            shuffled = list(feature_order)
            random.shuffle(shuffled)
            for feat in shuffled:
                # Feature "baseline" or "gate-only" — disable all proxy features
                ov = dict(FEATURES[feat])
                if feat not in ("baseline", "gate-only"):
                    # Enable all features, then selectively disable one
                    pass  # all-on already correct
                print(f"  [r{ri+1}] {feat}...", end=" ", flush=True)
                r = run_condition(feat, ri, ov)
                trials.append(r)
                ok_mark = "+OK" if r["ok"] else "FAIL"
                print(f"pt={r['pt']} ct={r['ct']} tc={r['tc']} {r['wt']}s wc={r['wc']} {ok_mark}")
    finally:
        restore_configs()

    # Aggregate
    baselines = [t for t in trials if t["feature"] == "baseline"]
    baseline_wc = sum(t["wc"] for t in baselines) / len(baselines) if baselines else 1

    print("\n" + "=" * 80)
    for feat in feature_order:
        feat_trials = [t for t in trials if t["feature"] == feat]
        if not feat_trials: continue
        avg_pt = sum(t["pt"] for t in feat_trials) / len(feat_trials)
        avg_ct = sum(t["ct"] for t in feat_trials) / len(feat_trials)
        avg_wc = sum(t["wc"] for t in feat_trials) / len(feat_trials)
        avg_wt = sum(t["wt"] for t in feat_trials) / len(feat_trials)
        avg_tc = sum(t["tc"] for t in feat_trials) / len(feat_trials)
        ok_count = sum(1 for t in feat_trials if t["ok"])
        delta = ((avg_wc - baseline_wc) / baseline_wc * 100) if baseline_wc else 0
        fix = feat_trials[0].get("fix_lines", [])
        fix_str = fix[:2][0] if fix else "(none)"
        print(f"  {feat:16s}  pt={avg_pt:<7.0f} ct={avg_ct:<7.0f} wc={avg_wc:<8.0f} wt={avg_wt:<6.1f}s tc={avg_tc:<4.0f} acc={ok_count}/{len(feat_trials)} delta={delta:+.1f}%  fix={fix_str}")

    print()
    print("Results saved to /tmp/bench_isolation_results.json")
    with open("/tmp/bench_isolation_results.json", "w") as f:
        json.dump(trials, f, indent=2)
