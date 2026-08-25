#!/usr/bin/env python3
"""S3 smoke test: verify warmup injection works in a persistent session.

Note: we import test_persistent_session for the shared fns. The imported
module does not auto-run its main() body because it has no `if __name__`
guard, so import would execute the full test. To avoid that, we duplicate
the minimal needed logic here rather than import.
"""
import json, os, subprocess, sys, re, time

PROJECT = "$HOME/src/reliary8"
OPENCODE = "/home/linuxbrew/.linuxbrew/bin/opencode"
MODEL = "deepseek/deepseek-v4-flash"
TIMEOUT = 45


def call_opencode(task, session_id=None):
    """One opencode invocation. Returns parsed events + cost + session id."""
    args = [OPENCODE, "run", "--model", MODEL, "--agent", "build", "--format", "json", task]
    if session_id:
        args = [OPENCODE, "run", "-c", "-s", session_id, "--model", MODEL, "--agent", "build",
                "--format", "json", task]
    t0 = time.time()
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=TIMEOUT, cwd=PROJECT)
    except subprocess.TimeoutExpired:
        return {"elapsed": TIMEOUT, "score": 0, "tools": 0, "cost": 0,
                "text": "TIMEOUT", "session_id": session_id, "events": []}
    elapsed = time.time() - t0
    events = []
    for l in result.stdout.splitlines():
        l = l.strip()
        if l.startswith("{"):
            try: events.append(json.loads(l))
            except: pass
    texts, cost, tools, sid = [], 0, 0, session_id
    for e in events:
        if e.get("type") == "text":
            texts.append(e.get("part", {}).get("text", ""))
        elif e.get("type") == "tool_use":
            tools += 1
        elif e.get("type") == "step_finish":
            cost = e.get("part", {}).get("cost", 0)
        elif e.get("sessionID"):
            sid = e["sessionID"]
    return {"elapsed": elapsed, "tools": tools, "cost": cost,
            "text": "\n".join(texts), "session_id": sid}


def main():
    # Step 1: load pack
    print("Step 1: load pack")
    pack = subprocess.run(
        ["$HOME/src/reliary8/target/release/reliary", "pack", PROJECT,
         "--format", "l2l3"],
        capture_output=True, text=True, timeout=30, cwd=PROJECT,
    ).stdout
    pack = pack[:25000]
    print(f"  pack: {len(pack.splitlines())} lines, {len(pack)} chars")

    r = call_opencode("You are analyzing reliary8. Reply READY after reading.")
    sid = r.get("session_id")
    print(f"  sid={sid[:20] if sid else None}, elapsed={r['elapsed']:.1f}s")

    if not sid:
        print("FAILED: no session id")
        return 1

    # Step 2: warmup
    print("Step 2: warmup")
    fn_match = re.search(r"`(\w+)`", "What does `skeleton()` return?")
    func = fn_match.group(1) if fn_match else "skeleton"
    warmup_text = (
        f"Read these entries and answer briefly:\n"
        f"1. What does `{func}` return for empty/blank input?\n"
        f"2. What's a key surprise or edge case in `{func}`?\n"
        f"3. Which functions call or interact with `{func}`?"
    )
    w = call_opencode(warmup_text, session_id=sid)
    print(f"  cost=${w['cost']:.5f}, tools={w['tools']}, elapsed={w['elapsed']:.1f}s")
    print(f"  warmup_text first 200: {w['text'][:200]}")

    # Step 3: real question after warmup
    print("Step 3: ask real question after warmup")
    r2 = call_opencode("What UUID dash positions does skeleton() detect?", session_id=sid)
    text = r2.get("text", "")
    print(f"  cost=${r2['cost']:.5f}, tools={r2['tools']}, elapsed={r2['elapsed']:.1f}s")
    print(f"  answer_text first 200: {text[:200]}")

    if "8" in text or "13" in text or "uuid" in text.lower():
        print("PASSED: warmup session produced correct UUID answer")
        return 0
    else:
        print(f"FAILED: answer doesn't mention UUID positions")
        return 1


if __name__ == "__main__":
    sys.exit(main())
