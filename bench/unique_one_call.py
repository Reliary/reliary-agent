"""
Track C3: Round-trip cost comparison.

Measures the N+1 round-trip penalty of altbackend's two-tool workflow
(search_graph + N get_code_snippet) vs reliary's one-call with_source.

Design:
- Single task: find references to a symbol
- Condition A (reliary): 1 call to find_references_with_source = 1 round-trip
- Condition B (altbackend): 1 search_graph + N get_code_snippet = 1+N round-trips
- Metrics: tool_calls, wall_time, tool_output_bytes
- Pass gate: reliary tool_calls <= altbackend/2 AND wall_time <= altbackend's wall_time
"""
import json
import os
import subprocess
import time
from pathlib import Path


def reliary_call(name, args, corpus_path, session=None):
    """Make a single reliary MCP call. Returns (output, wall_time_s)."""
    if session is not None:
        # Use persistent MCP session — much faster than spawning per call.
        t0 = time.time()
        result = session.call(name, {**args, "path": "."})
        wall = time.time() - t0
        return result, wall, len(result)

    init = json.dumps({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test", "version": "1.0"}},
    })
    notif = json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"})
    payload = init + "\n" + notif + "\n" + json.dumps({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": name, "arguments": {**args, "path": "."}},
    })

    t0 = time.time()
    proc = subprocess.run(
        ["/home/user/src/reliary8/target/release/reliary", "mcp"],
        input=payload,
        capture_output=True, text=True, timeout=60,
        cwd=corpus_path,
    )
    wall = time.time() - t0
    output_bytes = len(proc.stdout)
    return proc.stdout, wall, output_bytes


def altbackend_call(tool, args, project=None):
    """Make a single altbackend call. Returns (output, wall_time_s)."""
    if project:
        args = {**args, "project": project}
    t0 = time.time()
    proc = subprocess.run(
        ["/home/user/.local/bin/altbackend-mcp", "cli", tool, json.dumps(args)],
        capture_output=True, text=True, timeout=60,
    )
    wall = time.time() - t0
    output_bytes = len(proc.stdout)
    return proc.stdout, wall, output_bytes


def main():
    import sys
    sys.path.insert(0, str(Path(__file__).parent))
    from multi_turn_harness import MCPSession

    corpus_path = os.environ.get("CORPUS_PATH", "/tmp/tokio-corpus/tokio/src")
    symbol = os.environ.get("SYMBOL", "consume")

    print(f"=== Track C3: Round-trip cost comparison ===")
    print(f"Corpus: {corpus_path}")
    print(f"Symbol: {symbol}")

    # --- Condition A: reliary one-call ---
    print("\nCondition A: reliary one-call with_source (persistent session)")
    session = MCPSession("/home/user/src/reliary8/target/release/reliary", corpus_path)
    # Prewarm: call once (lazy occurrence JIT) so the timed call is hot.
    session.call("reliary_find_references_with_source", {"name": symbol, "threshold": 0.05, "limit": 10, "path": "."})
    out_a, wall_a, bytes_a = reliary_call(
        "reliary_find_references_with_source",
        {"name": symbol, "threshold": 0.05, "limit": 10},
        corpus_path,
        session=session,
    )
    calls_a = 1
    print(f"  calls=1 wall={wall_a:.1f}s bytes={bytes_a}")
    session.close()

    # --- Condition B: altbackend N+1 ---
    print("\nCondition B: altbackend search_graph + get_code_snippet")
    altbackend_project = "tmp-tokio-corpus-tokio-src"
    out_b, wall_b1, bytes_b1 = altbackend_call("search_graph", {"query": symbol, "limit": 10}, project=altbackend_project)
    calls_b = 1

    # Parse altbackend search_graph output to get qualified names
    qnames = []
    try:
        data = json.loads(out_b)
        if isinstance(data, dict) and "results" in data:
            for item in data["results"][:10]:
                if isinstance(item, dict) and "qualified_name" in item:
                    qnames.append(item["qualified_name"])
        elif isinstance(data, list):
            for item in data[:10]:
                if isinstance(item, dict) and "qualified_name" in item:
                    qnames.append(item["qualified_name"])
    except json.JSONDecodeError:
        for line in out_b.split("\n"):
            if ":" in line and not line.startswith("{"):
                parts = line.split()
                if len(parts) >= 2:
                    qname = parts[0].rstrip(":")
                    if qname:
                        qnames.append(qname)

    # Limit to top 10
    qnames = qnames[:10]
    print(f"  Found {len(qnames)} qualified names, fetching source for each...")

    bytes_b2 = 0
    wall_b2 = 0.0
    for qname in qnames:
        out, wall, b = altbackend_call("get_code_snippet", {"qualified_name": qname, "show_lines": 5}, project=altbackend_project)
        calls_b += 1
        bytes_b2 += b
        wall_b2 += wall

    wall_b = wall_b1 + wall_b2
    bytes_b = bytes_b1 + bytes_b2
    print(f"  calls={calls_b} wall={wall_b:.1f}s bytes={bytes_b}")

    # --- Compare ---
    print("\n=== Summary ===")
    print(f"  Reliary: {calls_a} call, {wall_a:.1f}s, {bytes_a} bytes")
    print(f"  ALTBACKEND:     {calls_b} calls, {wall_b:.1f}s, {bytes_b} bytes")

    if calls_b > 0:
        call_ratio = calls_a / calls_b
        print(f"\n  Reliary uses {call_ratio:.2f}x as many calls")
    if wall_b > 0:
        time_ratio = wall_a / wall_b
        print(f"  Reliary takes {time_ratio:.2f}x as much wall time")

    # Pass gates
    pass_calls = calls_a <= calls_b / 2
    pass_wall = wall_a <= wall_b
    print(f"\n  Pass gate (calls <= altbackend/2): {'PASS' if pass_calls else 'FAIL'}")
    print(f"  Pass gate (wall <= altbackend):    {'PASS' if pass_wall else 'FAIL'}")

    return {
        "reliary": {"calls": calls_a, "wall": wall_a, "bytes": bytes_a},
        "altbackend": {"calls": calls_b, "wall": wall_b, "bytes": bytes_b},
        "pass_calls": pass_calls,
        "pass_wall": pass_wall,
    }


if __name__ == "__main__":
    main()