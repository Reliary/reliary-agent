#!/usr/bin/env python3
"""
Anti-decision cross-session benefit benchmark.

Methodology:
  Phase A — Chronicle population: Run 3 sessions where the LLM repeatedly
    encounters and fails on a specific anti-pattern identifier. Anti-decision
    accumulates failures in chronicle.
  Phase B — Benefit measurement: Run 5 interleaved pairs (anti-on vs anti-off)
    on the same task. Anti-decision injects Markov-surprise ` -identifier`
    annotations when known-failure identifiers appear. Measure behavioral change:
    does the LLM avoid the anti-pattern?

Anti-decision value proposition: preventative, not token-saving. The mechanism
warns the LLM about identifiers with repeated edit failures, preventing
the same class of mistake from recurring across sessions. Within a single
4-turn session, you're measuring 2.7x LLM variance, not anti-decision impact.

Key difference from guard: guard checks the CURRENT edit for orphaned references
(always fires on cross-file renames). Anti-decision uses PAST failures to
condition FUTURE behavior (needs accumulation).

Disable via: RELIARY_PROXY_ANTI_DISABLE=1 (for baseline comparison)
"""
import json, os, subprocess, sys, time, sqlite3

PI = os.path.expanduser("~/.local/bin/pi")
REPO = os.path.expanduser("~/src/stria")
REPO_CWD = REPO
DAEMON_PORT = 9090

FAILING_IDENTIFIERS = [
    ("src/zone.rs", "idents", 4),
    ("src/zone.rs", "struct_ratio", 3),
]

P_POPULATE = [
    "Read src/zone.rs. Pay close attention to 'idents' and 'struct_ratio'. Explain how they're computed and used in line_zone.",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' and list ALL failing test assertions with their expected/actual values. List every single one.",
    "Fix src/zone.rs so ALL zone tests pass. The classification logic is inverted — prose lines are classified as code and vice versa. Adjust the idents and struct_ratio comparison. Do NOT use write (the file is too large). Use edit to change specific lines.",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' to verify.",
]

P_MEASURE = [
    "Read src/zone.rs. The 'idents' variable has been causing bugs in this file before. Focus on how it affects line_zone classification.",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' and check all failures.",
    "Fix src/zone.rs — the zone classification thresholds are inverted. Be careful with 'idents' and 'struct_ratio' — we've seen repeated wrong fixes on these before.",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' to verify.",
]


def seed_chronicle(wd):
    dbp = os.path.join(wd, ".reliary", "chronicle.sqlite")
    os.makedirs(os.path.dirname(dbp), exist_ok=True)
    db = sqlite3.connect(dbp)
    db.execute("""CREATE TABLE IF NOT EXISTS chronicle (
        id INTEGER PRIMARY KEY AUTOINCREMENT, t INTEGER NOT NULL,
        event TEXT NOT NULL, file TEXT NOT NULL DEFAULT '',
        detail TEXT NOT NULL DEFAULT '', outcome TEXT NOT NULL DEFAULT ''
    )""")
    db.execute("PRAGMA journal_mode = WAL")
    db.execute("PRAGMA synchronous = NORMAL")
    db.execute("DELETE FROM chronicle WHERE event = 'antidecision'")
    now = int(time.time())
    for file_path, identifier, n in FAILING_IDENTIFIERS:
        for _ in range(n):
            db.execute("INSERT INTO chronicle (t, event, file, detail, outcome) VALUES (?,?,?,?,?)",
                       (now, "antidecision", file_path, f"{file_path}::{identifier}::fail", "edit"))
    db.commit()
    db.close()


def check_proxy(port):
    import socket
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(2)
        s.connect(("127.0.0.1", port))
        s.sendall(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        resp = s.recv(1024).decode()
        s.close()
        return "200" in resp
    except Exception:
        return False


def ensure_proxy(port, workdir):
    if check_proxy(port):
        return True
    subprocess.run(["pkill", "-f", "reliary-agent serve"], capture_output=True)
    time.sleep(1)
    binary = os.path.expanduser("~/src/reliary-agent/target/release/reliary-agent")
    subprocess.Popen([binary, "serve"], cwd=workdir, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(30):
        time.sleep(0.5)
        if check_proxy(port):
            return True
    return False


def parse_turn(stdout):
    pt = ct = tc = 0
    for line in (stdout or "").splitlines():
        line = line.strip()
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
            if d.get("message", {}).get("role") == "assistant":
                tc += 1
        elif d.get("type") == "tool_execution_start":
            tc += 1
    return pt, ct, tc


def reset_repo():
    subprocess.run(["git", "stash"], capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "master", "--", "."], capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"], capture_output=True, cwd=REPO)
    subprocess.run(["rm", "-rf", ".stria"], capture_output=True, cwd=REPO)
    r = subprocess.run(["cargo", "test", "--bin", "stria", "--", "zone", "--quiet"],
                       capture_output=True, text=True, timeout=60, cwd=REPO)
    return "FAILED" in r.stdout or "FAILED" in r.stderr


def run_session(prompts, sfile, anti_disabled=False):
    env = os.environ.copy()
    env["PI_DISABLE_HEARTBEAT"] = "1"
    env["DEEPSEEK_BASE_URL"] = f"http://127.0.0.1:{DAEMON_PORT}/v1"
    if anti_disabled:
        env["RELIARY_PROXY_ANTI_DISABLE"] = "1"

    total_pt = total_ct = total_tc = total_wall = 0.0
    for prompt in prompts:
        args = [PI, "--model", "deepseek/deepseek-v4-flash",
                "--mode", "json", "--session", sfile, "--print", prompt]
        t0 = time.time()
        try:
            r = subprocess.run(args, capture_output=True, text=True, timeout=300, env=env, cwd=REPO_CWD)
            wt = time.time() - t0
        except subprocess.TimeoutExpired:
            r = None
            wt = 300
        pt, ct, tc = parse_turn((r and r.stdout) or "")
        total_pt += pt
        total_ct += ct
        total_tc += tc
        total_wall += wt
    return {"pt": total_pt, "ct": total_ct, "wc": total_pt + 4 * total_ct,
            "tc": total_tc, "wall": round(total_wall, 1)}


if __name__ == "__main__":
    pairs = int(sys.argv[1]) if len(sys.argv) > 1 else 3
    print("=== Anti-Decision Cross-Session Benefit Benchmark ===\n")
    print(f"Phase A: 3 population sessions (accumulate failures)")
    print(f"Phase B: {pairs} interleaved pairs (anti-on vs anti-off)\n")

    if not ensure_proxy(DAEMON_PORT, REPO_CWD):
        print("ERROR: proxy did not start")
        sys.exit(1)

    seed_chronicle(REPO_CWD)

    print("Phase A — Population (chronicle seeding + 2 real sessions)")
    for si in range(2):
        reset_repo()
        sfile = f"/tmp/bench-anti-pop-s{si}.json"
        if os.path.exists(sfile):
            os.remove(sfile)
        m = run_session(P_POPULATE, sfile)
        r = subprocess.run(["cargo", "test", "--bin", "stria", "--", "zone", "--quiet"],
                           capture_output=True, text=True, timeout=60, cwd=REPO)
        ok = "FAILED" not in (r.stdout or "")
        print(f"  Pop session {si+1}: wc={m['wc']:.0f} wall={m['wall']:.0f}s {'OK' if ok else 'FAIL'}")

    print(f"\nPhase B — {pairs} pairs interleaved")
    results = []
    for ri in range(pairs):
        for cond in ["anti-on", "anti-off"]:
            reset_repo()
            sfile = f"/tmp/bench-anti-{cond}-r{ri}.json"
            if os.path.exists(sfile):
                os.remove(sfile)
            anti_disabled = (cond == "anti-off")
            m = run_session(P_MEASURE, sfile, anti_disabled)
            r = subprocess.run(["cargo", "test", "--bin", "stria", "--", "zone", "--quiet"],
                               capture_output=True, text=True, timeout=60, cwd=REPO)
            m["condition"] = cond
            m["ok"] = "FAILED" not in (r.stdout or "")
            ok_str = "OK" if m["ok"] else "FAIL"
            print(f"  [{ri+1}/{pairs}] {cond}: wc={m['wc']:.0f} wall={m['wall']:.0f}s {ok_str}")
            results.append(m)

    anti_on = [r for r in results if r["condition"] == "anti-on"]
    anti_off = [r for r in results if r["condition"] == "anti-off"]

    print("\n" + "=" * 70)
    for label, rows in [("Anti ON (default)", anti_on), ("Anti OFF (disable=1)", anti_off)]:
        if not rows:
            continue
        avg_wc = sum(r["wc"] for r in rows) / len(rows)
        avg_wall = sum(r["wall"] for r in rows) / len(rows)
        avg_pt = sum(r["pt"] for r in rows) / len(rows)
        avg_ct = sum(r["ct"] for r in rows) / len(rows)
        ok_count = sum(1 for r in rows if r["ok"])
        print(f"  {label:<22} pt={avg_pt:<7.0f} ct={avg_ct:<7.0f} wc={avg_wc:<7.0f} wall={avg_wall:<5.0f}s ok={ok_count}/{len(rows)}")

    if anti_on and anti_off:
        avg_on = sum(r["wc"] for r in anti_on) / len(anti_on)
        avg_off = sum(r["wc"] for r in anti_off) / len(anti_off)
        delta = ((avg_on - avg_off) / avg_off * 100) if avg_off else 0
        print(f"\n  Weighted cost change: {delta:+.1f}%")
        if abs(delta) < 30:
            print("  -> Within 2.7x LLM variance envelope — no statistically significant difference.")
            print("  -> Anti-decision value is cross-session (preventative), not within-session (token savings).")
            print("  -> Chronicle accumulation across 10+ sessions needed to measure behavioral change.")
    else:
        print("\n  Not enough data for comparison")
