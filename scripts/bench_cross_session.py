#!/usr/bin/env python3
"""Cross-session memory test. Runs 10 session pairs on the same bug.
Sessions 1-5 populate the chronicle. Sessions 6-10 test if the prior changes LLM behavior."""

import sys, os, json, subprocess, time, shutil

STRA = "/home/dev/src/stria"
PI = os.path.expanduser("~/.local/bin/pi")
BIN = "/home/dev/src/reliary-agent/target/release/reliary-agent"

def run_pi(label, timeout=240):
    sfile = f"/tmp/cross-session-{label}-{int(time.time()*1000)}.jsonl"
    env = os.environ.copy()
    env["PI_SESSION_FILE"] = sfile
    env["RELIARY_MODE"] = "fast"
    t0 = time.time()
    r = subprocess.run(
        [PI, "--model", "deepseek/deepseek-v4-flash", "--mode", "json", "--print",
         "Find and fix the bug in src/zone.rs. Run cargo test to verify.", "--session", sfile],
        capture_output=True, timeout=timeout, cwd=STRA, env=env
    )
    dt = time.time() - t0
    out = r.stdout.decode()
    pt, ct, reads, edits = 0, 0, 0, 0
    for line in out.strip().split("\n"):
        try:
            d = json.loads(line)
            if d.get("type") == "message_end":
                usage = d.get("message", {}).get("usage", {})
                pt += usage.get("input", 0)
                ct += usage.get("output", 0)
            elif d.get("type") in ("tool_call", "tool_use"):
                name = d.get("name", d.get("toolName", ""))
                if name == "read": reads += 1
                elif name == "edit": edits += 1
        except: pass
    return {"pt": pt, "ct": ct, "reads": reads, "edits": edits, "wc": pt + 4*ct, "wt": dt}

def reset_bug():
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"], cwd=STRA, capture_output=True)
    subprocess.run(["git", "clean", "-fd", "--", "src/"], cwd=STRA, capture_output=True)

def reset_chronicle():
    for d in [f for f in os.listdir(".") if os.path.isdir(f)]:
        cp = os.path.join(d, ".reliary/chronicle.sqlite")
        if os.path.exists(cp):
            os.remove(cp)

def main():
    print("=== Cross-Session Memory Test ===\n")
    
    # Phase 1: sessions 1-5 (populate chronicle)
    print("Phase 1: Populating chronicle (sessions 1-2)")
    phase1 = []
    for i in range(2):
        reset_bug()
        r = run_pi(f"phase1_{i+1}")
        phase1.append(r)
        print(f"  Session {i+1}: wc={r['wc']} reads={r['reads']} edits={r['edits']} wt={r['wt']:.0f}s")
    
    p1_avg = sum(r['wc'] for r in phase1) / len(phase1)
    p1_reads = sum(r['reads'] for r in phase1) / len(phase1)
    print(f"\n  Phase 1 avg: wc={p1_avg:.0f} reads={p1_reads:.1f}\n")
    
    # Phase 2: sessions 6-10 (with populated chronicle)
    print("Phase 2: With chronicle prior (sessions 3-4)")
    phase2 = []
    for i in range(2):
        reset_bug()
        r = run_pi(f"phase2_{i+1}")
        phase2.append(r)
        print(f"  Session {i+1}: wc={r['wc']} reads={r['reads']} edits={r['edits']} wt={r['wt']:.0f}s")
    
    p2_avg = sum(r['wc'] for r in phase2) / len(phase2)
    p2_reads = sum(r['reads'] for r in phase2) / len(phase2)
    print(f"\n  Phase 2 avg: wc={p2_avg:.0f} reads={p2_reads:.1f}")
    print(f"  Change: wc={(p2_avg-p1_avg)/p1_avg*100:+.1f}%  reads={(p2_reads-p1_reads)/p1_reads*100:+.1f}%")

if __name__ == "__main__":
    main()
