"""Arc 29 — Tool-selection bench.

LLM sees the actual MCP tools/list and must pick which tool to call.
Measures: does the LLM pick the right tool? Does it succeed?

This is what real agents do — they see the tool list and decide.
"""
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from llm_conn import deepseek_chat, TOKIO_CORPUS

ALTBACKEND_BIN = "/home/user/.local/bin/altbackend-mcp"
TOKIO_PROJECT = "tmp-tokio-corpus-tokio-src"


def get_reliary_tools():
    """Get the actual MCP tools/list response from reliary."""
    proc = subprocess.run(
        ["/home/user/src/reliary8/target/release/reliary", "mcp"],
        input=b'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}\n{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}\n',
        capture_output=True, timeout=30)
    for line in proc.stdout.decode().split("\n"):
        if line.startswith("{"):
            try:
                r = json.loads(line)
                if "result" in r and "tools" in r.get("result", {}):
                    return r["result"]["tools"]
            except Exception:
                pass
    return []


def get_altbackend_tools():
    """Get altbackend's actual MCP tools/list response."""
    proc = subprocess.run(
        [ALTBACKEND_BIN],
        input=b'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}\n{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}\n',
        capture_output=True, timeout=30)
    for line in proc.stdout.decode().split("\n"):
        if line.startswith("{"):
            try:
                r = json.loads(line)
                if "result" in r and "tools" in r.get("result", {}):
                    return r["result"]["tools"]
            except Exception:
                pass
    return []


def get_grep_tools():
    """Simulate grep tool list (just bash)."""
    return [
        {"name": "bash",
         "description": "Run shell commands. Example: grep -rEn 'pattern' path --include=*.rs"}]


def call_reliary_tool(name, args):
    """Call a reliary MCP tool."""
    proc = subprocess.run(
        ["/home/user/src/reliary8/target/release/reliary", "mcp"],
        input=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                           "params": {"protocolVersion": "2024-11-05",
                                      "capabilities": {},
                                      "clientInfo": {"name": "t", "version": "1"}}}).encode()
             + b"\n"
             + json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                            "params": {"name": name, "arguments": args}}).encode()
             + b"\n",
        capture_output=True, cwd=TOKIO_CORPUS, timeout=60)
    for line in proc.stdout.decode().split("\n"):
        if line.startswith("{"):
            try:
                r = json.loads(line)
                if "result" in r and "content" in r.get("result", {}):
                    content = r["result"]["content"][0].get("text", "")
                    return json.loads(content)
            except Exception:
                pass
    return {}


def call_altbackend_tool(name, args):
    """Call a altbackend CLI tool."""
    r = subprocess.run([ALTBACKEND_BIN, "cli", name, json.dumps(args)],
                        capture_output=True, text=True, timeout=30)
    try:
        return json.loads(r.stdout)
    except Exception:
        return {}


def strict_grep_oracle(stem):
    """Oracle: all file:line where stem appears as a word (filtered)."""
    r = subprocess.run(
        ["grep", "-rEn", rf"\b{stem}\b", TOKIO_CORPUS, "--include=*.rs"],
        capture_output=True, text=True, timeout=15)
    refs = []
    for line in r.stdout.split("\n"):
        parts = line.split(":", 2)
        if len(parts) < 3:
            continue
        try:
            ln = int(parts[1])
        except ValueError:
            continue
        file_path = parts[0]
        if TOKIO_CORPUS in file_path:
            file_path = file_path[len(TOKIO_CORPUS):].lstrip("/")
        line_text = parts[2]
        if "/tests/" in file_path or "_test.rs" in file_path:
            continue
        if file_path.endswith("tests.rs"):
            continue
        s = line_text.strip()
        if s.startswith("///") or s.startswith("//!") or s.startswith("//"):
            continue
        refs.append(f"{file_path}:{ln}")
    seen = set()
    return [r for r in refs if not (r in seen or seen.add(r))]


def parse_llm_json(content):
    content = (content or "").strip()
    if not content:
        return None
    content = re.sub(r"^```(?:json)?\s*", "", content)
    content = re.sub(r"\s*```\s*$", "", content)
    try:
        return json.loads(content)
    except Exception:
        pass
    for m in re.finditer(r"\{[\s\S]*?\}", content):
        try:
            return json.loads(m.group(0))
        except Exception:
            continue
    return None


def run_question(tools_desc, question, model="deepseek-chat"):
    """LLM picks a tool, calls it (we do the call), returns result."""
    sys = (
        f"You are a code analyst. You have these tools:\n\n{tools_desc}\n\n"
        f"Pick the BEST tool for the question. Tell me which tool to call "
        f"and with what arguments. Output ONLY a JSON line:\n"
        f'{{"tool": "<tool_name>", "arguments": {{...}}}}\n'
    )
    user = f"Question: {question['question']}\n\nOutput ONLY the JSON line."
    t0 = time.time()
    resp = deepseek_chat(
        [{"role": "system", "content": sys},
         {"role": "user", "content": user}],
        model=model, max_tokens=300, timeout=30)
    elapsed = time.time() - t0
    if "error" in resp:
        return {"elapsed": elapsed, "error": resp["error"], "tool_picked": None,
                "tool_args": None, "tool_result": None, "predictions": [],
                "jaccard": 0, "tokens_in": 0, "tokens_out": 0}
    msg = resp["choices"][0]["message"]
    content = msg.get("content") or msg.get("reasoning_content", "")
    usage = resp.get("usage", {})
    pt = usage.get("prompt_tokens", 0)
    ct = usage.get("completion_tokens", 0)
    parsed = parse_llm_json(content)
    if not parsed:
        return {"elapsed": elapsed, "content": content, "tool_picked": None,
                "tool_args": None, "tool_result": None, "predictions": [],
                "jaccard": 0, "tokens_in": pt, "tokens_out": ct,
                "weighted_cost": pt + 4*ct}
    tool = parsed.get("tool")
    args = parsed.get("arguments", {})
    return {"elapsed": elapsed, "content": content, "tool_picked": tool,
            "tool_args": args, "predictions": [], "jaccard": 0,
            "tokens_in": pt, "tokens_out": ct,
            "weighted_cost": pt + 4*ct, "tool_result": None}


def main():
    # Get actual tools
    print("Getting reliary tools...")
    reliary_tools = get_reliary_tools()
    print(f"  {len(reliary_tools)} tools")
    print("Getting altbackend tools...")
    altbackend_tools = get_altbackend_tools()
    print(f"  {len(altbackend_tools)} tools")

    # Build tool descriptions as the LLM would see them
    reliary_desc = "\n".join(f"- {t['name']}: {t['description'][:200]}"
                              for t in reliary_tools)
    altbackend_desc = "\n".join(f"- {t['name']}: {t['description'][:200]}"
                          for t in altbackend_tools)
    grep_desc = "\n".join(f"- {t['name']}: {t['description']}"
                           for t in get_grep_tools())

    # Build test questions
    questions = [
        {"id": "hom-009", "stem": "consume",
         "question": f"Find all references to the method `consume` defined at `io/util/take.rs:121` in {TOKIO_CORPUS}. The anchor is a method_call. Return file:line references."},
        {"id": "hom-014", "stem": "split",
         "question": f"Find all references to the method `split` in {TOKIO_CORPUS}. Return file:line."},
        {"id": "hom-015", "stem": "kill",
         "question": f"Find all references to the method `kill` in {TOKIO_CORPUS}. Return file:line."},
    ]

    for q in questions:
        q["ground_truth"] = strict_grep_oracle(q["stem"])

    print(f"\nQuestions: {len(questions)}")
    print(f"GT sizes: {[len(q['ground_truth']) for q in questions]}\n")

    out_path = Path("/home/user/src/reliary8/bench/results/tool_selection.jsonl")
    out_f = open(out_path, "w")

    for q in questions:
        for cond_name, tools_desc in [("A (reliary)", reliary_desc),
                                       ("B (altbackend)", altbackend_desc),
                                       ("C (grep)", grep_desc)]:
            run = run_question(tools_desc, q)
            picked = run.get("tool_picked")
            print(f"  {q['id']} {cond_name}: tool={picked}")
            run.update({"task_id": q["id"], "stem": q["stem"], "condition": cond_name[0],
                        "gt_size": len(q["ground_truth"])})
            out_f.write(json.dumps(run) + "\n")
            out_f.flush()
    out_f.close()
    print(f"\nDone. Output: {out_path}")


if __name__ == "__main__":
    main()