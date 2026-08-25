#!/usr/bin/env python3
"""bench_symbols.py — measure cluster purity of reliary's grammar-free symbol queries.

Pass criterion from PLAN_reliary_v0.8: ≥80% purity on top-20 polymorphic identifiers.

We test on the reliary8 codebase itself (self-hosting) because:
- It has real symbols with multiple distinct uses across modules
- It's the most honest test: if it works on ourselves, it'll work elsewhere
- It avoids needing an external labeled corpus

Method:
1. Index the reliary8 crate (target + crates/reliary-*).
2. Pick the top-20 stems by occurrence count that have BOTH is_def occurrences
   AND non-is_def occurrences (the "polymorphic" pattern: defined and used).
3. For each, call find_references with threshold sweep [0.0, 0.1, 0.3, 0.5].
4. Measure purity = (hits in anchor's own block + hits in similar blocks) / total hits.
   Higher purity = the tool is doing something useful (filtering by context), not
   returning every occurrence indiscriminately.

Reports median purity across the 20 symbols per threshold, plus overall.
"""
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import shutil
from pathlib import Path

HERE = Path(__file__).resolve().parent
BIN = HERE.parent / "target" / "release" / "reliary"
CORPUS = HERE.parent  # index reliary8 itself
TEST_DIR = "/tmp/reliary-symbol-bench"


def index_corpus():
    """Re-index the corpus into TEST_DIR. Returns the absolute path."""
    p = Path(TEST_DIR)
    if p.exists():
        shutil.rmtree(p)
    p.mkdir(parents=True)
    print(f"indexing {CORPUS}/crates into {TEST_DIR}/crates ...", flush=True)
    dst_src = p / "crates"
    shutil.copytree(CORPUS / "crates", dst_src)
    res = subprocess.run([str(BIN), "index", str(dst_src)], capture_output=True, text=True, timeout=120)
    if res.returncode != 0:
        print(f"index failed: {res.stderr}", file=sys.stderr)
        sys.exit(1)
    src_idx = dst_src / ".reliary" / "index.sqlite"
    dst_idx = p / ".reliary" / "index.sqlite"
    dst_idx.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(src_idx, dst_idx)
    print(f"  done; index mirrored to {dst_idx}")
    return p


def pick_polymorphic_symbols(test_dir, top_n=20):
    """Find stems that appear both as def and as non-def, ordered by total occurrences."""
    db = sqlite3.connect(test_dir / ".reliary" / "index.sqlite")
    rows = list(db.execute("""
        SELECT p.phrase,
               SUM(CASE WHEN o.is_def=1 THEN 1 ELSE 0 END) AS defs,
               SUM(CASE WHEN o.is_def=0 THEN 1 ELSE 0 END) AS refs,
               COUNT(*) AS total
        FROM occurrence o JOIN phrases p ON p.id = o.phrase_id
        GROUP BY p.phrase
        HAVING defs > 0 AND refs > 0 AND total > 5
        ORDER BY total DESC
        LIMIT ?
    """, (top_n,)))
    db.close()
    return [{"stem": r[0], "defs": r[1], "refs": r[2], "total": r[3]} for r in rows]


def mcp_call(tool_name, arguments):
    """Send a single MCP tools/call request to the reliary binary."""
    p = subprocess.Popen([str(BIN), "mcp"], cwd=str(TEST_DIR),
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    req = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": tool_name, "arguments": arguments}})
    out, err = p.communicate(req.encode(), timeout=60)
    for line in out.decode().splitlines():
        try:
            r = json.loads(line)
            if "result" in r:
                return json.loads(r["result"]["content"][0]["text"])
            elif "error" in r:
                return {"error": r["error"].get("message", str(r["error"]))}
        except Exception:
            continue
    return {"error": "no result: " + err.decode()[:200]}


def purity_for(symbol, threshold):
    """For a stem, call find_references at a representative anchor and measure purity.

    Purity definition: hits in the anchor's own block (sim=1.0) + hits in blocks
    with non-zero similarity — these are the "cluster" the LLM cares about.
    Hits with sim=0.0 from totally unrelated blocks are "noise."
    """
    # Anchor: pick the first is_def occurrence of this stem as anchor.
    db = sqlite3.connect(str(Path(TEST_DIR) / ".reliary" / "index.sqlite"))
    row = db.execute("""
        SELECT f.file_path, o.line, o.block_id
        FROM occurrence o JOIN phrases p ON p.id = o.phrase_id
        JOIN file_map f ON f.id = o.file_id
        WHERE p.phrase = ? AND o.is_def = 1
        ORDER BY o.occ_id LIMIT 1
    """, (symbol,)).fetchone()
    db.close()
    if row is None:
        return None
    file_path, line, _block_id = row
    # file_path is absolute — convert to relative for the MCP tool.
    # file_path stored in index looks like /tmp/reliary-symbol-bench/crates/<rest>.
    # MCP prepends TEST_DIR so we must send "crates/<rest>" as the anchor_file.
    _fp = Path(file_path)
    rel = str(_fp.relative_to(TEST_DIR))
    r = mcp_call("reliary_find_references", {
        "name": symbol,
        "anchor_file": rel,
        "anchor_line": int(line),
        "threshold": threshold,
        "path": ".",
    })
    if "error" in r:
        return {"symbol": symbol, "error": r["error"], "threshold": threshold}
    hits = r.get("hits", [])
    if not hits:
        return {"symbol": symbol, "count": 0, "in_cluster": 0, "purity": 0.0, "threshold": threshold}
    in_cluster = sum(1 for h in hits if h.get("similarity", 0) > 0.0)
    return {
        "symbol": symbol,
        "count": len(hits),
        "in_cluster": in_cluster,
        "purity": in_cluster / len(hits),
        "threshold": threshold,
    }


def main():
    test_dir = index_corpus()
    global TEST_DIR
    TEST_DIR = str(test_dir)
    print(f"\npicking top-20 polymorphic stems from the index...")
    symbols = pick_polymorphic_symbols(test_dir, top_n=20)
    for s in symbols[:10]:
        print(f"  {s['stem']}: defs={s['defs']} refs={s['refs']} total={s['total']}")
    print(f"  ... ({len(symbols)} total)")

    thresholds = [0.0, 0.1, 0.3, 0.5]
    results = {t: [] for t in thresholds}
    for s in symbols:
        for t in thresholds:
            r = purity_for(s["stem"], t)
            if r:
                results[t].append(r)

    print("\n== purity by threshold (median over 20 symbols) ==")
    import statistics
    for t in thresholds:
        purities = [r["purity"] for r in results[t] if "purity" in r and r["count"] > 0]
        if not purities:
            print(f"  threshold={t}: no data")
            continue
        med = statistics.median(purities)
        avg = statistics.mean(purities)
        hits = [r["count"] for r in results[t] if "count" in r]
        print(f"  threshold={t}: median_purity={med:.2f} mean_purity={avg:.2f} "
              f"avg_hits={statistics.mean(hits):.1f} (n={len(purities)})")

    # Detailed per-symbol dump at threshold=0.1 (the documented default).
    print("\n== detail @ threshold=0.1 ==")
    for r in results[0.1]:
        if "error" in r:
            print(f"  {r['symbol']:30s} ERROR: {r['error']}")
        else:
            print(f"  {r['symbol']:30s} count={r['count']:3d} in_cluster={r['in_cluster']:3d} purity={r['purity']:.2f}")


if __name__ == "__main__":
    main()