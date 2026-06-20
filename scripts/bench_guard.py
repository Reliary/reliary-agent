"""Guard-firing benchmark.

The trigger: ask LLM to add a shared error-logging function across 6 modules.
LLM invents function names (log_error, handle_error, etc.) not in the FTS5 index.
Guard catches each invented reference on the first edit.

3 conditions × 3 runs interleaved = 9 sessions, 7 turns each.
"""
import json, os, subprocess, sys, time, random, shutil

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from bench_lib import cwd_prefix, weighted_cost

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "pi", "gate.js"))
REPO = "/tmp/bench_guard"
RELIARY_BIN = (shutil.which("reliary-agent") or
               os.path.join(os.environ.get("HOME", ""), "src/reliary-agent/target/release/reliary-agent"))

SETTINGS_BAK = SETTINGS + ".grbak"

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
    {"label": "recommended", "needs_proxy": True,  "needs_gate": True,
     "env": {"RELIARY_MODE": "strict",
             "RELIARY_LOG": "debug",
             "RELIARY_FEATURES": "compress,convWindow,readEnrichment,healEdit"}},
]

TURNS = [
    "Read every file in src/ and explain what each module does and how data flows between them.",
    "Run 'python3 -m pytest tests/ -v' and report all results.",
    "Add a function called log_error(message, module_name) in src/utils.py that prints an error line with module name. Also import it and call log_error once from each of the 5 consumer modules (ingest.py, transform.py, filter.py, export.py, api.py) when an item fails processing or validation.",
    "For each consumer module, confirm the log_error call handles the actual failure case — when process_item or validate_schema fails, the module should call log_error with a descriptive message and its own module name, then continue to the next item. Edit each module file individually.",
    "Run 'python3 -m pytest tests/ -v'. If any tests fail, state which ones and why.",
    "Fix any remaining test failures by editing the affected files.",
    "Run 'python3 -m pytest tests/ -v' one final time to confirm all tests pass.",
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
    if os.path.exists(RELIARY_BIN):
        subprocess.run([RELIARY_BIN, "index", "."], capture_output=True, timeout=30, cwd=REPO)

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
    sfile = f"/tmp/bench-gr-{cond['label']}-r{run_idx}.json"
    if os.path.exists(sfile): os.remove(sfile)

    reset_repo()
    route_pi_to_proxy(cond["needs_proxy"])
    set_ext(GATE if cond["needs_gate"] else None)

    env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1", "DEEPSEEK_API_KEY": API_KEY}
    env.update(cond["env"])

    total_pt = total_ct = total_tc = 0
    total_wt = 0.0
    guard_fire_count = 0
    turn_results = []

    for ti, prompt in enumerate(TURNS):
        if ti == 0:
            prompt = cwd_prefix(REPO) + prompt
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

        # Check for guard signals
        if "[guard:" in r.stdout:
            guard_fire_count += 1

        # Check for hallucinated identifiers in the response
        invented_refs = 0
        for pat in ["log_error(", "handle_error(", "error_log(", "log_failure("]:
            if pat in r.stdout:
                invented_refs += 1

        turn_results.append({
            "turn": ti + 1, "pt": pt, "ct": ct, "tc": tc,
            "wt": round(wt, 1), "guard_hit": "[guard:" in r.stdout,
            "invented_refs": invented_refs,
        })

    all_pass, test_out = check_tests(REPO)
    wc = weighted_cost(total_pt, total_ct)

    return {
        "feature": cond["label"], "run": run_idx,
        "pt": total_pt, "ct": total_ct, "tc": total_tc,
        "wc": wc, "wt": round(total_wt, 1),
        "turns": len(TURNS), "ok": all_pass,
        "guard_fires": guard_fire_count,
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
    print(f"Guard bench: {runs} runs × {len(CONDITIONS)} conditions = {runs * len(CONDITIONS)} sessions")
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
                g = f" G={result['guard_fires']}" if result["guard_fires"] else ""
                print(f"pt={result['pt']} ct={result['ct']} wc={result['wc']} {result['wt']:>4.0f}s {ok}{g} ({el:.0f}s)")
                all_trials.append(result)
    finally:
        restore_configs()

    print("\n" + "=" * 110)
    hdr = f"  {'Condition':<14s} {'PT':>8s} {'CT':>8s} {'WC':>10s} {'WT':>7s} {'TC':>5s} {'Acc':>5s} {'Δ%':>7s}  Guard"
    print(hdr)
    print("-" * 110)

    b_trials = [t for t in all_trials if t["feature"] == "baseline"]
    bar_wc = sum(t["wc"] for t in b_trials) / len(b_trials) if b_trials else 1
    if b_trials:
        print(f"  {'baseline':<12s}  {sum(t['pt'] for t in b_trials)//len(b_trials):<8d}  {sum(t['ct'] for t in b_trials)//len(b_trials):<8d}  {bar_wc:<10.0f}  {sum(t['wt'] for t in b_trials)/len(b_trials):<6.1f}s  {sum(t['tc'] for t in b_trials)//len(b_trials):<5d}  {sum(1 for t in b_trials if t['ok'])}/{len(b_trials):<2}  —       {sum(t['guard_fires'] for t in b_trials)}/{len(b_trials)}")

    for cond in CONDITIONS:
        if cond["label"] == "baseline": continue
        t = [x for x in all_trials if x["feature"] == cond["label"]]
        if not t: continue
        awc = sum(x["wc"] for x in t) / len(t)
        delta = (awc - bar_wc) / bar_wc * 100
        okc = sum(1 for x in t if x["ok"])
        gf = sum(x["guard_fires"] for x in t)
        print(f"  {cond['label']:<12s}  {sum(x['pt'] for x in t)//len(t):<8d}  {sum(x['ct'] for x in t)//len(t):<8d}  {awc:<10.0f}  {sum(x['wt'] for x in t)/len(t):<6.1f}s  {sum(x['tc'] for x in t)//len(t):<5d}  {okc}/{len(t):<2}  {delta:>+6.1f}%  {gf}/{len(t)}")

    print(f"\nPer-run:")
    for t in all_trials:
        gf = f" guard={t['guard_fires']}" if t["guard_fires"] else ""
        print(f"  r{t['run']} {t['feature']:<12s} pt={t['pt']} ct={t['ct']} wc={t['wc']} {t['wt']:>4.0f}s ok={'Y' if t['ok'] else 'N'}{gf}")

    with open("/tmp/bench_guard_results.json", "w") as f:
        json.dump(all_trials, f, indent=2)
    print(f"\nResults: /tmp/bench_guard_results.json")
