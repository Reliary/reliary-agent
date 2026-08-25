"""Run the reliary self-bench against a specific binary + corpus snapshot,
for ALL conditions (A=reliary, B=altbackend, C=grep).

Usage: python3 bench/run_snapshot_bench.py --bin /path/to/reliary --corpus /path/to/repo --conds A,B,C --out results/foo.jsonl --seeds 42 17 123 456
"""
import sys
import os
import argparse

p = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, p)

import llm_conn
import multi_turn_harness as mth
import long_session_bench

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--out", default=None)
    ap.add_argument("--seeds", nargs="+", type=int, default=[42, 17, 123, 456])
    ap.add_argument("--conds", default="A")
    ap.add_argument("--altbackend-project", default=None,
                    help="Override ALTBACKEND_PROJECT (default: derive from corpus path)")
    args = ap.parse_args()

    llm_conn.RELIARY_BIN = args.bin
    llm_conn.TOKIO_CORPUS = args.corpus
    mth.RELIARY_BIN = args.bin
    mth.TOKIO_CORPUS = args.corpus

    # V59 diag: trace every MCP session creation (binary + workdir)
    _OrigSession = mth.MCPSession
    class _TracedSession(_OrigSession):
        def __init__(self, binary, workdir, label="mcp", extra_env=None):
            print(f"[session-trace] {label}: bin={binary} wd={workdir}", file=sys.stderr, flush=True)
            super().__init__(binary, workdir, label, extra_env)
        def call(self, tool_name, arguments, timeout=60):
            out = super().call(tool_name, arguments, timeout=timeout)
            proc = getattr(self, "proc", None)
            alive = proc is not None and proc.poll() is None
            if not alive:
                code = proc.poll() if proc is not None else None
                print(f"[session-trace] exit_code={code} (neg=-signal)", file=sys.stderr, flush=True)
                err = b""
                try:
                    if proc is not None and proc.stderr:
                        err = proc.stderr.read() or b""
                except Exception:
                    pass
                tail = (err if isinstance(err, str) else err.decode(errors="replace"))[-300:]
                print(f"[session-trace] SERVER DIED during {tool_name}. stderr:\n{tail}", file=sys.stderr, flush=True)
            return out
    mth.MCPSession = _TracedSession

    # Derive altbackend project name from corpus path: /x/y/z -> x-y-z
    if args.altbackend_project:
        mth.ALTBACKEND_PROJECT = args.altbackend_project
    else:
        parts = args.corpus.strip("/").split("/")
        mth.ALTBACKEND_PROJECT = "-".join(parts).replace(".", "-")

    from reliary_bench import SESSION_QUERIES
    long_session_bench.SESSION_QUERIES = SESSION_QUERIES

    # V58d: RELIARY_GT=1 also swaps QUESTION TEXT for corpus-matched versions
    # (same IDs, answerable on this corpus). Keyword rubrics stay as smoke
    # tests; the LLM judge (RELIARY_GT=1) does real quality scoring.
    if os.environ.get("RELIARY_GT") == "1":
        from reliary_judge_gt import QUESTIONS as _RQ
        import copy
        _patched = copy.deepcopy(SESSION_QUERIES)
        for q in _patched:
            if q["id"] in _RQ:
                q["question"] = _RQ[q["id"]]
        long_session_bench.SESSION_QUERIES = _patched

    sys.argv = ["long_session_bench.py",
                "--conditions", args.conds,
                "--seeds"] + [str(s) for s in args.seeds]
    if args.out:
        sys.argv += ["--out", args.out]
    long_session_bench.main()

if __name__ == "__main__":
    main()
