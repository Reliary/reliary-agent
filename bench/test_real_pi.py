#!/usr/bin/env python3
"""Real Pi agent test of the holographic pack.

Three conditions:
- A: Pi with reliary MCP tools (no pack)
- F: Pi with reliary MCP tools + holographic pack in system prompt
- N: Pi with no extensions (just bash/read/write/edit)

Uses real coding tasks on the reliary8 codebase.
"""

import json
import os
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).parent
PI_BIN = "$HOME/.local/bin/pi"
RELIARY_BIN = os.path.expanduser("~/src/reliary8/target/release/reliary")
PROJECT = "$HOME/src/reliary8"
PACK_PATH = "/tmp/reliary8-real-pack.md"
PI_SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
RELIARY_EXT = str(HERE / "reliary_mcp_pi_extension.js")

DEEPSEEK_KEY = os.environ.get("DEEPSEEK_API_KEY", "")


def set_extensions(extensions):
    """Atomically swap the pi settings extensions list."""
    with open(PI_SETTINGS) as f:
        d = json.load(f)
    d["extensions"] = extensions
    d["packages"] = extensions
    tmp = PI_SETTINGS + ".tmp"
    with open(tmp, "w") as f:
        json.dump(d, f, indent=2)
    os.rename(tmp, PI_SETTINGS)


def run_pi_task(task, cond, pack_text=None, timeout=60):
    """Run a single coding task via the real Pi agent using a PTY (Pi needs a TTY)."""
    env = os.environ.copy()
    env["RELIARY_BIN"] = RELIARY_BIN
    env["DEEPSEEK_API_KEY"] = DEEPSEEK_KEY
    env["PATH"] = "$HOME/.local/bin:" + env.get("PATH", "")
    env["PI_DISABLE_HEARTBEAT"] = "1"

    args = [
        PI_BIN,
        "--print",
    ]

    if cond == "A":
        set_extensions([RELIARY_EXT])
        args.extend(["--exclude-tools", "edit,write"])
    elif cond == "F":
        set_extensions([RELIARY_EXT])
        if pack_text:
            with open(PACK_PATH, "w") as f:
                f.write(pack_text)
            args.extend(["--append-system-prompt", f"@file://{PACK_PATH}"])
        args.extend(["--exclude-tools", "edit,write"])
    elif cond == "N":
        set_extensions([])
        args.extend(["--exclude-tools", "edit,write"])
    else:
        raise ValueError(f"Unknown condition: {cond}")

    args.append(task)

    # Pi needs a TTY to output correctly (pipe mode causes hangs)
    import pty, select
    master, slave = pty.openpty()
    proc = subprocess.Popen(
        args, cwd=PROJECT, stdin=slave, stdout=slave, stderr=slave,
        env=env, close_fds=True,
    )
    os.close(slave)

    output = b""
    t0 = time.time()
    try:
        while True:
            remaining = timeout - (time.time() - t0)
            if remaining <= 0:
                proc.kill()
                break
            r, _, _ = select.select([master], [], [], min(remaining, 0.5))
            if r:
                try:
                    data = os.read(master, 4096)
                    if not data:
                        break
                    output += data
                except OSError:
                    break
    finally:
        try: os.close(master)
        except: pass
    try: proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()

    elapsed = time.time() - t0
    return {
        "cond": cond, "task": task[:80], "elapsed": elapsed,
        "returncode": proc.returncode,
        "tokens_in": 0, "tokens_out": 0, "tool_calls": 0,
        "stdout": output.decode("utf-8", errors="replace"),
        "stderr": "",
    }


def parse_usage(stdout):
    """Extract token counts and tool call count from Pi's TTY output.
    The output contains [ext] callTool <name> lines for tool invocations
    and may include JSON events with usage information.
    """
    import re
    # Count tool calls from [ext] callTool lines
    tool_calls = len(re.findall(r'\[ext\] callTool', stdout))
    # Also count toolResults as tool invocations completed
    tool_results = len(re.findall(r'\[ext\] toolResult', stdout))
    if tool_results > tool_calls:
        tool_calls = tool_results
    # Look for token counts in any embedded JSON events
    pt = ct = 0
    for line in stdout.splitlines():
        if "usage" in line and '"input"' in line:
            try:
                d = json.loads(line)
                u = d.get("usage", {})
                pt = max(pt, u.get("input", 0))
                ct = max(ct, u.get("output", 0))
            except: pass
    return pt, ct, tool_calls


def generate_pack():
    """Generate the holographic pack for reliary8."""
    result = subprocess.run(
        [RELIARY_BIN, "pack", PROJECT, "--format", "l2l3"],
        capture_output=True, text=True, timeout=30
    )
    if result.returncode != 0:
        print(f"Pack generation FAILED: {result.stderr}")
        return ""
    pack = result.stdout
    print(f"Pack generated: {len(pack):,} chars")
    return pack


# Real coding tasks on reliary8
TASKS = [
    {
        "id": "find_skeleton_def",
        "task": "In the reliary8 codebase, find the definition of the `skeleton` function in `classify.rs`. Show me the file:line and the function signature.",
        "category": "find_definition",
        "expected_facts": ["classify.rs:108", "skeleton"],
    },
    {
        "id": "find_skelhash_callers",
        "task": "In the reliary8 codebase, find all functions that call `skeleton_hash()`. List the function names.",
        "category": "find_callers",
        "expected_facts": ["classify", "skeleton"],
    },
    {
        "id": "find_maxwell",
        "task": "In the reliary8 codebase, find the `MaxwellGate` struct. What is its default entropy threshold?",
        "category": "find_struct",
        "expected_facts": ["maxwell", "entropy", "3.5"],
    },
    {
        "id": "find_linetype",
        "task": "In the reliary8 codebase, find the `LineType` enum. List all its variants.",
        "category": "find_enum",
        "expected_facts": ["linetype", "blank", "comment", "import", "definition"],
    },
    {
        "id": "find_skeleton_behavior",
        "task": "In the reliary8 codebase, the `skeleton()` function in classify.rs — what does it return for empty input?",
        "category": "find_behavior",
        "expected_facts": ["empty", "string", "new"],
    },
]


def score_answer(answer, expected_facts):
    """Score 0-3 based on how many expected facts are mentioned in the answer."""
    answer_lower = answer.lower()
    hits = sum(1 for f in expected_facts if f.lower() in answer_lower)
    if hits >= len(expected_facts) * 0.7:
        return 3
    elif hits >= len(expected_facts) * 0.4:
        return 2
    elif hits >= len(expected_facts) * 0.15:
        return 1
    return 0


def main():
    # Generate pack once
    print("=== Generating holographic pack ===")
    pack_text = generate_pack()
    if not pack_text:
        print("FAIL: no pack generated")
        return

    results = []
    for task in TASKS:
        for cond in ["N", "A", "F"]:
            print(f"\n=== Task: {task['id']} | Cond: {cond} ===")
            r = run_pi_task(task["task"], cond, pack_text if cond == "F" else None, timeout=90)
            pt, ct, tc = parse_usage(r["stdout"])
            r["tokens_in"] = pt
            r["tokens_out"] = ct
            r["tool_calls"] = tc
            r["task_id"] = task["id"]
            r["category"] = task["category"]
            r["score"] = score_answer(r["stdout"], task["expected_facts"])
            r["completed"] = r["returncode"] == 0

            # Cost estimate (Pi is free via DeepSeek API, but we use their pricing)
            cost = pt * 0.14 / 1_000_000 + ct * 0.28 / 1_000_000
            r["cost"] = cost

            print(f"  elapsed={r['elapsed']:.1f}s, in={pt}, out={ct}, tools={tc}, ${cost:.5f}, score={r['score']}/3, done={r['completed']}")
            results.append(r)

            # Write results incrementally so partial runs survive
            with open(HERE / "results" / "real_pi_test.jsonl", "a") as f:
                f.write(json.dumps(r) + "\n")

    # Summary
    print("\n" + "=" * 70)
    print("SUMMARY — Real Pi Agent Test (5 tasks × 3 conditions)")
    print("=" * 70)
    from collections import defaultdict
    by_cond = defaultdict(list)
    for r in results:
        by_cond[r["cond"]].append(r)

    for cond in ["N", "A", "F"]:
        runs = by_cond[cond]
        if not runs: continue
        total_cost = sum(r["cost"] for r in runs)
        total_tools = sum(r["tool_calls"] for r in runs)
        avg_time = sum(r["elapsed"] for r in runs) / len(runs)
        completed = sum(1 for r in runs if r["completed"])
        total_score = sum(r["score"] for r in runs)
        max_score = len(runs) * 3
        print(f"\n{cond}:")
        print(f"  Score:       {total_score}/{max_score} ({total_score/max_score*100:.0f}%)")
        print(f"  Cost:        ${total_cost:.5f} total")
        print(f"  Tools:       {total_tools} ({total_tools/len(runs):.1f}/task)")
        print(f"  Wall time:   {avg_time:.1f}s/task (avg)")
        print(f"  Completed:   {completed}/{len(runs)} (rc=0)")


if __name__ == "__main__":
    # Clear previous results
    out = HERE / "results" / "real_pi_test.jsonl"
    if out.exists():
        out.unlink()
    main()
