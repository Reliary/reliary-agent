#!/usr/bin/env python3
"""Deterministic benchmark: record API responses in one run, replay in the next.
Uses the proxy in record/replay mode to eliminate LLM variance between runs.
Run with -C to compare baseline vs gate using the SAME recorded responses."""

import sys, os, json, subprocess, time, shutil, hashlib, argparse

PI_BIN = os.path.expanduser("~/.local/bin/pi")

def run_pi(prompt, label, replay):
    sfile = f"/tmp/bench-{label}-{int(time.time()*1000)}.jsonl"
    env = os.environ.copy()
    env["RELIARY_REPLAY"] = replay
    env["DEEPSEEK_BASE_URL"] = "http://localhost:9090/v1"
    env["PI_DISABLE_HEARTBEAT"] = "1"
    t0 = time.time()
    r = subprocess.run(
        [PI_BIN, "--model", "deepseek/deepseek-v4-flash", "--mode", "json",
         "--print", prompt, "--session", sfile],
        capture_output=True, timeout=360, env=env
    )
    dt = time.time() - t0
    out = r.stdout.decode()
    pt, ct = 0, 0
    for line in out.strip().split("\n"):
        try:
            d = json.loads(line)
            if d.get("type") == "message_end":
                msg = d.get("message", {})
                usage = msg.get("usage", {})
                pt += usage.get("input", 0)
                ct += usage.get("output", 0)
        except: pass
    return {"pt": pt, "ct": ct, "wc": pt + 4*ct, "wt": dt, "out": out}

def reset_bug():
    stra = "$HOME/src/stria"
    subprocess.run(["git", "checkout", "bench-bug", "--", "src/zone.rs"], cwd=stra, capture_output=True)
    subprocess.run(["git", "clean", "-fd", "--", "src/"], cwd=stra, capture_output=True)
    # Verify bug exists
    r = subprocess.run(["cargo", "test", "line_zone_code", "--", "--nocapture"], cwd=stra, capture_output=True, timeout=30)
    return "FAILED" in r.stdout.decode()

def get_fix_from_output(out):
    for line in out.strip().split("\n"):
        try:
            d = json.loads(line)
        except: continue
        for field in ("arguments", "input"):
            edits = d.get(field, {}).get("edits", [])
            if edits:
                for e in edits:
                    new_str = e.get("newText", "")
                    if "if idents" in new_str or "struct_ratio" in new_str:
                        return new_str.strip()
        if d.get("type") == "message":
            content = d.get("content", "")
            if isinstance(content, list):
                for block in content:
                    if block.get("type") == "text" and "if idents" in block.get("text", ""):
                        return block["text"].strip()
    return ""

def start_proxy(replay_mode):
    killall = subprocess.run(["killall", "reliary-agent"], capture_output=True)
    time.sleep(1)
    proxy = subprocess.Popen(
        ["reliary-agent", "serve"],
        env={**os.environ, "RELIARY_REPLAY": replay_mode},
        stderr=subprocess.PIPE, stdout=subprocess.PIPE
    )
    time.sleep(2)
    # Verify proxy is up
    r = subprocess.run(["nc", "-w", "1", "127.0.0.1", "9090"], capture_output=True, input=b"", timeout=3)
    return proxy

def main():
    parser = argparse.ArgumentParser(description="Deterministic benchmark with replay")
    parser.add_argument("--replay-file", default="/tmp/reliary-replay.jsonl",
                       help="Path to replay file")
    parser.add_argument("--record", action="store_true",
                       help="Record API responses (run baseline)")
    parser.add_argument("--replay", action="store_true",
                       help="Replay recorded responses (run gate)")
    parser.add_argument("--dual", action="store_true",
                       help="Dual mode: forward + cache miss, serve on hit")
    args = parser.parse_args()

    prompt = "Read src/zone.rs. Find and fix the bug. Run cargo test to verify."

    # Phase 1: Record
    if args.record:
        os.environ.pop("PI_SESSION_FILE", None)
        os.remove(args.replay_file) if os.path.exists(args.replay_file) else None
        proxy = start_proxy("record")
        if not reset_bug():
            print("WARNING: bug not present after reset — check git state")
        print(f"Recording baseline...")
        r = run_pi(prompt, "baseline-record", "record")
        wc = r["pt"] + 4 * r["ct"]
        fix = get_fix_from_output(r["out"])
        print(f"  pt={r['pt']} ct={r['ct']} wc={wc} {r['wt']:.0f}s")
        if fix: print(f"  fix: {fix[:80]}")
        replay_lines = sum(1 for _ in open(args.replay_file)) if os.path.exists(args.replay_file) else 0
        print(f"  Recorded {replay_lines} API responses")
        proxy.kill()
        return

    # Phase 2: Replay
    if args.replay:
        if not os.path.exists(args.replay_file):
            print(f"ERROR: replay file {args.replay_file} not found. Run with --record first.")
            sys.exit(1)
        proxy = start_proxy("replay")
        replay_lines_before = sum(1 for _ in open(args.replay_file))
        print(f"Replaying gate condition ({replay_lines_before} cached responses)...")
        if not reset_bug():
            print("WARNING: bug not present after reset")
        r = run_pi(prompt, "gate-replay", "replay")
        wc = r["pt"] + 4 * r["ct"]
        fix = get_fix_from_output(r["out"])
        replay_lines_after = sum(1 for _ in open(args.replay_file))
        cache_hits = replay_lines_after - replay_lines_before
        print(f"  pt={r['pt']} ct={r['ct']} wc={wc} {r['wt']:.0f}s")
        if fix: print(f"  fix: {fix[:80]}")
        print(f"  Cache hits: {replay_lines_before - (cache_hits if cache_hits > 0 else 0)}/{replay_lines_before} "
              f"(new entries: {max(0,cache_hits)})")
        proxy.kill()
        return

    # Default: show usage
    parser.print_help()
    print("\n\nSteps:")
    print("  1. python3 bench_deterministic.py --record   # Record baseline API responses")
    print("  2. python3 bench_deterministic.py --replay    # Replay responses for gate")
    print("\n  (Both steps must complete with 'OK' and matching fix pattern)")

if __name__ == "__main__":
    main()
