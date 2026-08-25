"""
Track C1: IR reasoning compression savings.

Measures reliary's `compress` tool on real LLM reasoning text.
ALTBACKEND has no equivalent tool — this is a unique capability.

Design:
- 3 multi-turn coding tasks
- 2 conditions: compression ON (LLM can call reliary_compress) vs OFF (no compression tool)
- 3 seeds each
- Metrics: weighted_cost, tokens_in, tokens_out, wall_time
"""
import json
import subprocess
import time
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
from multi_turn_harness import MCPSession

# ============================================================
# Tasks

TASKS = [
    {
        "id": "task_compress_consume",
        "question": "Find all types that implement `consume` on `AsyncBufRead`. List each type and its file:line.",
        "max_turns": 6,
    },
    {
        "id": "task_compress_block_on",
        "question": "Trace the call chain from `Runtime::block_on` to where a task is added to the work queue. List each hop with file:line.",
        "max_turns": 6,
    },
    {
        "id": "task_compress_bufwriter",
        "question": "When you call `write` on a `BufWriter<W>` in tokio, trace the delegation chain through `poll_write`, `flush_buf`, and the inner writer. List file:line for each step.",
        "max_turns": 6,
    },
]

# ============================================================
# System prompts

SYSTEM_PROMPT_BASE = """You are a code intelligence assistant. Answer the user's question using the provided tools.

When asked about code, use:
- `reliary_find_references_with_source` for find-references (returns file:line + source)
- `reliary_goto_def` to jump to a definition
- `reliary_callgraph` for call graphs
- `reliary_methods_on` for methods on a type
- `reliary_search` for BM25 file search
- `reliary_brace_graph` for file structure

Provide your final answer with file:line references and source code snippets."""

SYSTEM_PROMPT_COMPRESS = SYSTEM_PROMPT_BASE + """

You also have `reliary_compress` to compress verbose reasoning text before outputting it. Use it on long thinking blocks to save tokens.

IMPORTANT: After every multi-paragraph reasoning block, call `reliary_compress` on it before continuing. This reduces your token cost."""

# ============================================================
# Tool definitions

RELIARY_TOOLS = [
    {
        "name": "reliary_find_references_with_source",
        "description": "Find references to a symbol with inline source",
        "parameters": {
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "anchor_file": {"type": "string", "default": ""},
                "anchor_line": {"type": "integer", "default": 0},
                "path": {"type": "string"},
                "threshold": {"type": "number", "default": 0.05},
                "limit": {"type": "integer", "default": 10},
            },
            "required": ["name", "path"],
        },
    },
    {
        "name": "reliary_goto_def",
        "description": "Jump to a definition",
        "parameters": {
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "anchor_file": {"type": "string", "default": ""},
                "anchor_line": {"type": "integer", "default": 0},
                "path": {"type": "string"},
            },
            "required": ["name", "path"],
        },
    },
    {
        "name": "reliary_callgraph",
        "description": "Call graph for a function",
        "parameters": {
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "anchor_file": {"type": "string", "default": ""},
                "anchor_line": {"type": "integer", "default": 0},
                "path": {"type": "string"},
            },
            "required": ["name", "path"],
        },
    },
    {
        "name": "reliary_methods_on",
        "description": "Methods on a type",
        "parameters": {
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "path": {"type": "string"},
            },
            "required": ["name", "path"],
        },
    },
    {
        "name": "reliary_search",
        "description": "BM25 file search",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "path": {"type": "string"},
            },
            "required": ["query", "path"],
        },
    },
    {
        "name": "reliary_compress",
        "description": "Compress verbose reasoning text. Strips filler phrases and merges redundant thinking.",
        "parameters": {
            "type": "object",
            "properties": {
                "text": {"type": "string"},
            },
            "required": ["text"],
        },
    },
]

RELIARY_TOOLS_NO_COMPRESS = [t for t in RELIARY_TOOLS if t["name"] != "reliary_compress"]

# ============================================================
# DeepSeek client

API_URL = "https://api.deepseek.com/v1/chat/completions"
MODEL = "deepseek-v4-flash"

def call_deepseek(messages, tools, max_tokens=2000):
    """Call DeepSeek API with thinking disabled."""
    import os
    api_key = None
    auth_file = Path("/home/user/.local/share/opencode/auth.json")
    if auth_file.exists():
        with open(auth_file) as f:
            data = json.load(f)
            # Try multiple key formats
            api_key = (data.get("deepseek", {}).get("apiKey")
                       or data.get("deepseek", {}).get("key")
                       or data.get("apiKey")
                       or data.get("key"))
            if not api_key:
                # Search all top-level keys for 'sk-' prefix
                for v in data.values():
                    if isinstance(v, dict) and (v.get("key", "")).startswith("sk-"):
                        api_key = v["key"]
                        break

    if not api_key:
        # Try env var
        api_key = os.environ.get("DEEPSEEK_API_KEY")

    if not api_key:
        raise RuntimeError("No DeepSeek API key found")

    payload = {
        "model": MODEL,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": False,
        "extra_body": {"thinking": {"type": "disabled"}},
    }
    if tools:
        payload["tools"] = [{"type": "function", "function": t} for t in tools]
        payload["tool_choice"] = "auto"

    import urllib.request
    req = urllib.request.Request(
        API_URL,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {api_key}",
        },
    )
    with urllib.request.urlopen(req, timeout=120) as resp:
        result = json.loads(resp.read().decode("utf-8"))
    return result


def run_single_task(task, cond, seed, corpus_path, reliary_session):
    """Run one task in one condition. Returns dict with metrics."""
    system_prompt = SYSTEM_PROMPT_COMPRESS if cond == "on" else SYSTEM_PROMPT_BASE
    tools = RELIARY_TOOLS if cond == "on" else RELIARY_TOOLS_NO_COMPRESS

    messages = [
        {"role": "system", "content": system_prompt},
        {"role": "user", "content": task["question"]},
    ]

    t0 = time.time()
    tool_calls_total = 0
    turns = 0
    final_answer = ""
    compressed_texts = []

    while turns < task["max_turns"]:
        turns += 1
        resp = call_deepseek(messages, tools)
        choice = resp.get("choices", [{}])[0]
        msg = choice.get("message", {})
        usage = resp.get("usage", {})
        tokens_in = usage.get("prompt_tokens", 0)
        tokens_out = usage.get("completion_tokens", 0)

        # Handle tool calls
        tool_calls = msg.get("tool_calls", [])
        if tool_calls:
            # Append assistant message
            messages.append(msg)

            for tc in tool_calls:
                tool_name = tc.get("function", {}).get("name", "")
                tool_args_raw = tc.get("function", {}).get("arguments", "{}")
                try:
                    tool_args = json.loads(tool_args_raw) if isinstance(tool_args_raw, str) else tool_args_raw
                except json.JSONDecodeError:
                    tool_args = {}

                tool_calls_total += 1

                # Dispatch tool
                tool_result = reliary_session.call(tool_name, {**tool_args, "path": corpus_path})

                messages.append({
                    "role": "tool",
                    "tool_call_id": tc.get("id", ""),
                    "content": tool_result,
                })

                # If reliary_compress was called, record the compressed text length
                if tool_name == "reliary_compress":
                    text = tool_args.get("text", "")
                    compressed_texts.append({
                        "input_len": len(text),
                        "output_len": len(tool_result),
                    })
        else:
            # Final answer
            final_answer = msg.get("content", "")
            messages.append({"role": "assistant", "content": final_answer})
            break

    wall_time = time.time() - t0
    weighted_cost = tokens_in + 4 * tokens_out  # output costs 4x

    return {
        "task_id": task["id"],
        "cond": cond,
        "seed": seed,
        "turns": turns,
        "tool_calls": tool_calls_total,
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
        "weighted_cost": weighted_cost,
        "wall_time": wall_time,
        "final_answer": final_answer,
        "compressed_count": len(compressed_texts),
        "total_compressed_savings": sum(
            (c["input_len"] - c["output_len"]) for c in compressed_texts
        ),
    }


def main():
    import os
    corpus_path = os.environ.get("CORPUS_PATH", "/tmp/tokio-corpus/tokio/src")
    seeds = [int(s) for s in sys.argv[1:]] if len(sys.argv) > 1 else [42, 123, 789]

    print(f"=== Track C1: IR Reasoning Compression ===")
    print(f"Corpus: {corpus_path}")
    print(f"Seeds: {seeds}")

    results = []
    session = MCPSession("/home/user/src/reliary8/target/release/reliary", corpus_path)
    for task in TASKS:
        for cond in ["on", "off"]:
            for seed in seeds:
                print(f"\n{task['id']} cond={cond} seed={seed}...")
                r = run_single_task(task, cond, seed, corpus_path, session)
                results.append(r)
                print(f"  turns={r['turns']} calls={r['tool_calls']} wc={r['weighted_cost']} "
                      f"compressed={r['compressed_count']}")
    session.close()

    # Summary
    on_results = [r for r in results if r["cond"] == "on"]
    off_results = [r for r in results if r["cond"] == "off"]

    on_wc = [r["weighted_cost"] for r in on_results]
    off_wc = [r["weighted_cost"] for r in off_results]

    print("\n=== Summary ===")
    print(f"Compression ON:  median WC = {sorted(on_wc)[len(on_wc)//2]} mean = {sum(on_wc)//len(on_wc)}")
    print(f"Compression OFF: median WC = {sorted(off_wc)[len(off_wc)//2]} mean = {sum(off_wc)//len(off_wc)}")
    if on_wc and off_wc:
        on_med = sorted(on_wc)[len(on_wc)//2]
        off_med = sorted(off_wc)[len(off_wc)//2]
        if off_med > 0:
            reduction = (off_med - on_med) / off_med * 100
            print(f"WC reduction: {reduction:.1f}%")

    # Output JSONL
    out_file = Path(__file__).parent / "results" / "unique_compression.jsonl"
    out_file.parent.mkdir(exist_ok=True)
    with open(out_file, "w") as f:
        for r in results:
            f.write(json.dumps(r) + "\n")
    print(f"\nResults written to {out_file}")


if __name__ == "__main__":
    main()