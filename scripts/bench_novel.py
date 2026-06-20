"""Novel compression mechanisms benchmark: on vs off.
2 conditions × 3 runs interleaved = 6 sessions, 10 turns each.
"""
import json, os, subprocess, sys, time, random, shutil

PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "pi", "gate.js"))

RENAME_TASK = """cross-file rename in the bench repo.
Rename `process_record` to `process_entry` across all Python files.
Use the edit tool for each file. After all files are updated, verify with `pytest test_process.py`."""

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

def route_pi_through_proxy(enable):
    if enable:
        with open(MODELS, "w") as f:
            json.dump({"providers": {
                "deepseek": {
                    "baseUrl": "http://127.0.0.1:9090/v1",
                    "apiKey": API_KEY
                }
            }}, f, indent=2)

def run_pi(task_name, task_text, env_overrides=None):
    env = os.environ.copy()
    env["DEEPSEEK_API_KEY"] = API_KEY
    env["PI_MAX_TOKENS"] = "32000"
    if env_overrides:
        env.update(env_overrides)
    prompt = f"{task_text}"
    try:
        res = subprocess.run(
            [PI, "--model", "deepseek-v4-flash", "--print", "--mode", "json"],
            input=prompt, capture_output=True, text=True, timeout=480, env=env
        )
    except subprocess.TimeoutExpired:
        return {"tokens": 0, "error": "timeout"}
    except FileNotFoundError:
        return {"tokens": 0, "error": "pi not found"}
    out = res.stdout.strip()
    if not out:
        return {"tokens": 0, "error": "empty stdout", "stderr": res.stderr[:500]}
    tokens = 0
    acc = False
    for line in out.split("\n"):
        try:
            ev = json.loads(line)
            if ev.get("type") == "message_end" or ev.get("type") == "done":
                usage = ev.get("usage", {})
                # Pi uses input/output fields (not prompt_tokens/completion_tokens)
                pt = usage.get("input", usage.get("prompt_tokens", 0)) or 0
                ct = usage.get("output", usage.get("completion_tokens", 0)) or 0
                tokens = pt + ct * 2  # 1:2 pricing
            if ev.get("type") == "agent_end":
                acc = True
        except: pass
    if tokens == 0:
        pass
    return {"tokens": tokens, "acc": acc, "stdout_len": len(out)}

def run_trial(condition_name, env_overrides, workdir):
    route_pi_through_proxy(True)
    os.chdir(workdir)
    result = run_pi(condition_name, RENAME_TASK, env_overrides)
    os.chdir("/tmp")
    # Check accuracy: pytest should pass
    acc = False
    try:
        r = subprocess.run(["pytest", "test_process.py"], capture_output=True, text=True, timeout=15, cwd=workdir)
        acc = "passed" in r.stdout or "0 failed" in r.stdout
    except: pass
    result["acc"] = acc
    return result

# Main benchmark
def main():
    random.seed(42)
    workdir = "/tmp/bench_rename"
    if not os.path.exists(workdir):
        print(f"ERROR: bench repo {workdir} not found. Run bench_rename.py first.")
        sys.exit(1)

    features = [
        ("existing", {}),
        ("novel-full", {"RELIARY_PROXY_NOVEL_COMPRESS": "0"}),
    ]
    results = {f[0]: [] for f in features}
    order = []
    for r in range(3):
        random.shuffle(features)
        for name, overrides in features:
            order.append((r + 1, name, overrides))

    print(f"Novel bench: {len(order)} sessions ({len(features)} conditions × 3 runs)")
    print(f"Repo: {workdir}  Turns: 10")
    print()

    for run_num, feat_name, overrides in order:
        subprocess.run(["git", "checkout", "."], capture_output=True, cwd=workdir)
        t0 = time.time()
        result = run_trial(feat_name, overrides, workdir)
        wt = time.time() - t0
        wc = result.get("tokens", 0)
        acc = result.get("acc", False)
        tag = "+OK" if acc else "FAIL"
        results[feat_name].append({"wc": wc, "wt": wt, "acc": acc})
        print(f"  [r{run_num}] {feat_name}: wc={wc} {tag} ({int(wt)}s)")

    # Summary
    print(f"\n{'Condition':<16} {'Avg WC':>10} {'vs baseline':>12} {'Avg WT':>8} {'Acc':>5}")
    print("-" * 55)
    b_name = features[0][0]
    b_trials = results.get(b_name, [])
    b_wc = sum(t["wc"] for t in b_trials) / max(len(b_trials), 1) or 1
    for name, _ in features:
        trials = results.get(name, [])
        if not trials:
            continue
        awc = sum(t["wc"] for t in trials) / len(trials)
        awt = sum(t["wt"] for t in trials) / len(trials)
        delta = (awc - b_wc) / b_wc * 100 if b_wc else 0
        acc_str = f"{sum(1 for t in trials if t.get('acc'))}/{len(trials)}"
        print(f"{name:<16} {awc:>10,.0f} {delta:>+11.1f}% {awt:>7.0f}s {acc_str:>5}")

if __name__ == "__main__":
    main()
