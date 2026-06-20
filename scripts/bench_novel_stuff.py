"""Novel mechanisms benchmark: verbose reasoning + JSON-heavy config.
3 runs × 2 conditions (novel-on, novel-off) = 6 sessions, 12 turns each.
"""
import json, os, subprocess, sys, time, random, shutil

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "pi", "gate.js"))
REPO = "/tmp/bench_novel"
SETTINGS_BAK = SETTINGS + ".nvbak"

def read_api_key():
    try:
        with open(os.path.expanduser("~/.reliary/proxy-routes.json")) as f:
            routes = json.load(f)
        for v in routes.values():
            if isinstance(v, str) and v.startswith("sk-"):
                return v
    except: pass
    return os.environ.get("DEEPSEEK_API_KEY", "")

API_KEY = read_api_key()

CONDITIONS = [
    {"label": "novel-on",  "needs_proxy": True,
     "env": {"RELIARY_MODE": "strict",
             "RELIARY_FEATURES": "compress,convWindow,readEnrichment,healEdit"}},
    {"label": "novel-off", "needs_proxy": True,
     "env": {"RELIARY_MODE": "strict",
             "RELIARY_FEATURES": "compress,convWindow,readEnrichment,healEdit",
             "RELIARY_PROXY_NOVEL_COMPRESS": "0"}},
]

TASK = """Analyze and fix the data pipeline at /tmp/bench_novel.
The pipeline always returns empty results — no items are processed.
Read the config file and all source files to understand the bug.
Fix processor.py then verify with `python3 -m pytest tests/ -v`."""

WHIP = [
    "Read config/pipeline.json and all source files in src/ to understand the architecture. Describe the bug and your fix plan.",
    "Apply the fix, run `python3 -m pytest tests/ -v`, and report the results.",
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
        with open(MODELS, "w") as f:
            json.dump({"providers": {"deepseek": {
                "baseUrl": "http://127.0.0.1:9090/v1",
                "apiKey": API_KEY
            }}}, f, indent=2)

def reset_repo():
    subprocess.run(["git", "checkout", "-f", "master"], capture_output=True, cwd=REPO)
    subprocess.run(["git", "clean", "-fd"], capture_output=True, cwd=REPO)

def set_ext(ext_path):
    with open(SETTINGS, "w") as f:
        if ext_path:
            json.dump({"version": 1, "packages": [ext_path], "extensions": [ext_path]}, f, indent=2)
        else:
            json.dump({"version": 1, "packages": [], "extensions": []}, f, indent=2)

def parse_usage(stdout):
    pt = ct = 0
    for line in stdout.splitlines():
        if not line.startswith("{"): continue
        try:
            d = json.loads(line)
            if d.get("type") == "message_end":
                u = d.get("message", {}).get("usage", {})
                pt += u.get("input", 0)
                ct += u.get("output", 0)
        except: pass
    return pt, ct

def check_tests(repo):
    r = subprocess.run(["python3", "-m", "pytest", "tests/", "-v"],
                       capture_output=True, text=True, timeout=60, cwd=repo)
    return r.returncode == 0 and "FAILED" not in r.stdout

def run_condition(cond, run_idx):
    reset_repo()
    route_pi_to_proxy(cond["needs_proxy"])
    set_ext(GATE)

    env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1", "DEEPSEEK_API_KEY": API_KEY}
    env.update(cond["env"])

    total_pt = total_ct = 0
    total_wt = 0.0
    all_pass = True
    turn_results = []

    for turn_idx, turn_prompt in enumerate(WHIP):
        t0 = time.time()
        prompt = f"Turn {turn_idx + 1}/{len(WHIP)}: {turn_prompt}\n\n{TASK if turn_idx == 0 else ''}"
        try:
            res = subprocess.run(
                [PI, "--model", "deepseek-v4-flash", "--print", "--mode", "json"],
                input=prompt, capture_output=True, text=True,
                timeout=480, env=env
            )
        except subprocess.TimeoutExpired:
            turn_results.append({"ok": False, "pt": 0, "ct": 0, "wt": time.time() - t0})
            all_pass = False
            continue

        el = time.time() - t0
        pt, ct = parse_usage(res.stdout)
        total_pt += pt
        total_ct += ct
        total_wt += el
        turn_results.append({"ok": res.returncode == 0, "pt": pt, "ct": ct, "wt": el})

        if turn_idx == len(WHIP) - 1:
            acc = check_tests(REPO)
            if acc:
                all_pass = True

    wc = total_pt + 2 * total_ct  # DeepSeek 1:2 pricing
    return {
        "feature": cond["label"],
        "run": run_idx,
        "pt": total_pt, "ct": total_ct,
        "wc": wc, "wt": round(total_wt, 1),
        "ok": all_pass,
        "per_turn": turn_results,
    }

if __name__ == "__main__":
    try:
        r = subprocess.run(
            ["curl", "-s", "--max-time", "3", "http://127.0.0.1:9090/health"],
            capture_output=True, timeout=5)
        assert r.returncode == 0
    except Exception:
        print("ERROR: Daemon not ready on :9090")
        sys.exit(1)

    random.seed(42)
    save_configs()
    runs = 2
    print(f"Novel bench: {runs} runs × {len(CONDITIONS)} conditions = {runs * len(CONDITIONS)} sessions", flush=True)
    print(f"Repo: {REPO}  Turns: {len(WHIP)}", flush=True)
    print(flush=True)

    all_trials = []
    try:
        for ri in range(1, runs + 1):
            order = list(CONDITIONS)
            random.shuffle(order)
            for cond in order:
                t0 = time.time()
                result = run_condition(cond, ri)
                el = time.time() - t0
                ok = "+OK" if result["ok"] else "FAIL"
                print(f"  [r{ri}] {cond['label']}: pt={result['pt']} ct={result['ct']} wc={result['wc']} {result['wt']:>4.0f}s {ok} ({el:.0f}s)", flush=True)
                all_trials.append(result)
    finally:
        restore_configs()

    print()
    b_trials = [t["wc"] for t in all_trials if t["feature"] == "novel-on"]
    b_wc = sum(b_trials) / max(len(b_trials), 1)
    print(f"{'Condition':<16} {'Avg WC':>10} {'vs novel-on':>12} {'Avg WT':>8} {'Acc':>5}")
    print("-" * 55)
    for cond in CONDITIONS:
        trials = [t for t in all_trials if t["feature"] == cond["label"]]
        if not trials: continue
        awc = sum(t["wc"] for t in trials) / len(trials)
        awt = sum(t["wt"] for t in trials) / len(trials)
        delta = (awc - b_wc) / b_wc * 100
        acc_str = f"{sum(1 for t in trials if t['ok'])}/{len(trials)}"
        print(f"{cond['label']:<16} {awc:>10,.0f} {delta:>+11.1f}% {awt:>7.0f}s {acc_str:>5}")
