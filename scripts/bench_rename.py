"""Cross-file rename benchmark: guard catches stale references.

The task: rename process_record → process_entry across 5 Python files.
LLMs typically miss 1-2 call sites. Guard catches the stale reference
before the edit reaches the filesystem, preventing a 4-turn debug spiral.

3 conditions × 3 runs interleaved = 9 sessions, 10 turns each.

Usage: python3 bench_rename.py
"""
import json, os, subprocess, sys, time, random, shutil

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
# Ensure models.json exists for proxy routing
if not os.path.exists(MODELS):
    with open(MODELS, "w") as f:
        json.dump({"providers": {}}, f, indent=2)
if not os.path.exists(SETTINGS):
    with open(SETTINGS, "w") as f:
        json.dump({"version": 1, "packages": [], "extensions": []}, f, indent=2)
# Don't create models.json if it doesn't exist — Pi uses built-in defaults.
# Only modify it via route_pi_to_proxy() when proxy-routing is needed.
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "pi", "gate.js"))
def read_api_key():
    """Read the actual DeepSeek API key from proxy-routes.json or env."""
    routes_path = os.path.expanduser("~/.reliary/proxy-routes.json")
    try:
        with open(routes_path) as f:
            routes = json.load(f)
        # Find the deepseek route — it has the real key
        for key, url in routes.items():
            if "deepseek" in url and len(key) > 20 and not key.startswith("__"):
                return key
    except Exception:
        pass
    return os.environ.get("DEEPSEEK_API_KEY", "")

API_KEY = read_api_key()

GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "pi", "gate.js"))
RELIARY_BIN = (os.path.join(os.path.dirname(__file__), "..", "target", "release", "reliary-agent") if
               os.path.exists(os.path.join(os.path.dirname(__file__), "..", "target", "release", "reliary-agent"))
               else shutil.which("reliary-agent") or
               os.path.join(os.environ.get("HOME", ""), "src/reliary-agent/target/release/reliary-agent"))
REPO = "/tmp/bench_rename"

SETTINGS_BAK = SETTINGS + ".rnbak"
MODELS_BAK = MODELS + ".rnbak"

CONDITIONS = [
    {"label": "baseline",    "needs_proxy": False, "needs_gate": False, "env": {}},
    {"label": "gate-only",   "needs_proxy": False, "needs_gate": True,
     "env": {"RELIARY_MODE": "fast"}},
    {"label": "recommended", "needs_proxy": True,  "needs_gate": True,
     "env": {"RELIARY_MODE": "strict",
             "RELIARY_LOG": "debug",
             "RELIARY_FEATURES": "compress,convWindow,readEnrichment,healEdit"}},
    {"label": "existing-cc", "needs_proxy": True,  "needs_gate": True,
     "env": {"RELIARY_MODE": "strict",
             "RELIARY_LOG": "debug",
             "RELIARY_FEATURES": "compress,convWindow,readEnrichment,healEdit",
             "RELIARY_PROXY_NOVEL_COMPRESS": "0"}},
]

TURNS = [
    "Read all files in src/ and explain what each module does, how they import from each other, and which functions are shared across modules.",
    "Run 'python3 -m pytest tests/ -v' and report every passing and failing test.",
    "Rename function 'process_record' to 'process_entry' in src/utils.py (the definition only — keep the function body identical, just rename the def and all internal references to the new name).",
    "List all files that import 'process_record' from src.utils. For each one, edit the file to import 'process_entry' instead. Do each file one at a time.",
    "Now for each file that calls 'process_record', rename the call site to 'process_entry'. Edit each file individually. Do not rename the test file yet.",
    "Run 'python3 -m pytest tests/ -v' — some tests will fail because test_all.py still imports 'process_record'. Edit test_all.py to import 'process_entry' and update all call sites.",
    "Run 'python3 -m pytest tests/ -v'. If any tests fail, fix the remaining issues.",
    "Now add a new function 'validate_input(records, config)' to src/utils.py that checks every record has the required keys from config before processing. Return True if all valid, False otherwise.",
    "Add 'validate_input' calls in ingest.py, transform.py, filter.py, export.py, and api.py before they call 'process_entry'. Also import it in each file. If a validation fails, skip that record.",
    "Run 'python3 -m pytest tests/ -v' one final time and report the full output.",
]

def restart_daemon():
    """Restart the daemon to flush response cache between conditions."""
    subprocess.run([RELIARY_BIN, "stop"], capture_output=True, timeout=10)
    time.sleep(1)
    r = subprocess.run([RELIARY_BIN, "start"], capture_output=True, timeout=30)
    assert r.returncode == 0, f"Daemon start failed: {r.stderr.decode()}"
    time.sleep(2)
    import urllib.request
    for _ in range(10):
        try:
            r = urllib.request.urlopen("http://127.0.0.1:9090/health", timeout=3)
            assert r.status == 200
            return
        except Exception:
            time.sleep(1)
    raise RuntimeError("Daemon not healthy after restart")

def save_configs():
    for src, dst in [(SETTINGS, SETTINGS_BAK)]:
        if os.path.exists(src):
            shutil.copy2(src, dst)

def restore_configs():
    # Delete models.json so Pi uses built-in defaults
    if os.path.exists(MODELS):
        os.remove(MODELS)
    for src, dst in [(SETTINGS_BAK, SETTINGS)]:
        if os.path.exists(src):
            shutil.move(src, dst)

def route_pi_to_proxy(enable):
    """Set Pi's deepseek provider baseUrl to proxy.
    Restores by deleting models.json so Pi uses built-in defaults."""
    if enable:
        cfg = {
            "providers": {
                "deepseek": {
                    "apiKeyEnv": "DEEPSEEK_API_KEY",
                    "baseUrl": "http://127.0.0.1:9090/v1",
                }
            }
        }
        with open(MODELS, "w") as f:
            json.dump(cfg, f, indent=2)
    else:
        # Delete models.json — Pi will fall back to built-in defaults
        if os.path.exists(MODELS):
            os.remove(MODELS)

def reset_repo():
    subprocess.run(["git", "checkout", "-f", "master"], capture_output=True, cwd=REPO)
    subprocess.run(["git", "clean", "-fd"], capture_output=True, cwd=REPO)
    subprocess.run(["rm", "-rf", "src/__pycache__", "tests/__pycache__", ".pytest_cache", ".reliary"],
                   capture_output=True, cwd=REPO)
    # Build FTS5 index so guard can check identifiers
    bi = os.environ.get("RELIARY_BIN", shutil.which("reliary-agent") or "")
    if bi:
        subprocess.run([bi, "index", "."], capture_output=True, timeout=30, cwd=REPO)

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

def count_stale_refs(repo):
    """Count files that still reference 'process_record' (meaning the rename was missed)."""
    r = subprocess.run(["rg", "-l", "process_record", "src/", "tests/"],
                       capture_output=True, text=True, cwd=repo)
    if r.returncode != 0 and r.stdout.strip():
        return len(r.stdout.strip().splitlines())
    return 0

def guard_logs():
    """Check daemon log for guard-related messages."""
    try:
        r = subprocess.run(["grep", "-c", "guard", "/tmp/reliary-daemon.log"],
                           capture_output=True, text=True)
        return int(r.stdout.strip() or 0)
    except Exception:
        return 0

def run_condition(cond, run_idx):
    sfile = f"/tmp/bench-rn-{cond['label']}-r{run_idx}.json"
    if os.path.exists(sfile): os.remove(sfile)

    reset_repo()
    route_pi_to_proxy(cond["needs_proxy"])
    set_ext(GATE if cond["needs_gate"] else None)

    env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1", "DEEPSEEK_API_KEY": API_KEY}
    env.update(cond["env"])

    total_pt = total_ct = total_tc = 0
    total_wt = 0.0
    turn_results = []
    guard_signals = []

    for ti, prompt in enumerate(TURNS):
        t0 = time.time()
        r = subprocess.run(
            [PI, "--model", "deepseek/deepseek-v4-flash",
             "--mode", "json", "--session", sfile, "--print", prompt],
            capture_output=True, text=True, timeout=1200, env=env, cwd=REPO)
        wt = time.time() - t0
        pt, ct, tc = parse_usage(r.stdout)
        total_pt += pt
        total_ct += ct
        total_tc += tc
        total_wt += wt
        # Check for guard signals in response
        if "[guard:" in r.stdout:
            guard_signals.append(True)
        else:
            guard_signals.append(False)
        turn_results.append({"turn": ti + 1, "pt": pt, "ct": ct,
                             "tc": tc, "wt": round(wt, 1)})

    all_pass, test_out = check_tests(REPO)
    stale_after = count_stale_refs(REPO)
    guard_fired = any(guard_signals)

    wc = total_pt + 2 * total_ct  # DeepSeek V4 Flash: 1:2 pricing
    return {
        "feature": cond["label"],
        "run": run_idx,
        "pt": total_pt, "ct": total_ct, "tc": total_tc,
        "wc": wc, "wt": round(total_wt, 1),
        "turns": len(TURNS), "ok": all_pass,
        "stale_refs": stale_after,
        "guard_fired": guard_fired,
        "test_output": test_out[:200],
        "per_turn": turn_results,
    }

if __name__ == "__main__":
    runs = 3

    restart_daemon()
    # Initial health check already covered by restart_daemon

    save_configs()
    print(f"Rename bench: {runs} runs × {len(CONDITIONS)} conditions = {runs * len(CONDITIONS)} sessions")
    print(f"Repo: {REPO}  Turns: {len(TURNS)}")
    print()

    all_trials = []
    try:
        for ri in range(1, runs + 1):
            order = list(CONDITIONS)
            random.shuffle(order)
            for cond in order:
                restart_daemon()  # flush response cache between conditions
                label = f"[r{ri}] {cond['label']}"
                print(f"  {label}: ", end="", flush=True)
                t0 = time.time()
                result = run_condition(cond, ri)
                el = time.time() - t0
                ok = "+OK" if result["ok"] else "FAIL"
                g = " G!" if result["guard_fired"] else ""
                s = f" stale={result['stale_refs']}" if result["stale_refs"] else ""
                print(f"pt={result['pt']} ct={result['ct']} wc={result['wc']} {result['wt']:>4.0f}s {ok}{g}{s} ({el:.0f}s)")
                all_trials.append(result)
    finally:
        restore_configs()

    print("\n" + "=" * 110)
    hdr = f"  {'Condition':<14s} {'PT':>8s} {'CT':>8s} {'WC':>10s} {'WT':>7s} {'TC':>5s} {'Acc':>5s} {'Δ%':>7s}  Ref  Guard"
    print(hdr)
    print("-" * 110)

    b_trials = [t for t in all_trials if t["feature"] == "baseline"]
    bar_wc = sum(t["wc"] for t in b_trials) / len(b_trials) if b_trials else 1
    if b_trials:
        print(f"  {'baseline':<12s}  {sum(t['pt'] for t in b_trials)//len(b_trials):<8d}  {sum(t['ct'] for t in b_trials)//len(b_trials):<8d}  {bar_wc:<10.0f}  {sum(t['wt'] for t in b_trials)/len(b_trials):<6.1f}s  {sum(t['tc'] for t in b_trials)//len(b_trials):<5d}  {sum(1 for t in b_trials if t['ok'])}/{len(b_trials):<2}  —       {sum(t['stale_refs'] for t in b_trials)}/{len(b_trials)}  {sum(1 for t in b_trials if t['guard_fired'])}/{len(b_trials)}")

    for cond in CONDITIONS:
        if cond["label"] == "baseline": continue
        t = [x for x in all_trials if x["feature"] == cond["label"]]
        if not t: continue
        awc = sum(x["wc"] for x in t) / len(t)
        delta = (awc - bar_wc) / bar_wc * 100
        okc = sum(1 for x in t if x["ok"])
        gc = sum(1 for x in t if x["guard_fired"])
        sr = sum(x["stale_refs"] for x in t)
        print(f"  {cond['label']:<12s}  {sum(x['pt'] for x in t)//len(t):<8d}  {sum(x['ct'] for x in t)//len(t):<8d}  {awc:<10.0f}  {sum(x['wt'] for x in t)/len(t):<6.1f}s  {sum(x['tc'] for x in t)//len(t):<5d}  {okc}/{len(t):<2}  {delta:>+6.1f}%  {sr}/{len(t)}  {gc}/{len(t)}")

    print(f"\nPer-run:")
    for t in all_trials:
        g = " guard" if t["guard_fired"] else ""
        s = f" stale={t['stale_refs']}" if t["stale_refs"] else ""
        print(f"  r{t['run']} {t['feature']:<12s} pt={t['pt']} ct={t['ct']} wc={t['wc']} {t['wt']:>4.0f}s ok={'Y' if t['ok'] else 'N'}{g}{s}")

    with open("/tmp/bench_rn_results.json", "w") as f:
        json.dump(all_trials, f, indent=2)
    print(f"\nResults: /tmp/bench_rn_results.json")
