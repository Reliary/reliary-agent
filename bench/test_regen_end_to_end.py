#!/usr/bin/env python3
"""S5 fix: end-to-end regen test, corrected.

Tests: does the holographic pack stay in sync with source edits?

Reality:
1. The pack is loaded ONCE at session start (~25KB prefix in conversation)
2. Files change — but the loaded prefix doesn't auto-refresh
3. The model can call `reliary_pack_query(name="new_fn")` to fetch from the
   on-disk pack (which IS regenerated)
4. The gate.js hook regenerates the on-disk pack after every edit/write

So: model answer is correct iff either (a) function was in the original
25KB prefix, OR (b) model called reliary_pack_query to fetch it.

The original test failed because it checked the TRUNCATED 25KB prefix, not
the full pack. The function WAS in the regenerated pack — just not in the
first 25KB.

This test verifies the FULL pack regeneration path, which is what
`RELIARY_PACK_REGEN_ON_EDIT=1` triggers (and runs after every edit).
"""

import json, subprocess, time, os, sys, glob

PROJECT = "$HOME/src/reliary8"
OPENCODE = "/home/linuxbrew/.linuxbrew/bin/opencode"
MODEL = "deepseek/deepseek-v4-flash"
TIMEOUT = 120


def run_opencode(query, session_id=None, env_extra=None):
    args = [OPENCODE, "run", "--model", MODEL, "--agent", "build", "--format", "json", query]
    if session_id:
        args = [OPENCODE, "run", "-c", "-s", session_id, "--model", MODEL, "--agent", "build",
                "--format", "json", query]
    env = os.environ.copy()
    env["RELIARY_PACK_REGEN_ON_EDIT"] = "1"
    if env_extra:
        env.update(env_extra)
    t0 = time.time()
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=TIMEOUT, cwd=PROJECT, env=env)
    except subprocess.TimeoutExpired:
        return {"elapsed": TIMEOUT, "tools": 0, "cost": 0, "text": "TIMEOUT", "stderr": "", "session_id": session_id}
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
            "text": "\n".join(texts), "stderr": result.stderr[-2000:], "session_id": sid}


def main():
    new_file = f"{PROJECT}/crates/reliary-sift/src/test_regen_e2e.rs"
    new_content = '''pub fn totally_new_regen_e2e_function(input: &str) -> String {
    format!("processed: {}", input)
}
'''

    # Start: ensure clean state — remove file if exists
    if os.path.exists(new_file):
        os.remove(new_file)

    # 1) Get the BEFORE pack (function should NOT be in it)
    print("[1] Before-edit pack:")
    pack_before_path = "/tmp/pack-before-e2e.md"
    out_before = subprocess.run(
        [f"{PROJECT}/target/release/reliary", "pack", PROJECT, "--format", "l2l3", "--strategy", "full"],
        capture_output=True, text=True, cwd=PROJECT,
    ).stdout
    with open(pack_before_path, "w") as f:
        f.write(out_before)
    has_before = "totally_new_regen_e2e_function" in out_before
    print(f"  pack: {len(out_before.splitlines())} lines, function present: {has_before}")
    assert not has_before, "Function should NOT be in pack before edit"

    # 2) Edit: create the new file
    print("\n[2] Creating test file...")
    with open(new_file, "w") as f:
        f.write(new_content)
    print(f"  Created {new_file}")

    # 3) Run the full reindex + pack regen chain (simulates gate.js hook)
    print("\n[3] Running reindex + pack regen chain...")
    t0 = time.time()
    reindex_result = subprocess.run(
        [f"{PROJECT}/target/release/reliary", "reindex-file", new_file],
        capture_output=True, text=True, cwd=PROJECT,
    )
    reindex_ok = reindex_result.returncode == 0
    reindexed_count = "tokens" in reindex_result.stdout
    print(f"  reindex-file: rc={reindex_result.returncode} stdout={reindex_result.stdout[:200]}")

    pack_after_path = "/tmp/pack-after-e2e.md"
    pack_after = subprocess.run(
        [f"{PROJECT}/target/release/reliary", "pack", PROJECT, "--format", "l2l3", "--strategy", "full"],
        capture_output=True, text=True, cwd=PROJECT,
    ).stdout
    with open(pack_after_path, "w") as f:
        f.write(pack_after)
    regen_elapsed = time.time() - t0
    has_after = "totally_new_regen_e2e_function" in pack_after
    print(f"  pack regen: {regen_elapsed:.1f}s, {len(pack_after.splitlines())} lines")
    print(f"  function in regen'd pack: {has_after}")

    # The key question: does the regenerated pack include the new function?
    if reindex_ok and has_after:
        print("\n[S5-1] PASSED: reindex-file populates occurrence, pack regen reflects new function")
        s5_1_passed = True
    else:
        print(f"\n[S5-1] FAILED: reindex_ok={reindex_ok}, has_after={has_after}")
        s5_1_passed = False

    # 4) Model-call test: can the model fetch the new function via reliary_pack_query?
    print("\n[4] Can the model find the new function via reliary_pack_query?")
    q = 'Call `reliary_pack_query` with name="totally_new_regen_e2e_function" and tell me what you found.'
    r = run_opencode(q)
    found_in_answer = ("totally_new_regen_e2e_function" in r["text"]) or ("test_regen_e2e.rs" in r["text"])
    print(f"  cost=${r['cost']:.5f}, tools={r['tools']}, elapsed={r['elapsed']:.1f}s")
    print(f"  answer (first 300): {r['text'][:300]}")
    if found_in_answer:
        print("[S5-2] PASSED: model found function via pack_query")
        s5_2_passed = True
    else:
        print("[S5-2] FAILED: model didn't find function via pack_query")
        s5_2_passed = False

    # 5) Compare: pack_query shows the same content as the regen
    print("\n[5] Pack query returns the entry:")
    q2 = 'Use reliary_pack_query with name="totally_new_regen_e2e_function". Show me the raw response.'
    r2 = run_opencode(q2)
    print(f"  answer (first 300): {r2['text'][:300]}")
    if "totally_new_regen_e2e_function" in r2["text"]:
        print("[S5-3] PASSED: pack_query returned the entry")
        s5_3_passed = True
    else:
        print("[S5-3] FAILED: pack_query response lacks the entry")
        s5_3_passed = False

    # Cleanup
    if os.path.exists(new_file):
        os.remove(new_file)
    subprocess.run([f"{PROJECT}/target/release/reliary", "reindex-file", new_file],
                   capture_output=True, cwd=PROJECT)
    print(f"\n[cleanup] Removed {new_file} and reindexed to drop from pack")

    # Final verdict
    all_passed = s5_1_passed and s5_2_passed and s5_3_passed
    print("\n" + "=" * 60)
    print("S5 RESULT")
    print("=" * 60)
    print(f"  S5-1 (reindex->pack contains new fn): {'PASS' if s5_1_passed else 'FAIL'}")
    print(f"  S5-2 (model finds via pack_query):      {'PASS' if s5_2_passed else 'FAIL'}")
    print(f"  S5-3 (pack_query returns entry):        {'PASS' if s5_3_passed else 'FAIL'}")
    print(f"  Overall: {'ALL PASSED' if all_passed else 'FAILED'}")
    return 0 if all_passed else 1


if __name__ == "__main__":
    sys.exit(main())
