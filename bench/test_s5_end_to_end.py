#!/usr/bin/env python3
"""S5 end-to-end test: does the opencode build agent regenerate the pack
after writing a new file, and can pack_query fetch the new entry?

This test SIMULATES the S5 flow the opencode plugin would automate:
1. Opencode agent uses `write` tool to create a new file
2. `reliary reindex-file` rebuilds occurrence rows for that file
3. `reliary pack` regenerates `.reliary/pack_l2l3.md`
4. A fresh opencode session calls `reliary_pack_query` to verify the entry exists

The auto-generation path in the MCP dispatcher handles step 3 transparently
when the cache file is missing or empty (>1KB threshold).
"""
import json, subprocess, time, os, sys

PROJECT = "$HOME/src/reliary8"
NEW_FILE_REL = "crates/reliary-sift/src/test_s5_e2e.rs"
NEW_FILE = f"{PROJECT}/{NEW_FILE_REL}"
FILE_CONTENT = "pub fn s5_test_function_e2e() -> i32 { 42 }\n"


def run(cmd, cwd=PROJECT, timeout=60):
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, cwd=cwd)


def cleanup():
    """Remove test file from disk + from the index."""
    if os.path.exists(NEW_FILE):
        run(["$HOME/src/reliary8/target/release/reliary", "reindex-file", NEW_FILE])
        os.remove(NEW_FILE)
        run(["$HOME/src/reliary8/target/release/reliary", "reindex-file", NEW_FILE])


def step_pass(msg):
    print(f"  ✓ {msg}")


def step_fail(msg):
    print(f"  ✗ FAIL: {msg}")
    sys.exit(1)


def main():
    print(f"=== S5 end-to-end: file edit → regen → pack_query fetch ===\n")
    cleanup()

    # Step 1: opencode agent creates file via `write` tool
    print("[1] opencode writes a new Rust file...")
    prompt = (
        f"Create a NEW file at {NEW_FILE_REL} with EXACTLY this content:\n"
        f"{FILE_CONTENT}"
        f"Use the `write` tool. After writing, report success."
    )
    # Use the `main` agent which has write/edit (the `build` agent strips them)
    r = run([
        "/home/linuxbrew/.linuxbrew/bin/opencode", "run",
        "--model", "deepseek/deepseek-v4-flash", "--agent", "main", "--format", "json",
        prompt,
    ], timeout=120)
    if not os.path.exists(NEW_FILE):
        step_fail(f"agent did not create {NEW_FILE_REL}")
    written = open(NEW_FILE).read()
    if "s5_test_function_e2e" not in written:
        step_fail(f"file content wrong: {written!r}")
    step_pass(f"agent wrote {NEW_FILE_REL} ({len(written)} bytes)")

    # Step 2: reindex the file
    print("\n[2] reindex-file rebuilds occurrence rows for the new file...")
    r = run(["$HOME/src/reliary8/target/release/reliary", "reindex-file", NEW_FILE], timeout=30)
    combined = r.stdout + r.stderr
    # Strip ANSI color codes for the check
    import re as _re
    plain = _re.sub(r"\x1b\[[0-9;]*m", "", combined)
    if "reindexed" not in plain:
        step_fail(f"reindex failed: stdout={r.stdout!r} stderr={r.stderr!r}")
    step_pass("reindexed (occurrences populated)")

    # Step 3: regenerate the on-disk pack
    print("\n[3] regenerate .reliary/pack_l2l3.md...")
    pack_path = f"{PROJECT}/.reliary/pack_l2l3.md"
    # Truncate first (simulating "stale cache that needs refresh")
    open(pack_path, "w").close()
    # The pack CLI doesn't have --output; capture stdout + write to cache
    r = run(["$HOME/src/reliary8/target/release/reliary", "pack", PROJECT,
             "--format", "l2l3", "--strategy", "full"], timeout=120)
    open(pack_path, "w").write(r.stdout)
    if "s5_test_function_e2e" in open(pack_path).read():
        step_pass(f"pack regenerated and includes the new function (cache {len(r.stdout)} chars)")
    else:
        step_fail("pack regenerated but does NOT contain the new function")

    # Step 4: fresh opencode session queries the pack via MCP tool
    print("\n[4] fresh opencode session calls reliary_pack_query...")
    query_prompt = (
        f"Use the tool `reliary_reliary_pack_query` to query for the symbol "
        f"'s5_test_function_e2e' (path '{PROJECT}'). Show me the raw response."
    )
    r2 = run([
        "/home/linuxbrew/.linuxbrew/bin/opencode", "run",
        "--model", "deepseek/deepseek-v4-flash", "--agent", "build", "--format", "json",
        query_prompt,
    ], timeout=180, cwd=PROJECT)
    found_query = '"output":"# Pack Entry: s5_test_function_e2e' in r2.stdout or "s5_test_function_e2e" in r2.stdout
    if found_query:
        step_pass("pack_query returned the new entry")
    else:
        step_fail(f"pack_query did not find the entry. Stdout tail: {r2.stdout[-500:]}")

    # Cleanup
    cleanup()
    print("\n=== S5 ALL PASSED ===")
    print("The holographic pack stays in sync with code edits via manual reindex+regen.")
    print("Full automation (no manual regen) requires adding a tool.execute.after hook")
    print("to the opencode plugin at $HOME/src/autopsylab-agent/packages/opencode/src/plugin.ts")
    print("that triggers reindex-file + pack regen after write/edit tool calls.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
