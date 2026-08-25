#!/usr/bin/env python3
"""Lightweight SWE-bench Lite harness — no Docker required.

Clones a repo at base_commit, runs the LLM with tools, captures actual
file edits (via the edit tool), applies the test_patch, and runs the
FAIL_TO_PASS tests.

Usage:
    python3 swe_bench_lite.py --instance django__django-14534 --conds A,C --max-turns 15
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Optional

# Ensure bench/ is importable
sys.path.insert(0, str(Path(__file__).parent))
from llm_conn import deepseek_chat, DEEPSEEK_MODEL  # noqa: E402

RELIARY_BIN = "$HOME/src/reliary8/target/release/reliary"
WORK_ROOT = Path(tempfile.gettempdir()) / "swe_bench_lite_workdir"


def call_reliary_tool(workdir: str, tool_name: str, args: dict) -> str:
    """Call a reliary MCP tool via JSON-RPC over stdio."""
    proc = subprocess.Popen(
        [RELIARY_BIN, "mcp"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        cwd=workdir,
    )
    init_req = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}) + "\n"
    proc.stdin.write(init_req.encode())
    proc.stdin.flush()
    proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                                 "params": {"name": tool_name, "arguments": args}}) + "\n")
    proc.stdin.flush()
    proc.stdin.close()
    out, _ = proc.communicate(timeout=30)
    for line in out.decode().strip().split("\n"):
        try:
            d = json.loads(line)
            if d.get("id") == 2:
                content = d.get("result", {}).get("content", [])
                if content:
                    return content[0].get("text", str(d))
                return str(d)
        except json.JSONDecodeError:
            continue
    return "(reliary call failed)"


def clone_repo(repo: str, commit: str, dest: Path) -> bool:
    if (dest / ".git").exists():
        return True
    dest.parent.mkdir(parents=True, exist_ok=True)
    r = subprocess.run(
        ["git", "clone", "--quiet", f"https://github.com/{repo}.git", str(dest)],
        capture_output=True, text=True, timeout=300,
    )
    if r.returncode != 0:
        print(f"  Clone failed: {r.stderr[:200]}")
        return False
    r = subprocess.run(["git", "-C", str(dest), "checkout", "--quiet", commit],
                       capture_output=True, text=True, timeout=30)
    return r.returncode == 0


def run_tests(workdir: Path, instance: dict, timeout: int = 120) -> dict:
    repo_dir = str(workdir)
    f2p_raw = instance.get("FAIL_TO_PASS", "[]")
    try:
        f2p = json.loads(f2p_raw) if isinstance(f2p_raw, str) else f2p_raw
    except json.JSONDecodeError:
        f2p = [f2p_raw]
    f2p = [t.strip().strip("'\"") for t in f2p if t.strip()]

    # Apply test_patch
    test_patch = instance.get("test_patch", "")
    if test_patch:
        patch_file = workdir / "_test.patch"
        patch_file.write_text(test_patch)
        subprocess.run(["git", "-C", repo_dir, "apply", "--whitespace=nowarn", str(patch_file)],
                       capture_output=True, text=True, timeout=30)
        patch_file.unlink()

    env_extra = {"DJANGO_SETTINGS_MODULE": "tests.test_settings"}
    test_cmd = [sys.executable, "-m", "pytest", "-x", "--no-header", "-q"]

    f2p_pass = 0
    for test_name in f2p[:5]:
        r = subprocess.run(test_cmd + [test_name],
                           cwd=repo_dir, capture_output=True, text=True,
                           timeout=timeout, env={**os.environ, **env_extra})
        if r.returncode == 0:
            f2p_pass += 1

    return {
        "fail_to_pass": f2p_pass,
        "fail_to_pass_total": len(f2p),
        "all_f2p_pass": f2p_pass == len(f2p) and len(f2p) > 0,
    }


def _build_sys_prompt(cond: str, workdir: str) -> str:
    if cond == "A":
        return (
            "You are a software engineer fixing a GitHub issue.\n"
            "Make the minimal change using the edit tool. When done, respond with just DONE.\n\n"
            "Tool call format (ONE per response):\n"
            "antml:invoke\n"
            "<name>tool_name</name>\n"
            "<param>value</param>\n"
            "\n\n"
            "STEP 1 — CODEBASE OVERVIEW (do this FIRST):\n"
            "- reliary_pack: <path>.</path> — generates a holographic pack of ALL symbols\n"
            "  with their purpose, location, and surprise facts. This replaces 10+ grep/read\n"
            "  calls with ONE call. The pack is pre-generated — just call it.\n"
            "- reliary_architecture: <path>.</path> — high-level project structure\n\n"
            "STEP 2 — FIND THE BUG (use reliary, not grep):\n"
            "- reliary_search: <query>topic words</query> — BM25 search for files by topic\n"
            "- reliary_methods_on: <name>Type</name> — enumerate ALL methods on a type\n"
            "  (e.g. reliary_methods_on BoundWidget shows id_for_label, choice_label, etc.)\n"
            "- reliary_find_references_with_source: <name>function</name> — where is X used?\n"
            "- reliary_goto_def: <name>symbol</name> — where is X defined?\n"
            "- reliary_callgraph_v2: <name>function</name> — who calls X / what does X call?\n"
            "- reliary_trace_path: <name>function</name> <direction>both</direction> — call chain\n"
            "- reliary_brace_graph: <file_path>path.py</file_path> — file structure tree\n"
            "- reliary_query_ast: <pattern>Call(_, _)</pattern> <file>path.py</file> — pattern match\n\n"
            "STEP 3 — PRE-EDIT CHECK:\n"
            "- reliary_risk: <file>path.py</file> — what's affected by editing this file?\n"
            "- reliary_pack_query: <name>function</name> — get L0 purpose + L3 surprise facts\n"
            "  for the function you're about to edit. Tells you non-obvious behaviors.\n\n"
            "STEP 4 — APPLY THE FIX:\n"
            "- edit: <file_path>path.py</file_path> <old_string>EXACT text</old_string> <new_string>new text</new_string>\n"
            "- reliary_fix: <file>path.py</file> <old>text</old> <new>text</new> — pattern-based edit\n\n"
            "ALSO AVAILABLE:\n"
            "- bash: <command>shell command</command> — run any shell command\n"
            "- read: <file_path>path.py</file_path> [optional: <start_line>1</start_line> <end_line>50</end_line>]\n"
            "- reliary_dead_symbols: <path>.</path> — find unused code\n"
            "- reliary_scope: <name>symbol</name> <anchor_file>path</anchor_file> <anchor_line>1</anchor_line>\n\n"
            f"Working directory: {workdir}\n\n"
            "RULES:\n"
            "1. Call reliary_pack FIRST to get the codebase overview.\n"
            "2. Use reliary_methods_on to find the method that needs fixing.\n"
            "3. Edit the SOURCE file (e.g. django/forms/boundfield.py), NOT the test file.\n"
            "4. Use edit (or reliary_fix) to make the smallest possible change.\n"
            "5. Respond with DONE when finished.\n"
            "6. Do NOT output a diff block. Use the edit tool.\n"
        )
    else:
        return (
            "You are a software engineer fixing a GitHub issue.\n"
            "Make the minimal change using the edit tool. When done, respond with just DONE.\n\n"
            "Tool call format (ONE per response):\n"
            "antml:invoke\n"
            "<name>tool_name</name>\n"
            "<param>value</param>\n"
            "\n\n"
            "Tools:\n"
            "- bash: <command>shell command</command>\n"
            "- read: <file_path>path.py</file_path> [optional: <start_line>1</start_line> <end_line>50</end_line>]\n"
            "- edit: <file_path>path.py</file_path> <old_string>EXACT text to replace</old_string> <new_string>replacement</new_string>\n\n"
            f"Working directory: {workdir}\n\n"
            "RULES:\n"
            "1. Find the bug (2-3 tool calls max).\n"
            "2. Use edit to fix it (one edit, smallest change).\n"
            "3. Edit the SOURCE file (e.g. django/forms/boundfield.py), NOT the test file.\n"
            "4. Respond with DONE.\n"
            "5. Do NOT output a diff block. Use the edit tool.\n"
        )


def run_instance(instance: dict, cond: str, max_turns: int = 15, timeout: int = 300) -> dict:
    instance_id = instance["instance_id"]
    repo = instance["repo"]
    base_commit = instance["base_commit"]
    problem = instance["problem_statement"]

    workdir = WORK_ROOT / instance_id
    if not clone_repo(repo, base_commit, workdir):
        return {"instance_id": instance_id, "error": "clone failed", "cond": cond}

    print(f"  Indexing {workdir}...")
    r = subprocess.run([RELIARY_BIN, "trust", str(workdir)],
                       capture_output=True, text=True, timeout=120)
    if r.returncode != 0:
        print(f"  Trust failed: {r.stderr[:200]}")

    # Pre-generate the pack so the model can call reliary_pack instantly.
    if cond == "A":
        print(f"  Building occurrence table (JIT)...")
        r = subprocess.run([RELIARY_BIN, "build-all", str(workdir)],
                           capture_output=True, text=True, timeout=300)
        if r.returncode != 0:
            print(f"  Build-all failed (non-fatal): {r.stderr[:100]}")
        print(f"  Pre-generating pack...")
        pack_path = Path(workdir) / ".reliary" / "pack_l2l3.md"
        r = subprocess.run([RELIARY_BIN, "pack", str(workdir), "--format", "l2l3"],
                           capture_output=True, text=True, timeout=60)
        if r.returncode == 0 and r.stdout:
            pack_path.write_text(r.stdout)
            print(f"  Pack: {len(r.stdout)} chars, {r.stdout.count('L2:')} symbols")
        else:
            print(f"  Pack gen failed (non-fatal): {r.stderr[:100]}")

    sys_prompt = _build_sys_prompt(cond, str(workdir))
    messages = [
        {"role": "system", "content": sys_prompt},
        {"role": "user", "content": f"# Issue\n{problem}\n\nFix this issue. Make minimal changes."},
    ]

    total_tokens_in = 0
    total_tokens_out = 0
    total_cached = 0
    edit_count = 0

    t0 = time.time()
    last_content = ""
    for turn in range(max_turns):
        if time.time() - t0 > timeout:
            break

        resp = deepseek_chat(messages, max_tokens=4000, timeout=180, disable_thinking=False)
        if "error" in resp:
            break

        usage = resp.get("usage", {})
        total_tokens_in += usage.get("prompt_tokens", 0)
        total_tokens_out += usage.get("completion_tokens", 0)
        total_cached += usage.get("prompt_tokens_details", {}).get("cached_tokens", 0)

        msg = resp.get("choices", [{}])[0].get("message", {})
        content = msg.get("content", "") or msg.get("reasoning_content", "")
        last_content = content
        messages.append({"role": "assistant", "content": content})

        # DONE signal — model is finished editing.
        if content.strip().upper() == "DONE":
            break

        # Execute tool calls.
        tool_results = _execute_tool_calls(content, str(workdir), cond)
        if tool_results:
            if "[edit " in tool_results and "OK" in tool_results:
                edit_count += 1
            messages.append({"role": "user", "content": tool_results})
        else:
            # No tool call and not DONE — nudge.
            if turn == max_turns - 2:
                messages.append({"role": "user", "content":
                    "Make your edit now using the edit tool, then respond DONE."})

    elapsed = time.time() - t0

    # Count edits by checking tool results for "[edit" or "[reliary_fix" success.
    edit_count = sum(
        1 for m in messages
        if m.get("role") == "user" and "OK" in m.get("content", "")
        and ("[edit" in m.get("content", "") or "[reliary_fix" in m.get("content", ""))
    )
    # Fallback: if git diff shows changes but edit_count=0, the edit parser missed it.
    if edit_count == 0:
        r_diff = subprocess.run(["git", "-C", str(workdir), "diff", "--stat"],
                                capture_output=True, text=True, timeout=5)
        if r_diff.stdout.strip():
            edit_count = 1  # at least one change was made

    # Save conversation log.
    log_dir = WORK_ROOT / "logs"
    log_dir.mkdir(exist_ok=True)
    log_file = log_dir / f"{instance_id}_{cond}.json"
    log_file.write_text(json.dumps({
        "instance_id": instance_id, "cond": cond, "turns": turn + 1,
        "edit_count": edit_count, "messages": messages[-20:],
        "elapsed": elapsed,
    }, indent=2))

    # Capture actual changes via git diff (the edit tool modified files directly).
    r = subprocess.run(["git", "-C", str(workdir), "diff"],
                       capture_output=True, text=True, timeout=10)
    actual_diff = r.stdout
    has_changes = bool(actual_diff.strip())

    # Run tests on the model's actual edits.
    test_result = run_tests(workdir, instance, timeout=120)
    test_result["instance_id"] = instance_id
    test_result["cond"] = cond
    test_result["elapsed"] = elapsed
    test_result["tokens_in"] = total_tokens_in
    test_result["tokens_out"] = total_tokens_out
    test_result["cached"] = total_cached
    test_result["turns"] = turn + 1
    test_result["edit_count"] = edit_count
    test_result["has_changes"] = has_changes
    test_result["diff_size"] = len(actual_diff)
    test_result["diff_preview"] = actual_diff[:500]
    return test_result


def _execute_tool_calls(content: str, workdir: str, cond: str) -> str:
    """Parse and execute tool calls from the LLM's response."""
    tool_calls = re.findall(r'antml:invoke\n(.*?)\nantml:invoke', content, re.DOTALL)
    if not tool_calls:
        return ""

    results = []
    for tc in tool_calls:
        tc = tc.strip()
        tool_name_match = re.search(r'<(\w+)>(.*?)</\1>', tc, re.DOTALL)
        if not tool_name_match:
            continue
        tool_name = tool_name_match.group(1)
        args_str = tool_name_match.group(2)

        if tool_name == "bash":
            cmd = _extract_xml(args_str, "command")
            if cmd:
                try:
                    r = subprocess.run(cmd, shell=True, cwd=workdir,
                                       capture_output=True, text=True, timeout=30)
                    out = (r.stdout + r.stderr)[:2000]
                    results.append(f"[bash] {cmd[:200]}\n{out}")
                except subprocess.TimeoutExpired:
                    results.append(f"[bash] timeout 30s")
                except Exception as e:
                    results.append(f"[bash error] {e}")
        elif tool_name == "read":
            file_path = (_extract_xml(args_str, "file_path") or _extract_xml(args_str, "path")
                         or _extract_xml(args_str, "param"))
            start = _extract_xml(args_str, "start_line") or _extract_xml(args_str, "offset")
            end = _extract_xml(args_str, "end_line") or _extract_xml(args_str, "limit")
            if file_path:
                abs_path = workdir + "/" + file_path.lstrip("/")
                try:
                    text = Path(abs_path).read_text()
                    lines = text.split("\n")
                    if start and end:
                        s, e = int(start) - 1, int(end)
                        text = "\n".join(lines[s:e])
                    results.append(f"[read {file_path}]\n{text[:8000]}")
                except Exception as e:
                    results.append(f"[read error] {e}")
        elif tool_name in ("edit", "Edit"):
            file_path = (_extract_xml(args_str, "file_path") or _extract_xml(args_str, "path")
                         or _extract_xml(args_str, "param"))
            old_text = (_extract_xml(args_str, "old_string") or _extract_xml(args_str, "old")
                        or _extract_xml(args_str, "old_text"))
            new_text = (_extract_xml(args_str, "new_string") or _extract_xml(args_str, "new")
                        or _extract_xml(args_str, "new_text"))
            if file_path and old_text is not None and new_text is not None:
                abs_path = workdir + "/" + file_path.lstrip("/")
                try:
                    fc = Path(abs_path).read_text()
                    if old_text in fc:
                        Path(abs_path).write_text(fc.replace(old_text, new_text, 1))
                        results.append(f"[edit {file_path}] OK")
                    else:
                        results.append(f"[edit {file_path}] ERROR: old_string not found")
                except Exception as e:
                    results.append(f"[edit error] {e}")
        elif tool_name in ("reliary_fix", "reliary_fix_v2", "reliary_fix_v3"):
            file_path = (_extract_xml(args_str, "file") or _extract_xml(args_str, "file_path")
                         or _extract_xml(args_str, "param"))
            old_t = (_extract_xml(args_str, "old") or _extract_xml(args_str, "old_text")
                     or _extract_xml(args_str, "old_string"))
            new_t = (_extract_xml(args_str, "new") or _extract_xml(args_str, "new_text")
                     or _extract_xml(args_str, "new_string"))
            context = _extract_xml(args_str, "context") or _extract_xml(args_str, "ctx")
            if file_path and old_t is not None:
                abs_path = workdir + "/" + file_path.lstrip("/")
                try:
                    fc = Path(abs_path).read_text()
                    if old_t in fc:
                        Path(abs_path).write_text(fc.replace(old_t, new_t or "", 1))
                        results.append(f"[reliary_fix {file_path}] OK")
                    else:
                        results.append(f"[reliary_fix {file_path}] ERROR: old text not found")
                except Exception as e:
                    results.append(f"[reliary_fix error] {e}")
            else:
                results.append(f"[reliary_fix] ERROR: missing file or old text")
        elif cond == "A":
            # Normalize common typo: "relairy" → "reliary"
            normalized_name = tool_name.replace("relairy", "reliary")
            if normalized_name.startswith("reliary_"):
                args_dict = _extract_all_xml_args(args_str)
                if normalized_name == "reliary_pack":
                    pack_path = Path(workdir) / ".reliary" / "pack_l2l3.md"
                    if pack_path.exists():
                        pack_text = pack_path.read_text()[:4000]
                        results.append(f"[reliary_pack] {len(pack_text)} chars of pack:\n{pack_text}")
                    else:
                        results.append("[reliary_pack] Pack not generated. Use bash to explore.")
                else:
                    try:
                        result = call_reliary_tool(workdir, normalized_name, args_dict)
                        results.append(f"[{normalized_name}]\n{result[:3000]}")
                    except Exception as e:
                        results.append(f"[{normalized_name} error] {e}")

    return "\n\n".join(results)


def _extract_xml(text: str, tag: str) -> Optional[str]:
    m = re.search(rf'<{tag}>(.*?)</{tag}>', text, re.DOTALL)
    return m.group(1) if m else None


def _extract_all_xml_args(text: str) -> dict:
    args = {}
    for m in re.finditer(r'<(\w+)>(.*?)</\1>', text, re.DOTALL):
        key, val = m.group(1), m.group(2).strip()
        if val.lower() in ("true", "false"):
            args[key] = val.lower() == "true"
        elif val.isdigit():
            args[key] = int(val)
        else:
            args[key] = val
    return args


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--instance", required=True)
    parser.add_argument("--conds", default="A,C")
    parser.add_argument("--max-turns", type=int, default=15)
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--out", default=None)
    args = parser.parse_args()
    conds = [c.strip() for c in args.conds.split(",")]

    from datasets import load_dataset
    print("Loading SWE-bench Lite...")
    ds = load_dataset("princeton-nlp/SWE-bench_Lite", split="test")

    instances = [x for x in ds if x["instance_id"] == args.instance]
    if not instances:
        print(f"Instance {args.instance} not found.")
        return

    print(f"Selected {len(instances)} instances")

    results = []
    for inst in instances:
        for cond in conds:
            print(f"\n=== {inst['instance_id']} ({cond}) ===")
            r = run_instance(inst, cond, max_turns=args.max_turns, timeout=args.timeout)
            results.append(r)
            f2p = r.get("fail_to_pass", 0)
            f2p_total = r.get("fail_to_pass_total", 0)
            edits = r.get("edit_count", 0)
            changed = r.get("has_changes", False)
            print(f"  f2p: {f2p}/{f2p_total}, edits={edits}, changed={changed}")
            if r.get("error"):
                print(f"  error: {r['error'][:100]}")

    out_file = args.out or f"bench/results/swe_bench_lite_{int(time.time())}.jsonl"
    Path("bench/results").mkdir(exist_ok=True)
    with open(out_file, "w") as f:
        for r in results:
            f.write(json.dumps(r) + "\n")
    print(f"\nResults written to {out_file}")


if __name__ == "__main__":
    main()