"""Measure altbackend's actual end-to-end cost when used with full round-trip workflow.

This simulates what a real LLM agent does:
1. Call search_graph to find candidates (qualified_name, file_path, start_line)
2. For each top-N candidate, call get_code_snippet to read the actual code
3. Synthesize the final answer

Measures: total wall time, total altbackend bytes returned, prompt equivalents.
"""
import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

RESULTS_DIR = Path("/home/user/src/reliary8/bench/results")
ALTBACKEND_BIN = "/home/user/.local/bin/altbackend-mcp"


def run_altbackend_cli(tool, args, timeout=30):
    r = subprocess.run([ALTBACKEND_BIN, "cli", tool, json.dumps(args)],
                        capture_output=True, text=True, timeout=timeout)
    try:
        return json.loads(r.stdout)
    except Exception:
        return {"error": r.stdout[-300:]}


def altbackend_search_with_snippets(stem, top_n=10):
    """Full altbackend workflow: search_graph + per-hit get_code_snippet."""
    t0 = time.time()
    # Step 1: search_graph
    search = run_altbackend_cli("search_graph", {
        "project": "tmp-tokio-corpus-tokio-src",
        "query": stem,
        "limit": top_n,
    })
    search_time = time.time() - t0

    if "results" not in search:
        return {
            "search_time": search_time, "snippet_time": 0, "total_time": search_time,
            "search_bytes": len(json.dumps(search)),
            "snippet_bytes": 0, "total_bytes": len(json.dumps(search)),
            "snippet_calls": 0, "hits": [],
        }

    hits = search["results"][:top_n]
    search_bytes = len(json.dumps(search))

    # Step 2: get_code_snippet per hit
    t1 = time.time()
    snippets = []
    for h in hits:
        qn = h.get("qualified_name", "")
        if qn:
            snip = run_altbackend_cli("get_code_snippet", {
                "project": "tmp-tokio-corpus-tokio-src",
                "qualified_name": qn,
            })
            snippets.append({
                "name": h.get("name", ""),
                "qualified_name": qn,
                "file_path": h.get("file_path", ""),
                "start_line": h.get("start_line", 0),
                "snippet": snip,
            })
    snippet_time = time.time() - t1
    snippet_bytes = sum(len(json.dumps(s)) for s in snippets)

    return {
        "search_time": search_time,
        "snippet_time": snippet_time,
        "total_time": time.time() - t0,
        "search_bytes": search_bytes,
        "snippet_bytes": snippet_bytes,
        "total_bytes": search_bytes + snippet_bytes,
        "snippet_calls": len(snippets),
        "hits": snippets,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--top-n", type=int, default=10,
                        help="Number of hits to fetch snippets for")
    parser.add_argument("--stems", nargs="+",
                        default=["consume", "split", "kill", "send", "default",
                                  "from_std", "spawn"],
                        help="Stems to test")
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    if args.out is None:
        ts = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        out_path = RESULTS_DIR / f"altbackend_roundtrip_{args.top_n}_{ts}.jsonl"
    else:
        out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    print(f"=== ALTBACKEND Round-Trip Cost Measurement ===")
    print(f"Top-N: {args.top_n}")
    print(f"Stems: {args.stems}")
    print(f"Output: {out_path}\n")

    rows = []
    for stem in args.stems:
        print(f"  {stem:14s} ... ", end="", flush=True)
        r = altbackend_search_with_snippets(stem, args.top_n)
        print(f"total={r['total_time']:.2f}s search={r['search_time']:.2f}s "
              f"snippet={r['snippet_time']:.2f}s ({r['snippet_calls']} calls) "
              f"bytes={r['total_bytes']}")
        r["stem"] = stem
        rows.append(r)
        with open(out_path, "w") as f:
            f.write(json.dumps(r) + "\n")

    # Aggregate
    import statistics
    print(f"\n=== Aggregate (top-N={args.top_n}) ===\n")
    print(f"{'metric':<20} {'median':<10} {'mean':<10} {'min':<8} {'max':<8}")
    print("-" * 60)
    for label, key in [
        ("total_time_sec", "total_time"),
        ("search_time_sec", "search_time"),
        ("snippet_time_sec", "snippet_time"),
        ("total_bytes", "total_bytes"),
        ("search_bytes", "search_bytes"),
        ("snippet_bytes", "snippet_bytes"),
        ("snippet_calls", "snippet_calls"),
    ]:
        vals = [r[key] for r in rows]
        print(f"{label:<20} {statistics.median(vals):<10.2f} "
              f"{statistics.mean(vals):<10.2f} {min(vals):<8.0f} {max(vals):<8.0f}")

    print(f"\nDone. Output: {out_path}")


if __name__ == "__main__":
    main()