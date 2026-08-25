#!/usr/bin/env python3
"""Quick test for reliary_query_ast MCP tool."""
import json
import subprocess
import sys

bin_path = '/home/user/src/reliary8/target/release/reliary'
workdir = '/tmp/tokio-corpus/tokio/src'

queries = [
    ('Call(_, ?args)', '/tmp/tokio-corpus/tokio/src/runtime/context.rs', 5),
    ('Access(_, ?field)', '/tmp/tokio-corpus/tokio/src/runtime/context.rs', 5),
    ('BinaryOp(?op, ?a, ?b)', '/tmp/tokio-corpus/tokio/src/sync/mpsc/bounded.rs', 5),
]

for pattern, file, max_results in queries:
    req = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call',
                      'params': {'name': 'reliary_query_ast',
                                 'arguments': {'pattern': pattern, 'file': file, 'max_results': max_results}}})
    print(f"\n--- Pattern: {pattern} on {file.split('/')[-1]} ---")
    try:
        proc = subprocess.Popen([bin_path, 'mcp'], cwd=workdir,
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        out, _ = proc.communicate(req.encode(), timeout=30)
        for line in out.decode().splitlines():
            try:
                r = json.loads(line)
                if 'result' in r:
                    res = json.loads(r['result']['content'][0]['text'])
                    print(f"  Total: {res['total']}, Returned: {res['returned']}")
                    for m in res['matches'][:3]:
                        print(f"  L{m['line']:>4}: {m['expr'][:70]}")
                    break
            except Exception:
                pass
    except subprocess.TimeoutExpired:
        print("  TIMEOUT")
