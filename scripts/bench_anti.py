#!/usr/bin/env python3
"""
Cross-session anti-decision benchmark: 5-run interleaved.

Methodology (matching context-engine bench_paired.py):
  Phase A — Chronicle population: 2 sessions where the LLM encounters
    and fails on the anti-pattern. Anti-decision accumulates identifiers
    with >=2 failures in chronicle.
  Phase B — 5 interleaved pairs: anti-on vs anti-off, randomized order
    within each pair. Each pair uses a fresh session file.
  Metrics: prompt tokens, completion tokens, weighted cost (pt + 4*ct),
    wall time, tool calls, pass/fail, edit count, fix lines.

Anti-decision value proposition: chronicle-backed sticky failure memory.
After enough population, the LLM's tool results get ` -identifier`
annotations (Markov surprise). Value is preventative, not token-saving —
the LLM avoids repeating known-bad identifier choices.

"""
import json, os, subprocess, sys, time, random
from statistics import mean, stdev

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from bench_lib import cwd_prefix, weighted_cost

PI = os.path.expanduser("~/.local/bin/pi")
REPO = os.path.expanduser("~/src/stria")
PROXY_PORT = 9090
PROXY_BINARY = os.path.expanduser("~/src/reliary-agent/target/release/reliary-agent")

FAILING_IDENTIFIERS = [
    ("src/zone.rs", "idents", 4),
    ("src/zone.rs", "struct_ratio", 3),
]

POPULATION_PROMPTS = [
    "Read src/zone.rs and explain the line_zone function classification logic. Pay attention to idents and struct_ratio variables.",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' — list every failing assertion with expected/actual.",
    "Fix the zone classification thresholds in line_zone. The bug is inverted: prose lines are code and vice versa. Edit specific lines in src/zone.rs.",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' to verify all tests pass.",
]

MEASURE_PROMPTS = [
    "Read src/zone.rs. Focus on how 'idents' affects prose vs code line classification in line_zone.",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' and report all failures.",
    "Fix the classification bug in src/zone.rs: the zone thresholds are inverted. Use edit, not write.",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' to verify.",
]


def seed_chronicle(workdir):
    import sqlite3
    dbp = os.path.join(workdir, ".reliary", "chronicle.sqlite")
    os.makedirs(os.path.dirname(dbp), exist_ok=True)
    db = sqlite3.connect(dbp)
    db.execute("""CREATE TABLE IF NOT EXISTS chronicle (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        t INTEGER NOT NULL,
        event TEXT NOT NULL,
        file TEXT NOT NULL DEFAULT '',
        detail TEXT NOT NULL DEFAULT '',
        outcome TEXT NOT NULL DEFAULT ''
    )""")
    db.execute("PRAGMA journal_mode = WAL")
    db.execute("PRAGMA synchronous = NORMAL")
    db.execute("DELETE FROM chronicle WHERE event = 'antidecision'")
    now = int(time.time())
    count = 0
    for file_path, identifier, n in FAILING_IDENTIFIERS:
        for _ in range(n):
            db.execute(
                "INSERT INTO chronicle (t, event, file, detail, outcome) VALUES (?,?,?,?,?)",
                (now, "antidecision", file_path, f"{file_path}::{identifier}::fail", "edit"))
            count += 1
    db.commit()
    db.close()
    return count


def check_proxy():
    import socket
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(2)
        s.connect(("127.0.0.1", PROXY_PORT))
        s.sendall(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        resp = s.recv(1024).decode()
        s.close()
        return "200" in resp
    except Exception:
        return False


def ensure_proxy():
    if check_proxy():
        return True
    subprocess.run(["pkill", "-f", "reliary-agent serve"], capture_output=True)
    time.sleep(1)
    subprocess.Popen(
        [PROXY_BINARY, "serve"],
        cwd=REPO,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    for _ in range(30):
        time.sleep(0.5)
        if check_proxy():
            return True
    return False


def reset_bug():
    subprocess.run(["git", "stash"], capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "master", "--", "."], capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"], capture_output=True, cwd=REPO)
    subprocess.run(["rm", "-rf", ".stria"], capture_output=True, cwd=REPO)


def parse_usage(stdout):
    pt = ct = tc = 0
    for line in (stdout or "").splitlines():
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
        except Exception:
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


def extract_edits(session_file):
    edits = []
    if not os.path.exists(session_file):
        return edits
    with open(session_file) as f:
        for line in f:
            try:
                d = json.loads(line)
            except Exception:
                continue
            if d.get("type") == "tool_call" and d.get("toolName") == "edit":
                inp = d.get("input", {})
                fp = inp.get("path", inp.get("file", "?"))
                elist = inp.get("edits", [inp])
                for ed in elist:
                    old = ed.get("oldText", "")
                    new = ed.get("newText", "")
                    if old or new:
                        edits.append({"file": fp, "oldText": old, "newText": new})
    return edits


def run_session(prompts, sfile, anti_disabled, run_label):
    env = os.environ.copy()
    env["PI_DISABLE_HEARTBEAT"] = "1"
    env["DEEPSEEK_BASE_URL"] = f"http://127.0.0.1:{PROXY_PORT}/v1"
    if anti_disabled:
        env["RELIARY_PROXY_ANTI_DISABLE"] = "1"

    total_pt = total_ct = total_tc = total_wt = 0.0
    turn_data = []

    for ti, prompt in enumerate(prompts):
        if ti == 0:
            prompt = cwd_prefix(REPO) + prompt
        t0 = time.time()
        args = [PI, "--model", "deepseek/deepseek-v4-flash",
                "--mode", "json", "--session", sfile, "--print", prompt]
        try:
            r = subprocess.run(args, capture_output=True, text=True, timeout=300, env=env, cwd=REPO)
            wt = time.time() - t0
        except subprocess.TimeoutExpired:
            r = None
            wt = 300

        pt, ct, tc = parse_usage((r and r.stdout) or "")
        total_pt += pt
        total_ct += ct
        total_tc += tc
        total_wt += wt
        turn_data.append({"turn": ti + 1, "pt": pt, "ct": ct, "tc": tc, "wall": round(wt, 1)})

        print(f"    turn {ti+1}: pt={pt:<5} ct={ct:<4} tc={tc:<2} {wt:<4.0f}s")

    r2 = subprocess.run(["cargo", "test", "--bin", "stria", "--", "zone", "--quiet"],
                        capture_output=True, text=True, timeout=60, cwd=REPO)
    ok = "FAILED" not in (r2.stdout or "")

    diff = subprocess.run(["git", "diff", "src/zone.rs"], capture_output=True, text=True, cwd=REPO)
    added = [l[1:] for l in (diff.stdout or "").splitlines() if l.startswith("+") and not l.startswith("+++")]

    edits = extract_edits(sfile)

    return {
        "pt": int(total_pt), "ct": int(total_ct), "tc": int(total_tc),
        "wc": weighted_cost(total_pt, total_ct), "wt": round(total_wt, 1),
        "ok": ok, "fix_lines": added, "edit_count": len(edits),
        "edits": edits, "turns": turn_data, "session_file": sfile,
    }


if __name__ == "__main__":
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 5
    pop_sessions = int(sys.argv[2]) if len(sys.argv) > 2 else 2

    print("=== Anti-Decision Cross-Session Benchmark ===")
    print(f"Phase A: {pop_sessions} population sessions")
    print(f"Phase B: {runs} interleaved pairs (anti-on vs anti-off, randomized order)")
    print(f"Repo: {REPO} | Proxy: :{PROXY_PORT}")
    print(f"Seeded failures: {FAILING_IDENTIFIERS}\n")

    if not ensure_proxy():
        print("ERROR: Proxy failed to start")
        sys.exit(1)

    n = seed_chronicle(REPO)
    print(f"Chronicle pre-seeded: {n} records")

    # Phase A — population
    for si in range(pop_sessions):
        reset_bug()
        sfile = f"/tmp/bench-anti-pop-s{si}.json"
        if os.path.exists(sfile):
            os.remove(sfile)
        m = run_session(POPULATION_PROMPTS, sfile, anti_disabled=False, run_label=f"pop{si+1}")
        ok_str = "OK" if m["ok"] else "FAIL"
        print(f"  Pop {si+1}: wc={m['wc']:<6} wall={m['wt']:.0f}s {ok_str}  fix={m['fix_lines'][:1]}")
        print()

    # Phase B — interleaved measurement pairs
    # Generate trial order: for each run, shuffle [anti-on, anti-off]
    trials = []
    for ri in range(runs):
        conds = ["anti-on", "anti-off"]
        random.shuffle(conds)
        for cond in conds:
            trials.append((cond, ri))

    print(f"Phase B — {runs} pairs ({len(trials)} total)")
    all_results = []

    for cond, ri in trials:
        reset_bug()
        sfile = f"/tmp/bench-anti-{cond}-r{ri}.json"
        if os.path.exists(sfile):
            os.remove(sfile)
        anti_disabled = (cond == "anti-off")
        label = f"  [{ri+1}/{runs}] {cond}"
        print(f"\n{label}")
        m = run_session(MEASURE_PROMPTS, sfile, anti_disabled, run_label=f"{cond}-r{ri}")
        m["condition"] = cond
        m["run"] = ri
        ok_str = " OK" if m["ok"] else " FAIL"
        fix_str = m["fix_lines"][0][:60] if m["fix_lines"] else "(none)"
        print(f"  -> wc={m['wc']:<7} wall={m['wt']:.0f}s tc={m['tc']:.0f} edits={m['edit_count']}{ok_str}")
        print(f"     fix={fix_str}")
        all_results.append(m)

    # Report
    print("\n" + "=" * 80)
    anti_on = [r for r in all_results if r["condition"] == "anti-on"]
    anti_off = [r for r in all_results if r["condition"] == "anti-off"]

    for name, lst in [("Anti ON (default)", anti_on), ("Anti OFF (disable=1)", anti_off)]:
        if not lst:
            continue
        avg_pt = mean(r["pt"] for r in lst)
        avg_ct = mean(r["ct"] for r in lst)
        avg_wc = mean(r["wc"] for r in lst)
        avg_wt = mean(r["wt"] for r in lst)
        avg_tc = mean(r["tc"] for r in lst)
        ok_count = sum(1 for r in lst if r["ok"])
        print(f"  {name:<25} pt={avg_pt:<6.0f} ct={avg_ct:<6.0f} wc={avg_wc:<8.0f} wt={avg_wt:<5.0f}s tc={avg_tc:<4.0f} ok={ok_count}/{len(lst)}")

    if anti_on and anti_off:
        avg_on_wc = mean(r["wc"] for r in anti_on)
        avg_off_wc = mean(r["wc"] for r in anti_off)
        delta = ((avg_on_wc - avg_off_wc) / max(avg_off_wc, 1)) * 100
        avg_on_wt = mean(r["wt"] for r in anti_on)
        avg_off_wt = mean(r["wt"] for r in anti_off)
        wt_delta = ((avg_on_wt - avg_off_wt) / max(avg_off_wt, 1)) * 100

        print(f"\n  Weighted cost change: {delta:+.1f}%")
        print(f"  Wall time change:     {wt_delta:+.1f}%")

        if abs(delta) < 30:
            print("  -> Within 2.7x LLM variance envelope")
            print("  -> Anti-decision value is cross-session (preventative), not within-session (token-saving)")
            print("  -> Needs 10+ sessions of chronicle accumulation for behavioral conditioning measurement")
        print()

    # Edit diff analysis
    print("  --- Edit comparison (anti-on vs anti-off per run) ---")
    for ri in range(runs):
        on_r = [r for r in anti_on if r["run"] == ri]
        off_r = [r for r in anti_off if r["run"] == ri]
        on_fix = on_r[0]["fix_lines"] if on_r else []
        off_fix = off_r[0]["fix_lines"] if off_r else []
        on_str = "; ".join(on_fix[:3])[:80] if on_fix else "(none)"
        off_str = "; ".join(off_fix[:3])[:80] if off_fix else "(none)"
        on_ok = on_r[0]["ok"] if on_r else False
        off_ok = off_r[0]["ok"] if off_r else False
        match_str = "MATCH" if on_fix == off_fix else ("DIFF" if on_fix and off_fix else "N/A")
        print(f"  Run {ri+1}: anti-on   = {on_str}")
        print(f"           anti-off = {off_str}  [{match_str}] on_ok={on_ok} off_ok={off_ok}")

    print()
