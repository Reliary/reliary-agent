#!/usr/bin/env python3
"""bench_homonyms_discover.py — emit candidate homonyms from a tokio-class corpus.

Indexes /tmp/tokio-corpus/tokio/src (or the path given) and emits the top stems by
"defined in many distinct blocks." These are the candidates that have the highest
chance of being true homonyms — the same name used as semantically different things
in different scopes. Output: bench/fixtures/homonym_candidates.json.

The user then eyeballs the candidates and picks 50 to label manually.
"""
import json
import os
import sqlite3
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIX = HERE / "fixtures"
DEFAULT_CORPUS = Path("/tmp/tokio-corpus/tokio/src")
BIN = HERE.parent / "target" / "release" / "reliary"


def index_corpus(corpus: Path) -> None:
    print(f"indexing {corpus} ...", flush=True)
    res = subprocess.run([str(BIN), "index", str(corpus)], capture_output=True, text=True, timeout=300)
    if res.returncode != 0:
        print(f"FAIL: index errored: {res.stderr}", file=sys.stderr)
        sys.exit(1)
    # Index goes inside corpus/.reliary/index.sqlite.
    idx = corpus / ".reliary" / "index.sqlite"
    if not idx.exists():
        print(f"FAIL: expected index at {idx}", file=sys.stderr)
        sys.exit(1)
    print(f"  done ({idx.stat().st_size // 1024} KB)")


def _context_diversity_score(corpus: Path, phrase_id: int) -> float:
    """Heuristic for 'true homonym': how many DISTINCT block-bag clusters does
    this stem appear in? Higher score = more semantically diverse uses.

    Computed as: average pairwise cosine distance between random pairs of
    blocks containing the stem, normalized to [0, 1].
    """
    db = sqlite3.connect(corpus / ".reliary" / "index.sqlite")
    # Get the block_ids that contain at least one occurrence of this phrase.
    blocks = [r[0] for r in db.execute(
        "SELECT DISTINCT block_id FROM occurrence WHERE phrase_id = ?", (phrase_id,)
    ).fetchall()]
    if len(blocks) < 2:
        return 0.0
    # For each block, compute its bag (phrase_id -> count). Use sqlite aggregate.
    bags: list[dict[int, int]] = []
    for bid in blocks[:50]:  # cap to keep it fast
        bag_rows = db.execute(
            "SELECT phrase_id, COUNT(*) FROM occurrence WHERE block_id = ? GROUP BY phrase_id",
            (bid,),
        ).fetchall()
        bags.append({p: c for p, c in bag_rows})
    if len(bags) < 2:
        db.close()
        return 0.0
    # Compute average pairwise cosine distance (1 - cosine).
    import math
    total_dist = 0.0
    n_pairs = 0
    for i in range(min(20, len(bags))):
        for j in range(i + 1, min(20, len(bags))):
            a, b = bags[i], bags[j]
            if not a or not b:
                continue
            dot = sum(a.get(k, 0) * b.get(k, 0) for k in set(a) | set(b))
            na = math.sqrt(sum(v * v for v in a.values()))
            nb = math.sqrt(sum(v * v for v in b.values()))
            if na and nb:
                cos = dot / (na * nb)
                total_dist += (1.0 - cos)
                n_pairs += 1
    db.close()
    return total_dist / max(n_pairs, 1)


def discover(corpus: Path, top_n: int = 100) -> list:
    """Find real homonyms by parsing the source.

    The `is_def` flag in the indexer is too coarse (flags any line starting with
    `pub`/`fn`/etc., so modifiers like `pub`/`mut`/`self` get counted). For
    candidate discovery we want actual *names* of definitions — the identifier
    immediately after `fn`/`struct`/`enum`/`trait`/`impl`/`type`. We use ripgrep
    to extract these directly from the corpus, then look up which of them are
    defined in >= 3 distinct blocks via the index.
    """
    import re
    import subprocess

    # Patterns: the regex captures the NAME being defined.
    # fn NAME | struct NAME | enum NAME | trait NAME | impl NAME | type NAME | mod NAME
    # (method-like `fn` requires the second pattern; for now we use the simple case)
    name_re = re.compile(
        r'\b(?:pub(?:\s*\([^)]*\))?\s+)?(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?'
        r'(?:fn|struct|enum|trait|impl(?:\s+\w+(?:\s+for\s+\w+)?)?|type|mod)\s+'
        r'([A-Za-z_][A-Za-z0-9_]*)'
    )

    # Run ripgrep across the corpus for all .rs files.
    rg = subprocess.run(
        ["rg", "-t", "rust", "-oN", name_re.pattern, "--no-filename", str(corpus)],
        capture_output=True, text=True, timeout=120
    )
    if rg.returncode not in (0, 1):  # 0=matches, 1=no matches (acceptable)
        # ripgrep not available — fall back to a Python walk.
        return _discover_python_walk(corpus, name_re, top_n)
    # rg output is "NAME" lines (because of -o), one per match.
    name_counts: dict[str, int] = {}
    for line in rg.stdout.splitlines():
        for m in re.finditer(name_re.pattern, line):
            # rg -o emits the WHOLE match with -N; we want the captured group only.
            mm = name_re.search(line)
            if mm:
                name_counts[mm.group(1)] = name_counts.get(mm.group(1), 0) + 1

    # Cross-reference: for each top-counted name, look up which (file, line, block)
    # it appears at in the index, and count distinct blocks/files.
    db = sqlite3.connect(corpus / ".reliary" / "index.sqlite")
    rows = db.execute("SELECT id, phrase FROM phrases").fetchall()
    phrase_to_id = {p: i for i, p in rows}

    candidates = []
    # Sort by definition count descending, take top_n.
    sorted_names = sorted(name_counts.items(), key=lambda kv: -kv[1])[:top_n * 3]
    for name, def_count in sorted_names:
        if name not in phrase_to_id:
            continue
        pid = phrase_to_id[name]
        info = db.execute("""
            SELECT COUNT(DISTINCT block_id), COUNT(DISTINCT file_id), COUNT(*)
            FROM occurrence WHERE phrase_id = ? AND is_def = 1
        """, (pid,)).fetchone()
        blocks, files, occs = info
        if blocks < 3:
            continue
        samples = db.execute("""
            SELECT f.file_path, o.line, o.col
            FROM occurrence o JOIN file_map f ON f.id = o.file_id
            WHERE o.phrase_id = ? AND o.is_def = 1
            ORDER BY o.occ_id LIMIT 3
        """, (pid,)).fetchall()
        candidates.append({
            "stem": name,
            "def_count_in_source": def_count,
            "def_blocks": blocks,
            "def_files": files,
            "total_def_occs": occs,
            "sample_locations": [
                {"file": s[0], "line": s[1], "col": s[2]} for s in samples
            ],
            "context_diversity": round(_context_diversity_score(corpus, pid), 3),
        })
        if len(candidates) >= top_n:
            break

    db.close()
    return candidates


def _discover_python_walk(corpus: Path, name_re: re.Pattern, top_n: int) -> list:
    """Fallback discovery without ripgrep. Slower."""
    counts: dict[str, int] = {}
    for path in corpus.rglob("*.rs"):
        try:
            text = path.read_text(errors="ignore")
        except Exception:
            continue
        for m in name_re.finditer(text):
            counts[m.group(1)] = counts.get(m.group(1), 0) + 1
    # Re-run the cross-reference logic.
    return discover_with_counts(corpus, counts, top_n)


def discover_with_counts(corpus: Path, counts: dict[str, int], top_n: int) -> list:
    db = sqlite3.connect(corpus / ".reliary" / "index.sqlite")
    rows = db.execute("SELECT id, phrase FROM phrases").fetchall()
    phrase_to_id = {p: i for i, p in rows}
    candidates = []
    sorted_names = sorted(counts.items(), key=lambda kv: -kv[1])[:top_n * 3]
    for name, def_count in sorted_names:
        if name not in phrase_to_id:
            continue
        pid = phrase_to_id[name]
        info = db.execute("""
            SELECT COUNT(DISTINCT block_id), COUNT(DISTINCT file_id), COUNT(*)
            FROM occurrence WHERE phrase_id = ? AND is_def = 1
        """, (pid,)).fetchone()
        blocks, files, occs = info
        if blocks < 3:
            continue
        samples = db.execute("""
            SELECT f.file_path, o.line, o.col
            FROM occurrence o JOIN file_map f ON f.id = o.file_id
            WHERE o.phrase_id = ? AND o.is_def = 1
            ORDER BY o.occ_id LIMIT 3
        """, (pid,)).fetchall()
        candidates.append({
            "stem": name,
            "def_count_in_source": def_count,
            "def_blocks": blocks,
            "def_files": files,
            "total_def_occs": occs,
            "sample_locations": [
                {"file": s[0], "line": s[1], "col": s[2]} for s in samples
            ],
            "context_diversity": round(_context_diversity_score(corpus, pid), 3),
        })
        if len(candidates) >= top_n:
            break
    db.close()
    return candidates


def main():
    import argparse
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--corpus", default=str(DEFAULT_CORPUS), help="corpus path to index")
    ap.add_argument("--top", type=int, default=100, help="top N candidates to emit")
    ap.add_argument("--out", default=str(FIX / "homonym_candidates.json"), help="output JSON path")
    args = ap.parse_args()

    corpus = Path(args.corpus)
    FIX.mkdir(parents=True, exist_ok=True)
    index_corpus(corpus)

    print(f"discovering homonym candidates from {corpus} ...", flush=True)
    cands = discover(corpus, top_n=args.top)
    out = {
        "corpus": str(corpus),
        "generated_at": __import__("datetime").datetime.utcnow().isoformat() + "Z",
        "count": len(cands),
        "candidates": cands,
    }
    Path(args.out).write_text(json.dumps(out, indent=2))
    print(f"\nwrote {len(cands)} candidates to {args.out}")
    print("\n== top 20 (eyeball these for labeling) ==")
    for c in cands[:20]:
        locs = ", ".join(f"{Path(s['file']).name}:{s['line']}" for s in c['sample_locations'])
        print(f"  {c['stem']:20s} blocks={c['def_blocks']:3d} files={c['def_files']:3d} total={c['total_def_occs']:4d}  e.g. {locs}")


if __name__ == "__main__":
    main()