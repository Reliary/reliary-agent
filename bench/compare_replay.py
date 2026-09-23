#!/usr/bin/env python3
"""Compare a cassette replay against its recorded run.

Reports, per (condition, seed): whether every scoring/cost metric and every
answer matches, and what differs. Timing fields are excluded by design —
replay does no network I/O, so wall/API latency legitimately differs.

Usage:
    python3 bench/compare_replay.py bench/cassettes/canonical-v1/record.jsonl \
                                       bench/results/canonical_replay.jsonl
"""
import json
import sys

TIMING_FIELDS = {
    "total_wall_time", "total_api_ms", "total_tool_ms", "total_overhead_ms",
}
QUERY_TIMING_FIELDS = {"api_ms", "tool_ms"}


def load(path):
    runs = {}
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            r = json.loads(line)
            runs[(r.get("cond"), r.get("seed"))] = r
    return runs


def query_diffs(a, b):
    """Compare two session rows, ignoring timing."""
    diffs = []
    for k in sorted(set(a) | set(b)):
        if k in TIMING_FIELDS or k == "cassette":
            continue
        if k == "queries":
            continue
        if a.get(k) != b.get(k):
            diffs.append(f"{k}: {a.get(k)!r} -> {b.get(k)!r}")
    qa = a.get("queries") or []
    qb = b.get("queries") or []
    if len(qa) != len(qb):
        diffs.append(f"query count: {len(qa)} -> {len(qb)}")
        return diffs
    for i, (x, y) in enumerate(zip(qa, qb)):
        for k in sorted(set(x) | set(y)):
            if k == "turns_detail":
                continue
            if x.get(k) != y.get(k):
                diffs.append(
                    f"queries[{i}].{k}: {str(x.get(k))[:60]!r} -> "
                    f"{str(y.get(k))[:60]!r}")
        # turns_detail: tool sequence and tool identity must match.
        ta = [{k: v for k, v in t.items() if k not in QUERY_TIMING_FIELDS}
              for t in (x.get("turns_detail") or [])]
        tb = [{k: v for k, v in t.items() if k not in QUERY_TIMING_FIELDS}
              for t in (y.get("turns_detail") or [])]
        if ta != tb:
            diffs.append(f"queries[{i}].turns_detail (tool sequence) differs")
    return diffs


def main():
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    rec, rep = load(sys.argv[1]), load(sys.argv[2])
    # Only compare conditions the replay actually ran. A replay is allowed to
    # cover a subset (the shipped script defaults to A,C because B is
    # best-effort); flagging an unrun condition as "missing" would report a
    # false failure for a deliberate scope choice.
    replay_conds = {k[0] for k in rep}
    keys = sorted((k for k in set(rec) | set(rep) if k[0] in replay_conds),
                  key=lambda k: (str(k[0]), k[1] or 0))
    ok = True
    recorded_wall = replay_wall = 0.0
    for k in keys:
        a, b = rec.get(k), rep.get(k)
        if b is None:
            print(f"{k[0]}/{k[1]}: MISSING in replay")
            ok = False
            continue
        if a is None:
            print(f"{k[0]}/{k[1]}: extra run in replay (not recorded)")
            ok = False
            continue
        if "error" in b:
            print(f"{k[0]}/{k[1]}: REPLAY ERROR: {b['error'][:160]}")
            ok = False
            continue
        diffs = query_diffs(a, b)
        recorded_wall += a.get("total_wall_time", 0.0)
        replay_wall += b.get("total_wall_time", 0.0)
        if diffs:
            ok = False
            print(f"{k[0]}/{k[1]}: DIFFERS")
            for d in diffs[:6]:
                print(f"    {d}")
        else:
            print(f"{k[0]}/{k[1]}: identical "
                  f"(score {b['total_score']}, answers {len(b['queries'])}/{len(b['queries'])})")
    print()
    print(f"recorded wall: {recorded_wall:.1f}s | replay wall: {replay_wall:.2f}s")
    print("VERDICT:", "byte-identical (excl. timing)" if ok else "DIFFERENCES FOUND")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
