"""Compression-only benchmark.

3 conditions:
1. baseline — direct API, no gate.js
2. gate-only — direct API, gate.js fast mode (inline JS reasoning compression)
3. proxy-compression-only — through :9090 proxy, no gate.js, only first-appearance freeze

3 runs × 3 conditions = 9 sessions, 10 turns each.
"""
import json, os, subprocess, sys, time, random, shutil

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "pi", "gate.js"))
REPO = "/tmp/bench_rename"
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from bench_lib import weighted_cost
RELIARY_BIN = (shutil.which("reliary-agent") or
               os.path.join(os.environ.get("HOME", ""), "src/reliary-agent/target/release/reliary-agent"))

SETTINGS_BAK = SETTINGS + ".cpbak"

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
    {"label": "baseline",    "needs_proxy": False, "needs_gate": False, "env": {}},
    {"label": "gate-only",   "needs_proxy": False, "needs_gate": True,
     "env": {"RELIARY_MODE": "fast"}},
    {"label": "proxy-comp",  "needs_proxy": True,  "needs_gate": False,
     "env": {"RELIARY_MODE": "strict"}},
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

def check_tests(repo):
    r = subprocess.run(["python3", "-m", "pytest", "tests/", "-v"],
                       capture_output=True, text=True, timeout=60, cwd=repo)
    passed = r.returncode == 0 and "FAILED" not in r.stdout
    return passed, r.stdout

def run_condition(cond, run_idx):
    sfile = f"/tmp/bench-cp-{cond['label']}-r{run_idx}.json"
    if os.path.exists(sfile): os.remove(sfile)

    reset_repo()
    route_pi_to_proxy(cond["needs_proxy"])
    set_ext(GATE if cond["needs_gate"] else None)

    env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1", "DEEPSEEK_API_KEY": API_KEY}
    env.update(cond["env"])

    total_pt = total_ct = total_tc = 0
    total_wt = 0.0
    turn_results = []

    for ti, prompt in enumerate(TURNS):
        if ti == 0:
            prompt = f"Working directory: {REPO}\nDo not add `cd` to bash commands — the working directory is already set.\n\n{prompt}"
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
        turn_results.append({"turn": ti + 1, "pt": pt, "ct": ct,
                             "tc": tc, "wt": round(wt, 1)})

    all_pass, test_out = check_tests(REPO)
    wc = weighted_cost(total_pt, total_ct)

    return {
        "feature": cond["label"], "run": run_idx,
        "pt": total_pt, "ct": total_ct, "tc": total_tc,
        "wc": wc, "wt": round(total_wt, 1),
        "turns": len(TURNS), "ok": all_pass,
        "per_turn": turn_results,
        "test_output": test_out[:200],
    }

if __name__ == "__main__":
    runs = 3

    import urllib.request
    try:
        r = urllib.request.urlopen("http://127.0.0.1:9090/health", timeout=3)
        assert r.status == 200
    except Exception:
        print("ERROR: Daemon not ready on :9090")
        sys.exit(1)

    save_configs()
    print(f"Compression bench: {runs} runs × {len(CONDITIONS)} conditions = {runs * len(CONDITIONS)} sessions")
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

    print("\n" + "=" * 110)
    print(f"  {'Condition':<14s} {'PT':>8s} {'CT':>8s} {'WC':>10s} {'WT':>7s} {'TC':>5s} {'Acc':>5s} {'Δ%':>7s}")
    print("-" * 110)

    b_trials = [t for t in all_trials if t["feature"] == "baseline"]
    bar_wc = sum(t["wc"] for t in b_trials) / len(b_trials) if b_trials else 1
    if b_trials:
        bwc = bar_wc
        print(f"  {'baseline':<12s}  {sum(t['pt'] for t in b_trials)//len(b_trials):<8d}  {sum(t['ct'] for t in b_trials)//len(b_trials):<8d}  {bwc:<10.0f}  {sum(t['wt'] for t in b_trials)/len(b_trials):<6.1f}s  {sum(t['tc'] for t in b_trials)//len(b_trials):<5d}  {sum(1 for t in b_trials if t['ok'])}/{len(b_trials):<2}  —")

    for cond in CONDITIONS:
        if cond["label"] == "baseline": continue
        t = [x for x in all_trials if x["feature"] == cond["label"]]
        if not t: continue
        awc = sum(x["wc"] for x in t) / len(t)
        delta = (awc - bar_wc) / bar_wc * 100
        okc = sum(1 for x in t if x["ok"])
        print(f"  {cond['label']:<12s}  {sum(x['pt'] for x in t)//len(t):<8d}  {sum(x['ct'] for x in t)//len(t):<8d}  {awc:<10.0f}  {sum(x['wt'] for x in t)/len(t):<6.1f}s  {sum(x['tc'] for x in t)//len(t):<5d}  {okc}/{len(t):<2}  {delta:>+6.1f}%")

    print(f"\nPer-run:")
    for t in all_trials:
        print(f"  r{t['run']} {t['feature']:<12s} pt={t['pt']} ct={t['ct']} wc={t['wc']} {t['wt']:>4.0f}s ok={'Y' if t['ok'] else 'N'}")

    with open("/tmp/bench_cp_results.json", "w") as f:
        json.dump(all_trials, f, indent=2)
    print(f"\nResults: /tmp/bench_cp_results.json")
