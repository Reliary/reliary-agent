"""Multi-turn bug-fix benchmark: 6 isolated bugs, interleaved baseline vs gate."""
import json, os, subprocess, sys, time, re, shutil

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from bench_lib import cwd_prefix

PI = os.path.expanduser("~/.local/bin/pi")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "gate.js"))
CORTEX_BIN = os.path.expanduser("~/.local/bin/cortex")
REPO = "/tmp/bench_multibug"
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")

# 6 independent bug fixes — each prompt tells the LLM WHAT to fix but not HOW
P = [
    "Fix rate_limiter.py: allow() always returns True even when _tokens is 0. It should return False when _tokens <= 0.",
    "Fix sort_utils.py: merge() has an infinite loop with <= instead of < in the while condition.",
    "Fix config_reader.py: read_config() doesn't handle empty values — keys with empty strings are skipped.",
    "Fix cache.py: LRUCache put() silently fails to remove from _order when key is re-added but not in order.",
    "Fix validator.py: validate_phone() returns True for non-digit input, validate_age() returns True for None.",
    "Fix formatter.py: indent_code() depth decrements on '}' even when it's not at the start of a line.",
]

def set_extension(condition, gate_path):
    try:
        with open(SETTINGS) as f:
            s = json.load(f)
    except:
        s = {"version": 1, "packages": [], "extensions": []}
    if condition in ("gate-only", "gate"):
        s["extensions"] = [gate_path]
        s["packages"] = [gate_path]
    else:
        s["extensions"] = []
        s["packages"] = []
    s.pop("custom_tools", None)
    s.pop("tool_config_overrides", None)
    with open(SETTINGS, "w") as f:
        json.dump(s, f, indent=2)

def parse(stdout):
    pt = ct = tc = 0
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
            if "toolName" in d.get("message", {}):
                tc += 1
        elif d.get("type") == "tool_execution_start":
            tc += 1
    return pt, ct, tc

def verify():
    """Check all 6 files for correct fixes."""
    fixes = {
        "rate_limiter.py": [
            ("allow should return False when tokens <= 0",
             lambda c: "return False" in c and "return True" in c),
        ],
        "sort_utils.py": [
            ("merge while condition should be < not <=",
             lambda c: "while i < len(left) and j < len(right)" in c),
        ],
        "config_reader.py": [
            ("should handle empty values after =",
             lambda c: "key.strip()" in c and "val.strip()" in c),
        ],
        "cache.py": [
            ("put should handle key-not-in-order gracefully",
             lambda c: "self._order.remove(key)" in c or "in self._order" in c),
        ],
        "validator.py": [
            ("validate_phone should reject non-digit",
             lambda c: "return False" in c and "return False" in c),
        ],
        "formatter.py": [
            ("indent_code should only decrement depth on line-start }",
             lambda c: "depth" in c),
        ],
    }
    issues = []
    for fname, checks in fixes.items():
        try:
            content = open(os.path.join(REPO, fname)).read()
        except:
            issues.append(f"can't read {fname}")
            continue
        for label, check_fn in checks:
            if not check_fn(content):
                issues.append(f"{fname}: {label}")
    return issues

def run_turns(condition, run_idx, cortex_db=None):
    set_extension(condition, GATE)
    sfile = f"/tmp/bench-mt-{condition}-r{run_idx}.json"
    if os.path.exists(sfile):
        os.remove(sfile)
    # Fresh worktree per condition (roll back all bug fixes)
    subprocess.run(["git", "checkout", "."], cwd=REPO, capture_output=True)

    env = os.environ.copy()
    env["PI_DISABLE_HEARTBEAT"] = "1"
    if cortex_db:
        env["CORTEX_DB"] = cortex_db
    if condition == "gate-only":
        env["GATE_DISABLE_CORTEX"] = "1"
        env.pop("CORTEX_DB", None)

    total_pt = total_ct = total_tc = 0
    total_wall = 0.0
    turn_data = []

    for ti, prompt in enumerate(P):
        if ti == 0:
            prompt = cwd_prefix(REPO) + prompt
        args = [PI, "--model", "deepseek/deepseek-v4-flash",
                "--mode", "json", "--session", sfile, "--print", prompt]
        t0 = time.time()
        try:
            r = subprocess.run(args, capture_output=True, text=True, timeout=300, env=env, cwd=REPO)
            wt = time.time() - t0
        except subprocess.TimeoutExpired:
            r = None
            wt = 120
        pt, ct, tc = parse((r and r.stdout) or "")
        total_pt += pt
        total_ct += ct
        total_tc += tc
        total_wall += wt

        gate_log = [l.strip() for l in ((r and r.stderr) or "").splitlines() if "[gate]" in l]
        # Show all gate log lines (not just last) to see cortex fix-dir
        gs_parts = []
        for gl in gate_log:
            if "cortex" in gl or "fix-dir" in gl or "augment" in gl or "sift" in gl or "turn" in gl:
                gs_parts.append(gl[:80])
        gs = (" " + " | ".join(gs_parts[:3])) if gs_parts else ""
        print(f"  turn {ti+1}: pt={pt:<5} ct={ct:<4} tc={tc:<2} {wt:<5.0f}s{gs}")

        turn_data.append({"turn": ti + 1, "file": prompt.split(":")[0].split("/")[-1], "pt": pt, "ct": ct, "tc": tc, "wall": round(wt, 1)})

    return {"condition": condition, "pt": total_pt, "ct": total_ct,
            "tc": total_tc, "wall": round(total_wall, 1), "turns": turn_data}


# ── Main ──
if __name__ == "__main__":
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 3
    cortex_db = os.environ.get("CORTEX_DB", "/tmp/bench_multibug_cortex.db")

    print(f"Multi-turn bug benchmark: {runs} interleaved runs, 6 turns each")
    print(f"Repo: {REPO}")
    print()

    # Kill leftover daemons, start fresh
    subprocess.run(["pkill", "-9", "-f", "cortex serve"], capture_output=True)
    time.sleep(0.3)
    if os.path.exists(cortex_db):
        os.remove(cortex_db)

    all_results = []

    # Pre-seed cortex with fix memories (only used by gate condition)
    SEEDS = [
        "fix: rate_limiter.py allow() should return False when _tokens <= 0. Change 'return True' → 'return False' at the end of allow().",
        "fix: sort_utils.py merge() has infinite loop. Change 'while i <= len(left) or j < len(right)' → 'while i < len(left) and j < len(right)'",
        "fix: config_reader.py read_config() skips empty values. The 'split' after '=' needs to handle trailing empty strings.",
        "fix: cache.py LRUCache put() reverses order. Use 'in self._order' check before remove, or use collections.OrderedDict pattern.",
        "fix: validator.py validate_phone() returns True for invalid input. Change 'return True' → 'return False' in the error branch.",
        "fix: formatter.py indent_code() decrements depth on '}' even mid-line. Only decrement when line starts with '}'.",
    ]
    seed_env = {**os.environ, "CORTEX_DB": cortex_db}
    for seed in SEEDS:
        subprocess.run([CORTEX_BIN, "retain", seed], env=seed_env, capture_output=True)
    print(f"Pre-seeded {len(SEEDS)} fix memories in cortex DB")

    for ri in range(runs):
        for cond in ["baseline", "gate-only", "gate"]:
            subprocess.run(["git", "checkout", "."], cwd=REPO, capture_output=True)
            print(f"[{ri+1}/{runs}] {cond}...")
            m = run_turns(cond, ri, cortex_db)
            issues = verify()
            m["ok"] = len(issues) == 0
            m["issues"] = issues
            v = " +OK" if m["ok"] else f" FAIL ({issues[0][:60]})"
            print(f"  → pt={m['pt']} ct={m['ct']} tc={m['tc']} {m['wall']}s{v}")
            print()
            all_results.append(m)

        # Reset between runs
        subprocess.run(["git", "checkout", "."], cwd=REPO, capture_output=True)

    # Report
    print("=" * 70)
    print("  Metric                              Baseline     Gate-Only    Gate+Cortex  Change(B→G)  Change(GO→GC)")
    print("  " + "-" * 66)
    from statistics import mean

    def avg2(lst, key):
        return mean(r[key] for r in lst) if lst else 0

    base = [r for r in all_results if r["condition"] == "baseline"]
    go   = [r for r in all_results if r["condition"] == "gate-only"]
    gc   = [r for r in all_results if r["condition"] == "gate"]

    b_pt = avg2(base, "pt"); go_pt = avg2(go, "pt"); gc_pt = avg2(gc, "pt")
    b_ct = avg2(base, "ct"); go_ct = avg2(go, "ct"); gc_ct = avg2(gc, "ct")
    b_wc = avg2(base, "pt")+4*avg2(base, "ct")
    go_wc = avg2(go, "pt")+4*avg2(go, "ct")
    gc_wc = avg2(gc, "pt")+4*avg2(gc, "ct")
    b_tc = avg2(base, "tc"); go_tc = avg2(go, "tc"); gc_tc = avg2(gc, "tc")
    b_wl = avg2(base, "wall"); go_wl = avg2(go, "wall"); gc_wl = avg2(gc, "wall")

    def cell(name, va, vb, vc):
        d1 = f"{(vb - va) / max(abs(va), 1) * 100:+.1f}%"
        d2 = f"{(vc - vb) / max(abs(vb), 1) * 100:+.1f}%" if vb else "N/A"
        return f"  {name:<35} {va:<14,.0f} {vb:<14,.0f} {vc:<14,.0f} {d1:<14} {d2}"

    print(cell("Prompt tokens", b_pt, go_pt, gc_pt))
    print(cell("Completion tokens", b_ct, go_ct, gc_ct))
    print(cell("Weighted cost (in+4xout)", b_wc, go_wc, gc_wc))
    print(cell("Tool calls", b_tc, go_tc, gc_tc))
    print(cell("Wall clock (s)", b_wl, go_wl, gc_wl))

    b_ok = sum(1 for r in base if r["ok"])
    go_ok = sum(1 for r in go if r["ok"])
    gc_ok = sum(1 for r in gc if r["ok"])
    print(f"\n  Accuracy: baseline {b_ok}/{len(base)}  gate-only {go_ok}/{len(go)}  gate+cortex {gc_ok}/{len(gc)}")
    for r in all_results:
        if not r["ok"]:
            print(f"    {r['condition']} issue: {r['issues'][0][:100]}")
    print()

    # Save results
    out = "$HOME/src/context-engine/scripts/bench_multibug_results.json"
    with open(out, "w") as f:
        json.dump(all_results, f, indent=2)
    print(f"Results saved to {out}")
