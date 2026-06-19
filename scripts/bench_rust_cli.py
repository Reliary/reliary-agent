"""Long-session Rust CLI benchmark.

Repo has 3 bugs causing compile errors and test failures.
Task forces 15+ turns with large cargo build/test output, repeated reads, and verbose reasoning.

Conditions: baseline, proxy-comp, proxy-comp+guard
3 runs × 3 conditions = 9 sessions interleaved.
"""
import json, os, subprocess, sys, time, random, shutil, urllib.request

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "pi", "gate.js"))
REPO = "/tmp/bench_rust_cli"
HOME = os.environ.get("HOME", "")
RELIARY_BIN = shutil.which("reliary-agent") or os.path.join(HOME, "src/reliary-agent/target/release/reliary-agent")

SETTINGS_BAK = SETTINGS + ".rcbak"

def read_api_key():
    routes_path = os.path.expanduser("~/.reliary/proxy-routes.json")
    try:
        with open(routes_path) as f:
            routes = json.load(f)
        for key, url in routes.items():
            if "deepseek" in url and len(key) > 20 and not key.startswith("__"):
                return key
    except Exception:
        pass
    return os.environ.get("DEEPSEEK_API_KEY", "")

API_KEY = read_api_key()

CONDITIONS = [
    {"label": "baseline",   "proxy": False, "gate": False, "env": {}},
    {"label": "proxy-comp", "proxy": True,  "gate": True,
     "env": {"RELIARY_MODE": "strict"}},
    {"label": "+guard",     "proxy": True,  "gate": True,
     "env": {"RELIARY_MODE": "strict",
             "RELIARY_PROXY_FEATURE_GUARD": "1"}},
]

TURNS = [
    "Build the project: 'cargo build 2>&1'. Report all errors and warnings.",
    "Read src/main.rs. Explain what bugs you see.",
    "Read src/config.rs. Explain what bugs you see.",
    "Read src/error.rs. Explain what bugs you see.",
    "Read src/processor.rs. Explain what bugs you see.",
    "Fix all compile errors. Build the project and report results.",
    "If build fails, re-read any files needed and fix remaining errors.",
    "Run 'cargo test 2>&1'. Report all test failures.",
    "Read tests/integration.rs. Explain what tests expect and what's broken.",
    "Fix test failures. Verify with 'cargo test 2>&1'.",
    "If tests still fail, fix remaining issues and re-run tests.",
    "Run 'cargo clippy 2>&1'. Fix any warnings.",
    "Final verification: 'cargo build 2>&1 && cargo test 2>&1'.",
]

def save_configs():
    if os.path.exists(SETTINGS):
        shutil.copy2(SETTINGS, SETTINGS_BAK)

def restore_configs():
    if os.path.exists(MODELS):
        os.remove(MODELS)
    if os.path.exists(SETTINGS_BAK):
        shutil.move(SETTINGS_BAK, SETTINGS)

def route_pi_to_proxy(enable):
    if enable:
        cfg = {"providers": {"deepseek": {"apiKeyEnv": "DEEPSEEK_API_KEY",
                                          "baseUrl": "http://127.0.0.1:9090/v1"}}}
        with open(MODELS, "w") as f:
            json.dump(cfg, f, indent=2)
    else:
        if os.path.exists(MODELS):
            os.remove(MODELS)

def reset_repo():
    subprocess.run(["git", "checkout", "-f", "master"], capture_output=True, cwd=REPO)
    subprocess.run(["git", "clean", "-fd"], capture_output=True, cwd=REPO)

def set_ext(ext_path):
    with open(SETTINGS, "w") as f:
        if ext_path:
            json.dump({"version": 1, "packages": [ext_path], "extensions": [ext_path]}, f, indent=2)
        else:
            json.dump({"version": 1, "packages": [], "extensions": []}, f, indent=2)

def parse_usage(stdout, daemon_log=None):
    pt = ct = tc = 0
    guard_hits = []
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

def check_passed(repo):
    r = subprocess.run(
        ["cargo", "test", "2>&1"],
        capture_output=True, text=True, timeout=120, shell=True, cwd=repo
    )
    build_r = subprocess.run(
        ["cargo", "build", "2>&1"],
        capture_output=True, text=True, timeout=120, shell=True, cwd=repo
    )
    # All tests pass + no compile errors
    build_ok = build_r.returncode == 0
    test_ok = r.returncode == 0 and "FAILED" not in r.stdout and "error" not in r.stdout.lower()
    test_out = (r.stdout + r.stderr)[:500]
    return build_ok and test_ok, test_out, build_ok, test_ok

def score_warnings(test_out):
    """Count remaining stale refs/errors in test output."""
    score = 0
    for pat in ["no field", "not found", "doesn't implement", "expected", "FAILED", "error[E"]:
        if pat in test_out:
            score += 1
    return score

def run_condition(cond, run_idx):
    sfile = f"/tmp/bench-rc-{cond['label']}-r{run_idx}.json"
    if os.path.exists(sfile):
        os.remove(sfile)

    reset_repo()
    route_pi_to_proxy(cond["proxy"])
    set_ext(GATE if cond["gate"] else None)

    env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1", "DEEPSEEK_API_KEY": API_KEY}
    env.update(cond["env"])

    total_pt = total_ct = total_tc = 0
    total_wt = 0.0
    all_pt_log = []
    turn_results = []

    for ti, prompt in enumerate(TURNS):
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
        all_pt_log.append(("T" + str(ti + 1), pt, ct))
        turn_results.append({"turn": ti + 1, "pt": pt, "ct": ct, "wt": round(wt, 1), "tc": tc})

    all_pass, test_out, build_ok, test_ok = check_passed(REPO)
    stale = score_warnings(test_out)
    wc = total_pt + 4 * total_ct

    return {
        "feature": cond["label"], "run": run_idx,
        "pt": total_pt, "ct": total_ct, "tc": total_tc,
        "wc": wc, "wt": round(total_wt, 1),
        "turns": len(TURNS), "ok": all_pass,
        "build_ok": build_ok, "test_ok": test_ok,
        "stale_score": stale,
        "per_turn": turn_results,
        "pt_log": all_pt_log,
        "test_output": test_out[:400],
    }

def print_summary(all_trials):
    conditions = [c["label"] for c in CONDITIONS]
    print("\n" + "=" * 130)
    print(f"  {'Condition':<14s} {'PT':>8s} {'CT':>8s} {'WC':>10s} {'WT':>7s} {'TC':>5s} {'Acc':>5s} {'Δ%':>7s}")
    print("-" * 130)

    b_trials = [t for t in all_trials if t["feature"] == "baseline"]
    bar_wc = sum(t["wc"] for t in b_trials) / len(b_trials) if b_trials else 1

    for cond_label in conditions:
        t = [x for x in all_trials if x["feature"] == cond_label]
        if not t:
            continue
        apt = sum(x["pt"] for x in t) // len(t)
        act = sum(x["ct"] for x in t) // len(t)
        awc = sum(x["wc"] for x in t) / len(t)
        awt = sum(x["wt"] for x in t) / len(t)
        atc = sum(x["tc"] for x in t) // len(t)
        okc = sum(1 for x in t if x["ok"])
        delta = (awc - bar_wc) / bar_wc * 100 if bar_wc else 0
        dstr = f"{delta:>+6.1f}%" if cond_label != "baseline" else "     —"
        print(f"  {cond_label:<12s}  {apt:<8d}  {act:<8d}  {awc:<10.0f}  {awt:<6.1f}s  {atc:<5d}  {okc}/{len(t):<2}  {dstr}")

    print(f"\nPer-run:")
    for t in all_trials:
        sr = f" STALE={t['stale_score']}" if t['stale_score'] else ""
        bk = " B+" if t['build_ok'] else " B-"
        tk = "T+" if t['test_ok'] else "T-"
        print(f"  r{t['run']} {t['feature']:<12s} pt={t['pt']} ct={t['ct']} wc={t['wc']} {t['wt']:>4.0f}s {'OK' if t['ok'] else 'FAIL'}{bk}{tk}{sr}")

    print(f"\nPer-turn PT accumulation:")
    for t in all_trials:
        log_str = " ".join(f"{tag}={v}" for tag, v, _ in t['pt_log'])
        print(f"  r{t['run']} {t['feature']:<12s} | {log_str}")

if __name__ == "__main__":
    runs = 3

    try:
        r = urllib.request.urlopen("http://127.0.0.1:9090/health", timeout=3)
        assert r.status == 200
    except Exception:
        print("ERROR: Daemon not ready on :9090")
        sys.exit(1)

    save_configs()
    print(f"Rust CLI bench: {runs} runs × {len(CONDITIONS)} conditions = {runs * len(CONDITIONS)} sessions")
    print(f"Repo: {REPO}  Turns: {len(TURNS)}")
    print()

    all_trials = []
    try:
        for ri in range(1, runs + 1):
            order = list(CONDITIONS)
            random.shuffle(order)
            for cond in order:
                label = f"[r{ri}] {cond['label']}"
                print(f"  {label}: ", end="", flush=True)
                t0 = time.time()
                result = run_condition(cond, ri)
                el = time.time() - t0
                ok = "+OK" if result["ok"] else "FAIL"
                print(f"pt={result['pt']} ct={result['ct']} wc={result['wc']} {result['wt']:>4.0f}s {ok} ({el:.0f}s)")
                all_trials.append(result)
    finally:
        restore_configs()

    print_summary(all_trials)

    with open("/tmp/bench_rc_results.json", "w") as f:
        json.dump(all_trials, f, indent=2)
    print(f"\nResults: /tmp/bench_rc_results.json")
