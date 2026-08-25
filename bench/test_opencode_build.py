#!/usr/bin/env python3
"""Real opencode build agent test with direct DeepSeek provider.

Conditions:
- A: build agent, no MCP, no pack
- F: build agent + reliary MCP + holographic pack in agent prompt
- N: build agent with no tools (just the LLM, no bash/read/grep)
"""

import json
import os
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).parent
OPENCODE_BIN = "/home/linuxbrew/.linuxbrew/bin/opencode"
PROJECT = "$HOME/src/reliary8"
PACK_PATH = "/tmp/reliary8-opencode-pack.md"
CONFIG_PATH = "$HOME/.config/opencode/opencode.json"
DEEPSEEK_MODEL = "deepseek/deepseek-v4-flash"

# ANSWERABLE tasks where the pack provides pre-computed facts.
# Each task IS solvable by grep+read, but the pack has the facts pre-loaded.
TASKS = [
    {
        "id": "hex_hash_bounds",
        "task": "In the reliary8 codebase, the `skeleton()` function detects hex hashes. What are the MINIMUM and MAXIMUM hex hash lengths it recognizes? Find the exact values in the code.",
        "expected_facts": ["7", "40", "hex", "hash"],
    },
    {
        "id": "first_comment_behavior",
        "task": "In the reliary8 codebase, when `compress_content()` processes comments, it treats the FIRST comment differently from subsequent comments. What exactly is the difference? Is the first comment kept or stripped?",
        "expected_facts": ["first", "comment", "keep", "true"],
    },
    {
        "id": "skeleton_crate_location",
        "task": "In the reliary8 codebase, which CRATE contains the `skeleton()` function? Give the crate name.",
        "expected_facts": ["sift", "reliary"],
    },
    {
        "id": "aggressive_single_letter",
        "task": "In the reliary8 codebase, how does `aggressive_skeleton()` handle SINGLE-LETTER words? Are they collapsed to `{w}` or kept as-is?",
        "expected_facts": ["single", "letter", "verbatim", "keep"],
    },
    {
        "id": "maxwell_default_entropy",
        "task": "In the reliary8 codebase, the `MaxwellGate` struct has an entropy_threshold. What is its DEFAULT value (from the Default impl or const)?",
        "expected_facts": ["3.5", "entropy", "default"],
    },
]


def set_config(agent_config, mcp_config=None):
    """Update the opencode config with given agent and MCP settings."""
    with open(CONFIG_PATH) as f:
        d = json.load(f)
    d["agent"]["build"] = agent_config
    if mcp_config is not None:
        d["mcp"] = mcp_config
    with open(CONFIG_PATH, "w") as f:
        json.dump(d, f, indent=2)


def generate_pack():
    """Generate the holographic pack for reliary8."""
    result = subprocess.run(
        ["$HOME/src/reliary8/target/release/reliary", "pack", PROJECT, "--format", "l2l3"],
        capture_output=True, text=True, timeout=30,
    )
    if result.returncode != 0:
        print(f"Pack generation FAILED: {result.stderr}")
        return ""
    pack = result.stdout
    with open(PACK_PATH, "w") as f:
        f.write(pack)
    print(f"Pack generated: {len(pack):,} chars -> {PACK_PATH}")
    return pack


def slice_pack(pack_text, query, top_k=5):
    """Slice the full pack for a specific query using BM25."""
    import sys
    sys.path.insert(0, str(HERE))
    try:
        from unseen_session_bench import slice_pack_for_query, _extract_func_names_from_question
        func_names = _extract_func_names_from_question(query)
        query_text = func_names[0] if func_names else query
        result = slice_pack_for_query(pack_text, query_text, top_k)
        # If the result is empty, fall back to the raw query text
        if not result.strip():
            result = slice_pack_for_query(pack_text, query, top_k)
        return result
    except Exception:
        # Fallback: just take the first N entries
        entries = pack_text.split("## ")[:top_k + 1]
        return "## " + "\n## ".join(entries[1:])


def run_task(task, cond, pack_text="", timeout=60):
    """Run a single coding task via the real opencode build agent."""
    # Configure the build agent based on condition
    base_tools = {
        "bash": True, "read": True, "grep": True, "glob": True,
        "edit": False, "write": False,  # disable writes for safety
    }

    # BUG FIX: set_config overwrites the ENTIRE agent config including permission rules.
    # Without permission rules, MCP tools get auto-rejected by opencode's "*": "ask" catch-all.
    # Include explicit permission rules in EVERY agent_config to allow MCP tools.
    base_permission = {
        "*": "allow",
        "bash": "allow",
        "read": "allow",
        "grep": "allow",
        "glob": "allow",
        "edit": "deny",
        "write": "deny",
    }

    # Slice the pack for this specific query (BUG FIX: was sending full 244K pack)
    sliced_pack = ""
    if cond in ("F", "F2") and pack_text:
        sliced_pack = slice_pack(pack_text, task, top_k=5)

    if cond == "A":
        # build agent, no MCP, no pack
        agent_config = {
            "mode": "primary",
            "model": DEEPSEEK_MODEL,
            "prompt": "You are a software engineering assistant. Answer coding questions about the codebase using your tools.",
            "tools": base_tools,
            "permission": base_permission,
        }
        set_config(agent_config, mcp_config={
            "codebase-memory-mcp": {"type": "local", "command": ["$HOME/.local/bin/codebase-memory-mcp"], "enabled": False},
            "context7": {"type": "remote", "url": "https://mcp.context7.com/mcp", "enabled": False, "headers": {"CONTEXT7_API_KEY": "ctx7sk-623f92ed-07db-4365-95ac-6a5dfd6e931d"}},
            "quale": {"type": "local", "command": ["$HOME/.local/bin/quale", "--mcp"], "enabled": False},
            "stria": {"type": "local", "command": ["$HOME/src/stria/target/release/stria", "serve"], "enabled": False},
            "reliary": {"type": "local", "command": ["$HOME/src/reliary8/target/release/reliary", "mcp"], "enabled": False},
        })
    elif cond == "F":
        # build agent + reliary MCP + SLICED pack in prompt (not full 244K)
        pack_prompt = f"""You are a software engineering assistant with access to the reliary codebase intelligence tools and a holographic pack of the codebase.

## Relevant codebase context (sliced for this query)

{sliced_pack}

## Task

Answer the user's question using the pack context and the reliary tools. Prefer the pack for comprehension, call tools for precision."""
        agent_config = {
            "mode": "primary",
            "model": DEEPSEEK_MODEL,
            "prompt": pack_prompt,
            "tools": base_tools,
            "permission": base_permission,
        }
        set_config(agent_config, mcp_config={
            "codebase-memory-mcp": {"type": "local", "command": ["$HOME/.local/bin/codebase-memory-mcp"], "enabled": False},
            "context7": {"type": "remote", "url": "https://mcp.context7.com/mcp", "enabled": False, "headers": {"CONTEXT7_API_KEY": "ctx7sk-623f92ed-07db-4365-95ac-6a5dfd6e931d"}},
            "quale": {"type": "local", "command": ["$HOME/.local/bin/quale", "--mcp"], "enabled": False},
            "stria": {"type": "local", "command": ["$HOME/src/stria/target/release/stria", "serve"], "enabled": False},
            "reliary": {"type": "local", "command": ["$HOME/src/reliary8/target/release/reliary", "mcp"], "enabled": True},
        })
    elif cond == "F2":
        # build agent + SLICED pack in prompt, NO reliary MCP, basic tools only
        # This isolates the pack's contribution from the reliary MCP's interference
        pack_prompt = f"""You are a software engineering assistant with access to a holographic pack of the codebase.

## Relevant codebase context (sliced for this query)

{sliced_pack}

## Task

Answer the user's question using the pack context and your file tools (grep, read, glob). The pack provides pre-computed codebase intelligence — use it to find what you need, then read the actual source for verification."""
        agent_config = {
            "mode": "primary",
            "model": DEEPSEEK_MODEL,
            "prompt": pack_prompt,
            "tools": base_tools,
            "permission": base_permission,
        }
        set_config(agent_config, mcp_config={
            "codebase-memory-mcp": {"type": "local", "command": ["$HOME/.local/bin/codebase-memory-mcp"], "enabled": False},
            "context7": {"type": "remote", "url": "https://mcp.context7.com/mcp", "enabled": False, "headers": {"CONTEXT7_API_KEY": "ctx7sk-623f92ed-07db-4365-95ac-6a5dfd6e931d"}},
            "quale": {"type": "local", "command": ["$HOME/.local/bin/quale", "--mcp"], "enabled": False},
            "stria": {"type": "local", "command": ["$HOME/src/stria/target/release/stria", "serve"], "enabled": False},
            "reliary": {"type": "local", "command": ["$HOME/src/reliary8/target/release/reliary", "mcp"], "enabled": False},
        })
    elif cond == "N":
        # build agent with --pure (no native tools, no plugins)
        # BUG FIX: use --pure flag instead of disabling tools in config
        agent_config = {
            "mode": "primary",
            "model": DEEPSEEK_MODEL,
            "prompt": "You are a software engineering assistant. Be practical and concise. Answer coding questions about the codebase using only your training knowledge.",
            "tools": {"bash": False, "read": False, "grep": False, "glob": False,
                      "edit": False, "write": False, "task": False, "skill": False,
                      "question": False, "webfetch": False, "todowrite": False,
                      "ctx_reduce": False, "ctx_expand": False, "ctx_note": False,
                      "ctx_search": False, "ctx_memory": False, "gemini_quota": False,
                      "codex-status": False, "codex-switch-accounts": False,
                      "codex-toggle-account": False, "codex-remove-account": False,
                      "create-personality": False},
            "permission": base_permission,
        }
        set_config(agent_config, mcp_config={
            "codebase-memory-mcp": {"type": "local", "command": ["$HOME/.local/bin/codebase-memory-mcp"], "enabled": False},
            "context7": {"type": "remote", "url": "https://mcp.context7.com/mcp", "enabled": False, "headers": {"CONTEXT7_API_KEY": "ctx7sk-623f92ed-07db-4365-95ac-6a5dfd6e931d"}},
            "quale": {"type": "local", "command": ["$HOME/.local/bin/quale", "--mcp"], "enabled": False},
            "stria": {"type": "local", "command": ["$HOME/src/stria/target/release/stria", "serve"], "enabled": False},
            "reliary": {"type": "local", "command": ["$HOME/src/reliary8/target/release/reliary", "mcp"], "enabled": False},
        })
    else:
        raise ValueError(f"Unknown condition: {cond}")

    # Run the task
    # BUG FIX: add --pure for N condition to disable all native tools
    extra_args = []
    if cond == "N":
        extra_args.append("--pure")

    t0 = time.time()
    try:
        result = subprocess.run(
            [OPENCODE_BIN, "run", task,
             "--model", DEEPSEEK_MODEL,
             "--agent", "build",
             "--format", "json"] + extra_args,
            cwd=PROJECT,
            capture_output=True, text=True, timeout=timeout,
        )
        elapsed = time.time() - t0
        return {
            "cond": cond, "task": task[:80], "elapsed": elapsed,
            "returncode": result.returncode,
            # BUG FIX: save FULL stdout (was [-3000:] which truncated tool calls)
            "stdout": result.stdout if result.stdout else "",
            "stderr": result.stderr[-500:] if result.stderr else "",
            "_sliced_pack_size": len(sliced_pack),
        }
    except subprocess.TimeoutExpired:
        return {
            "cond": cond, "task": task[:80], "elapsed": time.time() - t0,
            "returncode": -1, "stdout": "", "stderr": "TIMEOUT",
            "_sliced_pack_size": len(sliced_pack),
        }


def parse_metrics(stdout):
    """Extract tool calls, tokens, and final answer from opencode JSONL output."""
    events = []
    for line in stdout.splitlines():
        line = line.strip()
        if not line or not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
            events.append(d)
        except Exception:
            pass

    metrics = {"tool_calls": 0, "tools_used": set(), "input_tokens": 0, "output_tokens": 0, "cost": 0, "answer": "", "all_text": ""}

    text_parts = []
    for d in events:
        t = d.get("type", "")
        part = d.get("part", {})

        # tool_use
        if t == "tool_use":
            metrics["tool_calls"] += 1
            tool = part.get("tool", "unknown")
            metrics["tools_used"].add(tool)

        # step_finish has tokens
        if t == "step_finish":
            tok = part.get("tokens", {}) or d.get("tokens", {})
            metrics["input_tokens"] += tok.get("input", 0)
            metrics["output_tokens"] += tok.get("output", 0)
            cost = d.get("cost", 0)
            metrics["cost"] += cost

        # text can be in 'text' field or part.text
        if t == "text":
            text = d.get("text") or part.get("text", "")
            if text:
                text_parts.append(text)

    # Concatenate all text for scoring
    metrics["all_text"] = " ".join(text_parts)
    # Use the longest single text as the "answer"
    if text_parts:
        metrics["answer"] = max(text_parts, key=len)
    metrics["tools_used"] = list(metrics["tools_used"])
    return metrics


def score_answer(answer, expected_facts):
    """Score 0-3 based on expected facts found in the answer.
    Splits compound facts like 'classify.rs:108' into individual tokens."""
    answer_lower = (answer or "").lower()
    all_tokens = []
    for fact in expected_facts:
        # Split on common separators to allow partial matches
        tokens = [t for t in fact.replace(":", " ").replace(".", " ").replace(",", " ").lower().split() if t]
        all_tokens.extend(tokens)
    if not all_tokens:
        return 0
    hits = sum(1 for t in all_tokens if t in answer_lower)
    ratio = hits / len(all_tokens)
    if ratio >= 0.7:
        return 3
    elif ratio >= 0.4:
        return 2
    elif ratio >= 0.15:
        return 1
    return 0


def main():
    print("=== Generating holographic pack ===")
    pack = generate_pack()
    if not pack:
        print("FAIL")
        return

    results = []
    for task in TASKS:
        for cond in ["N", "A", "F2", "F"]:
            print(f"\n=== {task['id']} | {cond} ===")
            r = run_task(task["task"], cond, pack, timeout=60)
            metrics = parse_metrics(r["stdout"])
            r.update(metrics)
            r["task_id"] = task["id"]
            r["score"] = score_answer(metrics.get("all_text", ""), task["expected_facts"])
            r["completed"] = r["returncode"] == 0 and r["elapsed"] < 55
            r["sliced_pack_size"] = r.pop("_sliced_pack_size", 0)

            print(f"  elapsed={r['elapsed']:.1f}s, tools={r['tool_calls']} ({r['tools_used'][:5]}), "
                  f"in={r['input_tokens']:,}, out={r['output_tokens']:,}, "
                  f"score={r['score']}/3, pack={r['sliced_pack_size']:,} chars, "
                  f"done={r['completed']}")
            results.append(r)

            with open(HERE / "results" / "opencode_build_test.jsonl", "a") as f:
                f.write(json.dumps(r) + "\n")

    # Summary
    print("\n" + "=" * 70)
    print("SUMMARY — opencode build agent, direct deepseek-v4-flash, 5 tasks")
    print("=" * 70)
    from collections import defaultdict
    by_cond = defaultdict(list)
    for r in results:
        by_cond[r["cond"]].append(r)

    for cond in ["N", "A", "F2", "F"]:
        runs = by_cond[cond]
        if not runs:
            continue
        total_score = sum(r["score"] for r in runs)
        total_tools = sum(r["tool_calls"] for r in runs)
        total_cost = sum(r["cost"] for r in runs)
        completed = sum(1 for r in runs if r["completed"])
        print(f"\n{cond}:")
        print(f"  Score:     {total_score}/{len(runs)*3} ({total_score/(len(runs)*3)*100:.0f}%)")
        print(f"  Cost:      ${total_cost:.5f} total")
        print(f"  Tools:     {total_tools} ({total_tools/len(runs):.1f}/task)")
        print(f"  Completed: {completed}/{len(runs)}")


if __name__ == "__main__":
    out = HERE / "results" / "opencode_build_test.jsonl"
    if out.exists():
        out.unlink()
    main()
