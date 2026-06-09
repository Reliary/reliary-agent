"""Multi-turn stria benchmark: 1 bug, 4 turns, 3 conditions interleaved."""
import json, os, subprocess, sys, time, re, shutil

PI = os.path.expanduser("~/.local/bin/pi")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "gate.js"))
CORTEX_BIN = os.path.expanduser("~/.local/bin/cortex")
REPO = os.path.expanduser("~/src/stria")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
CORTEX_DB = "/tmp/bench_v2_cortex.db"

P = [
    "Read src/zone.rs. Understand the line_zone function — how does it classify prose vs code lines?",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' and list all failures.",
    "Fix line_zone so all zone tests pass. The bug is in the prose classification thresholds.",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' to verify all tests pass.",
]

def set_extension(condition, gate_path):
    with open(SETTINGS, "w") as f:
        if condition in ("gate-only", "gate"):
            json.dump({"version": 1, "packages": [gate_path], "extensions": [gate_path]}, f, indent=2)
        else:
            json.dump({"version": 1, "packages": [], "extensions": []}, f, indent=2)

def reset_repo():
    """Restore buggy state."""
    subprocess.run(["git", "stash"], capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"],
        capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "master", "--", "."],
        capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"],
        capture_output=True, cwd=REPO)
    subprocess.run(["rm", "-rf", ".stria"], capture_output=True, cwd=REPO)
    r = subprocess.run(["cargo", "test", "--bin", "stria", "--", "zone", "--quiet"],
        capture_output=True, text=True, timeout=60, cwd=REPO)
    return "FAILED" in r.stdout or "FAILED" in r.stderr

def parse_turn(stdout):
    pt = ct = tc = cr = cw = 0
    for line in stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
        except:
            continue
        if d.get("type") == "message_end":
            u = d.get("message", {}).get("usage", {})
            pt += u.get("input", 0)
            ct += u.get("output", 0)
            cr += u.get("input_tokens_read", 0) or 0
            cw += u.get("input_tokens_written", 0) or 0
            if "toolName" in d.get("message", {}):
                tc += 1
        elif d.get("type") == "tool_execution_start":
            tc += 1
    return pt, ct, tc, cr, cw

def run_condition(condition, run_idx):
    set_extension(condition, GATE)
    sfile = f"/tmp/bench-v2-{condition}-r{run_idx}.json"
    if os.path.exists(sfile):
        os.remove(sfile)

    env = os.environ.copy()
    env["PI_DISABLE_HEARTBEAT"] = "1"
    env["CORTEX_DB"] = CORTEX_DB
    if condition == "gate-only":
        env["GATE_DISABLE_CORTEX"] = "1"
        env.pop("CORTEX_DB", None)

    total_pt = total_ct = total_tc = total_wall = 0.0
    turns = []

    for ti, prompt in enumerate(P):
        args = [PI, "--model", "deepseek/deepseek-v4-flash",
                "--mode", "json", "--session", sfile, "--print", prompt]
        t0 = time.time()
        try:
            r = subprocess.run(args, capture_output=True, text=True, timeout=300, env=env, cwd=REPO)
            wt = time.time() - t0
        except subprocess.TimeoutExpired:
            r = None
            wt = 300

        pt, ct, tc, cr, cw = parse_turn((r and r.stdout) or "")
        total_pt += pt
        total_ct += ct
        total_tc += tc
        total_wall += wt

        gate_log = [l.strip() for l in ((r and r.stderr) or "").splitlines() if "[gate]" in l]
        gs = " | ".join(g[:60] for g in gate_log[-2:]) if gate_log else ""
        print(f"  turn {ti+1}: pt={pt:<5} ct={ct:<4} tc={tc:<2} {wt:<4.0f}s{gs}")

        turns.append({"turn": ti + 1, "pt": pt, "ct": ct, "tc": tc, "wall": round(wt, 1)})

    return {"condition": condition, "pt": total_pt, "ct": total_ct,
            "tc": total_tc, "wall": round(total_wall, 1), "turns": turns}

if __name__ == "__main__":
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 1

    print(f"Multi-turn stria benchmark: {runs} run(s), 4 turns each, 3 conditions")
    print(f"Repo: {REPO}")
    print()

    if os.path.exists(CORTEX_DB):
        os.remove(CORTEX_DB)
    seeds = [
        "'if idents < 2 {' → 'if idents == 0 {'  fix: change threshold from <2 to ==0 so single-identifier lines remain code",
    ]
    for s in seeds:
        subprocess.run([CORTEX_BIN, "retain", s], capture_output=True,
            env={**os.environ, "CORTEX_DB": CORTEX_DB})
    print(f"Pre-seeded {len(seeds)} cortex memories")
    print()

    all_results = []

    for ri in range(runs):
        for cond in ["baseline", "gate-only", "gate"]:
            bug_present = reset_repo()
            print(f"[{ri+1}/{runs}] {cond}... (bug: {bug_present})")
            m = run_condition(cond, ri)
            r = subprocess.run(["cargo", "test", "--bin", "stria", "--", "zone", "--quiet"],
                capture_output=True, text=True, timeout=60, cwd=REPO)
            m["ok"] = "FAILED" not in r.stdout
            v = " +OK" if m["ok"] else " FAIL"
            print(f"  → pt={m['pt']:.0f} ct={m['ct']:.0f} tc={m['tc']:.0f} {m['wall']:.0f}s{v}")
            print()
            all_results.append(m)

    # Report
    print("=" * 80)
    print(f"  {'Turn':<5} {'Baseline':>20} {'Gate-Only':>20} {'Gate+Cortex':>20}")
    print("  " + "-" * 76)
    base = [r for r in all_results if r["condition"] == "baseline"]
    go = [r for r in all_results if r["condition"] == "gate-only"]
    gc = [r for r in all_results if r["condition"] == "gate"]

    for ti in range(4):
        b = base[0]["turns"][ti]
        g1 = go[0]["turns"][ti] if go else {}
        g2 = gc[0]["turns"][ti] if gc else {}
        print(f"  {ti+1:<5} {b['pt']:>5}/{b['ct']:<5}({b['pt']+b['ct']:>5}) {g1.get('pt',0):>5}/{g1.get('ct',0):<5}({g1.get('pt',0)+g1.get('ct',0):>5}) {g2.get('pt',0):>5}/{g2.get('ct',0):<5}({g2.get('pt',0)+g2.get('ct',0):>5})")

    print("  " + "-" * 76)
    for name, lst in [("Total", base), ("Gate-Only", go), ("Gate+Cortex", gc)]:
        if not lst:
            continue
        r = lst[0]
        wc = r["pt"] + 4 * r["ct"]
        print(f"  {name:<10} pt={r['pt']:<5.0f} ct={r['ct']:<5.0f} wc={wc:<7.0f} wl={r['wall']:<4.0f}s tc={r['tc']:<3.0f} {'OK' if r['ok'] else 'FAIL'}")

    out = "$HOME/src/context-engine/scripts/bench_v2_results.json"
    with open(out, "w") as f:
        json.dump(all_results, f, indent=2)
    print(f"\nSaved to {out}")
