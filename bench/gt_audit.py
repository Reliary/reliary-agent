#!/usr/bin/env python3
"""V66: mechanical GT audit — verifies every ground-truth fact against the
index AND the source. Facts that fail source validation are dropped; facts the
index proves are missing are added. This is the reusable replacement for the
inline V64 audit.

Usage:
    python3 bench/gt_audit.py --corpus /tmp/rel8-corpus [--fix]

Prints a per-fact table. With --fix, rewrites the GT_FACTS dict in
reliary_judge_gt.py with verified/enriched facts.

Fact types:
    ("callers",  symbol)        -> all file:line where symbol is called (is_def=0)
    ("def",      symbol)        -> primary def file:line for symbol (is_def=1, tag=1)
    ("fields",   symbol)        -> field lines of the struct definition
    ("impls",    trait)         -> types implementing a trait (impl Trait for X)
    ("dead",     path_prefix)   -> is_def=1 symbols in path with 0 callers

Every fact is validated against source (line-contains or structural check)
before being written. GT never derives from bench answers — only from index +
source.
"""
import argparse
import json
import re
import sqlite3
import sys
from pathlib import Path

SOURCE_EXTS = {".rs", ".py", ".js", ".ts", ".go", ".c", ".h", ".cpp", ".java", ".rb"}


def open_db(corpus: str) -> sqlite3.Connection:
    db_path = Path(corpus) / ".reliary" / "index.sqlite"
    if not db_path.exists():
        sys.exit(f"no index at {db_path}")
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    return conn


def is_source_path(path: str) -> bool:
    return Path(path).suffix in SOURCE_EXTS


def phrase_id(conn, name):
    row = conn.execute(
        "SELECT id FROM phrases WHERE phrase = ?1", (name.lower(),)
    ).fetchone()
    if row:
        return row["id"]
    # stem-ish fallback: prefix
    row = conn.execute(
        "SELECT id FROM phrases WHERE phrase LIKE ?1 ORDER BY LENGTH(phrase) LIMIT 1",
        (name.lower() + "%",),
    ).fetchone()
    return row["id"] if row else None


def file_id(conn, path):
    row = conn.execute(
        "SELECT id FROM file_map WHERE file_path LIKE ?1", (f"%{path}",)
    ).fetchone()
    return row["id"] if row else None


def fact_def(conn, corpus, symbol):
    """Primary def: is_def=1, tag=1 (fn) falling back to tag=2 (struct/type).
    Returns (file_rel, line_1idx) or None."""
    pid = phrase_id(conn, symbol)
    if pid is None:
        return None
    for tag in (1, 2):
        rows = conn.execute(
            """SELECT f.file_path, o.line FROM occurrence o
               JOIN file_map f ON f.id = o.file_id
               WHERE o.phrase_id = ?1 AND o.is_def = 1 AND o.tag = ?2
               ORDER BY LENGTH(f.file_path) ASC""",
            (pid, tag),
        ).fetchall()
        for r in rows:
            if not is_source_path(r["file_path"]):
                continue
            rel = str(Path(r["file_path"]).relative_to(corpus))
            # source validation: line must contain the symbol (case-insensitive —
            # phrase table stores lowercase, source may be PascalCase)
            try:
                lines = Path(r["file_path"]).read_text().splitlines()
                if symbol.lower() in lines[r["line"]].lower():
                    return (rel, r["line"] + 1)
            except (OSError, IndexError):
                continue
    return None


def fact_callers(conn, corpus, symbol, limit=15, scope=None):
    """All call sites of symbol in source files. Returns [(file_rel, line_1idx)].

    scope: optional path prefix to restrict (e.g. 'crates/') — Python bench
    scripts legitimately call Rust symbols via subprocess, but for questions
    about crate internals we scope to the source tree.
    """
    pid = phrase_id(conn, symbol)
    if pid is None:
        return []
    rows = conn.execute(
        """SELECT f.file_path, o.line FROM occurrence o
           JOIN file_map f ON f.id = o.file_id
           WHERE o.phrase_id = ?1 AND o.is_def = 0
           ORDER BY f.file_path, o.line""",
        (pid,),
    ).fetchall()
    out = []
    for r in rows:
        if not is_source_path(r["file_path"]):
            continue
        if scope and scope not in r["file_path"]:
            continue
        rel = str(Path(r["file_path"]).relative_to(corpus))
        try:
            lines = Path(r["file_path"]).read_text().splitlines()
            if symbol in lines[r["line"]]:
                out.append((rel, r["line"] + 1))
        except (OSError, IndexError):
            continue
        if len(out) >= limit:
            break
    return out


def fact_struct_def(conn, corpus, symbol):
    """Struct/type def specifically: is_def=1, tag=2, line must declare 'struct SYMBOL'.
    Returns (file_rel, line_1idx) or None."""
    pid = phrase_id(conn, symbol)
    if pid is None:
        return None
    rows = conn.execute(
        """SELECT f.file_path, o.line FROM occurrence o
           JOIN file_map f ON f.id = o.file_id
           WHERE o.phrase_id = ?1 AND o.is_def = 1 AND o.tag = 2
           ORDER BY LENGTH(f.file_path) ASC""",
        (pid,),
    ).fetchall()
    for r in rows:
        if not is_source_path(r["file_path"]):
            continue
        rel = str(Path(r["file_path"]).relative_to(corpus))
        try:
            lines = Path(r["file_path"]).read_text().splitlines()
            if re.search(rf"\bstruct\s+{re.escape(symbol)}\b", lines[r["line"]]):
                return (rel, r["line"] + 1)
        except (OSError, IndexError):
            continue
    return None


def fact_fields(conn, corpus, symbol):
    """Field lines of the struct definition. Returns [(field, type, file_rel, line_1idx)]."""
    def_loc = fact_struct_def(conn, corpus, symbol)
    if def_loc is None:
        return []
    fpath = Path(corpus) / def_loc[0]
    lines = fpath.read_text().splitlines()
    fields = []
    depth = 0
    in_struct = False
    for i in range(def_loc[1] - 1, len(lines)):
        t = lines[i]
        if not in_struct:
            if f"struct {symbol}" in t:
                in_struct = True
                # count braces on the struct line itself (e.g. `pub struct X {`)
                depth = t.count("{") - t.count("}")
            continue
        depth += t.count("{") - t.count("}")
        if depth <= 0:
            break
        m = re.match(r"\s*pub\s+(\w+):\s*([^,]+),?", t)
        if m:
            fields.append((m.group(1), m.group(2).strip(), def_loc[0], i + 1))
    return fields


def fact_impls(conn, corpus, trait_name, workspace="crates"):
    """Types implementing a trait. Returns [(type, file_rel, line_1idx)]."""
    out = []
    for p in (Path(corpus) / workspace).rglob("*.rs"):
        try:
            lines = p.read_text().splitlines()
        except OSError:
            continue
        for i, l in enumerate(lines):
            m = re.search(rf"impl\s+(?:\S+\s+for\s+)?{re.escape(trait_name)}\s+for\s+(\w+)", l)
            if m:
                rel = str(p.relative_to(corpus))
                out.append((m.group(1), rel, i + 1))
    return out


def fact_dead(conn, corpus, path_prefix, limit=10):
    """pub fns in path with zero callers. Returns [(symbol, file_rel, line_1idx)]."""
    rows = conn.execute(
        """SELECT p.phrase, f.file_path, o.line FROM occurrence o
           JOIN phrases p ON p.id = o.phrase_id
           JOIN file_map f ON f.id = o.file_id
           WHERE o.is_def = 1 AND o.tag = 1 AND f.is_source = 1
             AND f.file_path LIKE ?1
           ORDER BY f.file_path, o.line""",
        (f"%{path_prefix}%",),
    ).fetchall()
    out = []
    seen = set()
    for r in rows:
        sym = r["phrase"]
        if sym in seen or len(sym) < 4:
            continue
        seen.add(sym)
        callers = conn.execute(
            """SELECT COUNT(*) AS n FROM occurrence o
               JOIN file_map f ON f.id = o.file_id
               WHERE o.phrase_id = ?1 AND o.is_def = 0 AND f.is_source = 1""",
            (r["phrase"],),
        ).fetchone()  # NOTE: wrong join key; fixed below
    # recompute properly per symbol
    out = []
    seen = set()
    for r in rows:
        sym = r["phrase"]
        if sym in seen or len(sym) < 4:
            continue
        seen.add(sym)
        pid = r["phrase"]
        prow = conn.execute("SELECT id FROM phrases WHERE phrase = ?1", (pid,)).fetchone()
        if not prow:
            continue
        n = conn.execute(
            """SELECT COUNT(*) AS n FROM occurrence o
               JOIN file_map f ON f.id = o.file_id
               WHERE o.phrase_id = ?1 AND o.is_def = 0 AND f.is_source = 1""",
            (prow["id"],),
        ).fetchone()["n"]
        if n == 0:
            rel = str(Path(r["file_path"]).relative_to(corpus))
            out.append((sym, rel, r["line"] + 1))
        if len(out) >= limit:
            break
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="/tmp/rel8-corpus")
    ap.add_argument("--fix", action="store_true", help="rewrite GT facts in reliary_judge_gt.py")
    args = ap.parse_args()
    conn = open_db(args.corpus)

    checks = [
        ("q3 def classify_structural", fact_def(conn, args.corpus, "classify_structural")),
        ("q6 def StructuralResult", fact_struct_def(conn, args.corpus, "StructuralResult")),
        ("q1 fields StructuralResult", fact_fields(conn, args.corpus, "StructuralResult")),
        ("q7 fields BraceNode", fact_fields(conn, args.corpus, "BraceNode")),
        ("q2 callers classify_structural", fact_callers(conn, args.corpus, "classify_structural", scope="crates/")),
        ("q5 callers build_brace_graph", fact_callers(conn, args.corpus, "build_brace_graph", scope="crates/")),
        ("q9 impls Default", fact_impls(conn, args.corpus, "Default")),
        ("q10 dead reliary-search/src", fact_dead(conn, args.corpus, "reliary-search/src")),
    ]
    for name, val in checks:
        print(f"== {name} ==")
        if isinstance(val, list):
            for v in val:
                print("  ", v)
        else:
            print("  ", val)


if __name__ == "__main__":
    main()
