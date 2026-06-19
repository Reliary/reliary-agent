"""5-condition feature benchmark.

Measures the token and latency impact of each proxy feature individually.

Conditions:
1. baseline — direct API, no gate, no proxy
2. proxy-comp — proxy, gate.js strict, compression only (default)
3. proxy+guard — proxy, gate.js strict, +RELIARY_PROXY_FEATURE_GUARD=1
4. proxy+cooccur — proxy, gate.js strict, +RELIARY_PROXY_FEATURE_COOCCUR=1
5. proxy+guard+cooccur — proxy, gate.js strict, +both

5 runs × 5 conditions = 25 sessions interleaved, 10 turns each.
"""
import json, os, subprocess, sys, time, random, shutil, urllib.request

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "pi", "gate.js"))
REPO = "/tmp/bench_rename"
HOME = os.environ.get("HOME", "")
RELIARY_BIN = shutil.which("reliary-agent") or os.path.join(HOME, "src/reliary-agent/target/release/reliary-agent")

SETTINGS_BAK = SETTINGS + ".febak"

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
    {"label": "+cooccur",   "proxy": True,  "gate": True,
     "env": {"RELIARY_MODE": "strict",
             "RELIARY_PROXY_FEATURE_COOCCUR": "1"}},
    {"label": "+guard+coocc","proxy": True,  "gate": True,
     "env": {"RELIARY_MODE": "strict",
             "RELIARY_PROXY_FEATURE_GUARD": "1",
             "RELIARY_PROXY_FEATURE_COOCCUR": "1"}},
]

TURNS = [
    "Read every file in src/ and explain what each module does and how data flows between them.",
    "Run 'python3 -m pytest tests/ -v' and report all results.",
    "Rename function process_item to transform_item in src/utils.py, updating the function definition and all import statements across the 5 consumer modules.",
    "Rename function validate_schema to check_schema in src/utils.py and update all imports and call sites.",
    "Run 'python3 -m pytest tests/ -v'. If any tests fail, state which tests and why.",
    "Fix test failures by updating any remaining old references to process_item or validate_schema.",
    "Now rename process_item references again — this time to handle_item. Update utils.py definition and all 5 consumer module imports and call sites.",
    "Run 'python3 -m pytest tests/ -v'. Report any remaining failures.",
    "Fix any remaining test failures.",
    "Run 'python3 -m pytest tests/ -v' final time to confirm all tests pass.",
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
    subprocess.run(["rm", "-rf", "src/__pycache__", "tests/__pycache__", ".pytest_cache", ".reliary"],
                   capture_output=True, cwd=REPO)

def set_ext(ext_path):
    with open(SETTINGS, "w") as f:
        if ext_path:
            json.dump({"version": 1, "packages": [ext_path], "extensions": [ext_path]}, f, indent=2)
        else:
            json.dump({"version": 1, "packages": [], "extensions": []}, f, indent=2)

def parse_usage(stdout):
    pt = ct = tc = 0
    guard_fired = False
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
        # Check for guard signal
        if "[guard:" in line:
            guard_fired = True
    return pt, ct, tc, guard_fired

def check_tests(repo):
    r = subprocess.run(["python3", "-m", "pytest", "tests/", "-v"],
                       capture_output=True, text=True, timeout=60, cwd=repo)
    passed = r.returncode == 0 and "FAILED" not in r.stdout
    return passed, r.stdout

def run_condition(cond, run_idx):
    sfile = f"/tmp/bench-fe-{cond['label']}-r{run_idx}.json"
    if os.path.exists(sfile): os.remove(sfile)

    reset_repo()
    route_pi_to_proxy(cond["proxy"])
    set_ext(GATE if cond["gate"] else None)

    env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1", "DEEPSEEK_API_KEY": API_KEY}
    env.update(cond["env"])

    total_pt = total_ct = total_tc = 0
    total_wt = 0.0
    guard_events = 0
    turn_results = []

    for ti, prompt in enumerate(TURNS):
        t0 = time.time()
        r = subprocess.run(
            [PI, "--model", "deepseek/deepseek-v4-flash",
             "--mode", "json", "--session", sfile, "--print", prompt],
            capture_output=True, text=True, timeout=600, env=env, cwd=REPO)
        wt = time.time() - t0
        pt, ct, tc, gf = parse_usage(r.stdout)
        total_pt += pt
        total_ct += ct
        total_tc += tc
        total_wt += wt
        if gf: guard_events += 1
        turn_results.append({"turn": ti + 1, "pt": pt, "ct": ct,
                             "tc": tc, "wt": round(wt, 1)})

    all_pass, test_out = check_tests(REPO)
    wc = total_pt + 4 * total_ct
    stale_refs = 0  # check for stale references in test output
    if "NameError" in test_out or "ImportError" in test_out:
        stale_refs = 1

    return {
        "feature": cond["label"], "run": run_idx,
        "pt": total_pt, "ct": total_ct, "tc": total_tc,
        "wc": wc, "wt": round(total_wt, 1),
        "turns": len(TURNS), "ok": all_pass,
        "guard_events": guard_events,
        "stale_refs": stale_refs,
        "per_turn": turn_results,
        "test_output": test_out[:300],
    }

def print_summary(all_trials):
    conditions = [c["label"] for c in CONDITIONS]
    print("\n" + "=" * 130)
    hdr = f"  {'Condition':<14s} {'PT':>8s} {'CT':>8s} {'WC':>10s} {'WT':>7s} {'TC':>5s} {'Acc':>5s} {'Guard':>6s} {'Δ%':>7s}"
    print(hdr)
    print("-" * 130)

    b_trials = [t for t in all_trials if t["feature"] == "baseline"]
    bar_wc = sum(t["wc"] for t in b_trials) / len(b_trials) if b_trials else 1

    for cond_label in conditions:
        t = [x for x in all_trials if x["feature"] == cond_label]
        if not t: continue
        apt = sum(x["pt"] for x in t) // len(t)
        act = sum(x["ct"] for x in t) // len(t)
        awc = sum(x["wc"] for x in t) / len(t)
        awt = sum(x["wt"] for x in t) / len(t)
        atc = sum(x["tc"] for x in t) // len(t)
        okc = sum(1 for x in t if x["ok"])
        total_guard = sum(x["guard_events"] for x in t)
        delta = (awc - bar_wc) / bar_wc * 100 if bar_wc else 0
        dstr = f"{delta:>+6.1f}%" if cond_label != "baseline" else "     —"
        print(f"  {cond_label:<12s}  {apt:<8d}  {act:<8d}  {awc:<10.0f}  {awt:<6.1f}s  {atc:<5d}  {okc}/{len(t):<2}  {total_guard:<5d}  {dstr}")

    print(f"\nPer-run:")
    for t in all_trials:
        gf = f" guard={t['guard_events']}" if t['guard_events'] else ""
        sr = f" STALE" if t['stale_refs'] else ""
        print(f"  r{t['run']} {t['feature']:<12s} pt={t['pt']} ct={t['ct']} wc={t['wc']} {t['wt']:>4.0f}s ok={'Y' if t['ok'] else 'N'}{gf}{sr}")

if __name__ == "__main__":
    runs = 5

    try:
        r = urllib.request.urlopen("http://127.0.0.1:9090/health", timeout=3)
        assert r.status == 200
    except Exception:
        print("ERROR: Daemon not ready on :9090")
        sys.exit(1)

    save_configs()
    print(f"Feature bench: {runs} runs × {len(CONDITIONS)} conditions = {runs * len(CONDITIONS)} sessions")
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
                gf = f" G={result['guard_events']}" if result["guard_events"] else ""
                print(f"pt={result['pt']} ct={result['ct']} wc={result['wc']} {result['wt']:>4.0f}s {ok}{gf} ({el:.0f}s)")
                all_trials.append(result)
    finally:
        restore_configs()

    print_summary(all_trials)

    with open("/tmp/bench_fe_results.json", "w") as f:
        json.dump(all_trials, f, indent=2)
    print(f"\nResults: /tmp/bench_fe_results.json")
