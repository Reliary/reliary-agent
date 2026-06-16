#!/usr/bin/env python3
"""
Anti-decision benchmark: measures whether chronicle-backed sticky failure memory
changes LLM behavior by warning against identifiers with repeated failures.

Methodology:
  1. Pre-seed chronicle with 3+ failures for a dangerous identifier
  2. Run interleaved: anti-on vs anti-off (RELIARY_PROXY_ANTI_DISABLE=1)
  3. Compare weighted cost + fix correctness

The proxy must be running for this benchmark — anti-decision annotations
fire in the proxy's proxy_post handler, injecting ` -identifier` markers
(via Markov surprise) into tool results when both the file and identifier
appear in the text.
"""
import json, os, subprocess, sys, time, shutil, sqlite3

PI = os.path.expanduser("~/.local/bin/pi")
REPO = os.path.expanduser("~/src/stria")
REPO_CWD = REPO
DAEMON_PORT = 9090

# Identifiers that historically cause repeat failures on stria zone.rs bugs
# LLM tends to tweak these wrong
FAILING_IDENTIFIERS = [
    ("src/zone.rs", "idents", 3),       # idents threshold bug — LLM repeatedly gets this wrong
    ("src/zone.rs", "struct_ratio", 2), # struct_ratio threshold — also commonly mis-adjusted
]

P = [
    "Read src/zone.rs. Understand the line_zone function and how it classifies prose vs code lines.",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' and list all failures.",
    "Fix the line_zone function in src/zone.rs so all zone tests pass. There is a bug in the prose classification logic — the thresholds are inverted.",
    "Run 'cargo test --bin stria -- zone --quiet 2>&1' to verify all tests pass.",
]


def seed_chronicle(workdir):
    """Insert anti-decision failures directly into the proxy's chronicle DB."""
    chronicle_path = os.path.join(workdir, ".reliary", "chronicle.sqlite")
    os.makedirs(os.path.dirname(chronicle_path), exist_ok=True)

    db = sqlite3.connect(chronicle_path)
    db.row_factory = sqlite3.Row
    db.execute("""
        CREATE TABLE IF NOT EXISTS chronicle (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            t INTEGER NOT NULL,
            event TEXT NOT NULL,
            file TEXT NOT NULL DEFAULT '',
            detail TEXT NOT NULL DEFAULT '',
            outcome TEXT NOT NULL DEFAULT ''
        )
    """)
    db.execute("PRAGMA journal_mode = WAL")
    db.execute("PRAGMA synchronous = NORMAL")

    now = int(time.time())
    db.execute("DELETE FROM chronicle WHERE event = 'antidecision'")

    for file_path, identifier, num_failures in FAILING_IDENTIFIERS:
        for _ in range(num_failures):
            detail = f"{file_path}::{identifier}::fail"
            db.execute(
                "INSERT INTO chronicle (t, event, file, detail, outcome) VALUES (?, ?, ?, ?, ?)",
                (now, "antidecision", file_path, detail, "edit")
            )

    count = db.execute("SELECT COUNT(*) FROM chronicle WHERE event = 'antidecision'").fetchone()[0]
    db.commit()
    db.close()
    print(f"[seed] Pre-seeded {count} anti-decision failures in {chronicle_path}")
    return count


def check_proxy(port):
    """Verify the proxy is running and responding."""
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


def wasp_proxy(port, workdir, env_extra=None):
    """Start the proxy with the correct workdir for chronicle resolution."""
    env = os.environ.copy()
    if env_extra:
        env.update(env_extra)
    subprocess.run(["pkill", "-f", "reliary-agent serve"], capture_output=True)
    time.sleep(1)
    # Start from workdir so SessionState::new(".") resolves to the project root
    binary = os.path.expanduser("~/src/reliary-agent/target/release/reliary-agent")
    if not os.path.exists(binary):
        print(f"[proxy] Binary not found at {binary} — build first")
        return False
    subprocess.Popen(
        [binary, "serve"],
        cwd=workdir,
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    for _ in range(20):
        time.sleep(0.5)
        if check_proxy(port):
            print(f"[proxy] Started on :{port} (workdir={workdir})")
            return True
    print("[proxy] FAILED to start")
    return False


def parse_turn(stdout):
    pt = ct = tc = 0
    for line in stdout.splitlines():
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
    subprocess.run(["git", "checkout", "master", "--", "."],
                   capture_output=True, cwd=REPO)
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"],
                   capture_output=True, cwd=REPO)
    subprocess.run(["rm", "-rf", ".stria"], capture_output=True, cwd=REPO)
    r = subprocess.run(["cargo", "test", "--bin", "stria", "--", "zone", "--quiet"],
                       capture_output=True, text=True, timeout=60, cwd=REPO)
    return "FAILED" in r.stdout or "FAILED" in r.stderr


def run_condition(condition, run_idx, sfile):
    env = os.environ.copy()
    env["PI_DISABLE_HEARTBEAT"] = "1"
    env["DEEPSEEK_BASE_URL"] = f"http://127.0.0.1:{DAEMON_PORT}/v1"

    if condition == "anti-off":
        env["RELIARY_PROXY_ANTI_DISABLE"] = "1"

    total_pt = total_ct = total_tc = total_wall = 0.0

    for ti, prompt in enumerate(P):
        args = [PI, "--model", "deepseek/deepseek-v4-flash",
                "--mode", "json", "--session", sfile, "--print", prompt]
        t0 = time.time()
        try:
            r = subprocess.run(args, capture_output=True, text=True, timeout=300,
                               env=env, cwd=REPO_CWD)
            wt = time.time() - t0
        except subprocess.TimeoutExpired:
            r = None
            wt = 300

        pt, ct, tc = parse_turn((r and r.stdout) or "")
        total_pt += pt
        total_ct += ct
        total_tc += tc
        total_wall += wt

        print(f"  turn {ti+1}: pt={pt:<5} ct={ct:<4} tc={tc:<2} {wt:<4.0f}s")

    return {
        "condition": condition,
        "pt": total_pt,
        "ct": total_ct,
        "wc": total_pt + 4 * total_ct,
        "tc": total_tc,
        "wall": round(total_wall, 1),
    }


if __name__ == "__main__":
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 2
    print("=== Anti-Decision Benchmark ===\n")
    print(f"Repo: {REPO}")
    print(f"Proxy: :{DAEMON_PORT}")
    print(f"Identifiers: {FAILING_IDENTIFIERS}")
    print(f"Runs: {runs} (interleaved anti-on/anti-off)\n")

    seed_count = seed_chronicle(REPO_CWD)
    if seed_count == 0:
        print("ERROR: chronicle seeding failed")
        sys.exit(1)

    if not wasp_proxy(DAEMON_PORT, REPO_CWD):
        print("ERROR: proxy did not start")
        sys.exit(1)

    results = []
    for ri in range(runs):
        for cond in ["anti-on", "anti-off"]:
            buggy = reset_repo()
            sfile = f"/tmp/bench-anti-{cond}-r{ri}.json"
            if os.path.exists(sfile):
                os.remove(sfile)
            label = f"[{ri+1}/{runs}] {cond} (bug present: {buggy})"
            print(f"\n{label}")
            m = run_condition(cond, ri, sfile)
            r = subprocess.run(
                ["cargo", "test", "--bin", "stria", "--", "zone", "--quiet"],
                capture_output=True, text=True, timeout=60, cwd=REPO
            )
            m["ok"] = "FAILED" not in (r.stdout or "") and "error" not in (r.stdout or "").lower()
            v = " +OK" if m["ok"] else " FAIL"
            print(f"  -> pt={m['pt']:.0f} ct={m['ct']:.0f} wc={m['wc']:.0f} {m['wall']:.0f}s{v}")
            results.append(m)

    print("\n" + "=" * 70)
    anti_on = [r for r in results if r["condition"] == "anti-on"]
    anti_off = [r for r in results if r["condition"] == "anti-off"]

    for label, rows in [("Anti ON (default)", anti_on), ("Anti OFF (disable=1)", anti_off)]:
        if not rows:
            continue
        avg_wc = sum(r["wc"] for r in rows) / len(rows)
        avg_wall = sum(r["wall"] for r in rows) / len(rows)
        ok_count = sum(1 for r in rows if r["ok"])
        print(f"  {label:<22} wc={avg_wc:<7.0f} wall={avg_wall:<5.0f}s ok={ok_count}/{len(rows)}")

    if anti_on and anti_off:
        avg_on = sum(r["wc"] for r in anti_on) / len(anti_on)
        avg_off = sum(r["wc"] for r in anti_off) / len(anti_off)
        delta = ((avg_on - avg_off) / avg_off * 100) if avg_off else 0
        print(f"\n  Weighted cost change: {delta:+.1f}%")
    else:
        print("\n  Not enough data for comparison")
