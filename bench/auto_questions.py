#!/usr/bin/env python3
"""Auto-generate comprehension questions + verifiable GT facts from ANY reliary index.

Repo-agnostic: questions are derived purely from index structure + source text.
No hardcoded symbols, no LLM. Seeded deterministically by repo root name so the
same corpus yields identical questions every run.

GT facts are mechanically checkable against the occurrence table:
  - def/caller/method/field: (sym, file, line) — symbol appears at line (±1)
  - dead: (sym, file, line) — tag=1 def with zero non-def occurrences

Usage:
  python3 bench/auto_questions.py --index /path/to/.reliary/index.sqlite \
      --seed 42 [--rust] [--skip bench/] [--out q.json]
"""
import json
import os
import random
import re
import sqlite3
import sys

STOP = {
    "self", "this", "test", "tests", "fn", "pub", "struct", "enum", "impl",
    "let", "mut", "const", "static", "return", "new", "default", "debug",
    "clone", "into", "from", "main", "as_ref", "as_mut", "unwrap", "expect",
    "error", "result", "option", "some", "none", "ok", "err", "todo", "match",
    "config", "init", "setup", "run", "start", "stop", "len", "is_empty",
    "add", "set", "get", "box", "index", "module", "types", "util", "utils",
    "utils_error", "error_chain", "future", "task", "sync", "async",
}


def is_ident(s):
    return bool(s) and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", s) and len(s) >= 3


def rust_files(db, prefixes=None, skip=()):
    return [f for f in source_files(db, prefixes, skip) if f.endswith(".rs")]


def source_files(db, prefixes=None, skip=()):
    cur = db.execute("SELECT file_path FROM file_map WHERE is_source = 1")
    files = []
    for (f,) in cur:
        if prefixes:
            if not any(f.startswith(p) for p in prefixes):
                continue
        if any(s in f for s in skip):
            continue
        files.append(f)
    return files


def read_lines(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            return fh.readlines()
    except OSError:
        return []


def brace_blocks(lines, start_idx, indent):
    """Return (start,end) 0-indexed block bounds for the block opening at
    start_idx (which is the line containing `{`). Grammar-free brace scan."""
    depth = 0
    in_str = False
    in_ch = False
    in_line_comment = False
    in_block_comment = False
    for i in range(start_idx, len(lines)):
        t = lines[i]
        j = 0
        in_line_comment = False
        while j < len(t):
            c = t[j]
            nxt = t[j + 1] if j + 1 < len(t) else ""
            if in_line_comment:
                break
            if in_block_comment:
                if c == "*" and nxt == "/":
                    in_block_comment = False
                    j += 2
                    continue
                j += 1
                continue
            if in_str:
                if c == "\\":
                    j += 2
                    continue
                if c == '"':
                    in_str = False
                j += 1
                continue
            if in_ch:
                if c == "\\":
                    j += 2
                    continue
                if c == "'":
                    in_ch = False
                j += 1
                continue
            if c == "/" and nxt == "/":
                in_line_comment = True
                break
            if c == "/" and nxt == "*":
                in_block_comment = True
                j += 2
                continue
            if c == '"':
                in_str = True
                j += 1
                continue
            if c == "'":
                in_ch = True
                j += 1
                continue
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    return (start_idx, i)
            j += 1
    return (start_idx, len(lines) - 1)


def struct_fields(path, type_name, def_line0):
    lines = read_lines(path)
    if not lines:
        return []
    # find the struct declaration containing the type name
    start = None
    for i in range(def_line0, min(def_line0 + 4, len(lines))):
        if re.search(r"\bstruct\s+" + re.escape(type_name) + r"\b", lines[i]):
            start = i
            break
    if start is None:
        # search nearby
        for i in range(max(0, def_line0 - 5), min(len(lines), def_line0 + 6)):
            if re.search(r"\bstruct\s+" + re.escape(type_name) + r"\b", lines[i]):
                start = i
                break
    if start is None:
        return []
    indent = len(lines[start]) - len(lines[start].lstrip())
    _, end = brace_blocks(lines, start, indent)
    fields = []
    for i in range(start + 1, end):
        t = lines[i].strip()
        if not t or t.startswith("//") or t.startswith("///") or t.startswith("/*"):
            continue
        if "fn " in t or "pub fn" in t:
            continue
        # field: `name: Type,` at struct indent
        m = re.match(r"pub\s+([A-Za-z_][A-Za-z0-9_]*)\s*:", t)
        if m:
            fields.append((m.group(1), i + 1))
    return fields


def impl_methods(path, type_name):
    """Grammar-free: find `impl <Type> {` blocks in the file, collect `fn` lines
    inside them as (1-indexed line, name, is_pub).

    `is_pub` is the leading-token test (`pub` followed by space or `(`), matching
    the Rust-side `is_pub_decl` — no language keyword list."""
    lines = read_lines(path)
    out = []
    i = 0
    while i < len(lines):
        t = lines[i]
        if re.search(r"\bimpl\b[^{]*\b" + re.escape(type_name) + r"\b[^{]*\{", t):
            _, end = brace_blocks(lines, i, 0)
            for j in range(i + 1, min(end, len(lines))):
                m = re.match(r"\s*((?:pub\s+)?)(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)", lines[j])
                if m:
                    out.append((m.group(2), j + 1, bool(m.group(1))))
            i = end
        i += 1
    return out


def public_methods(path, type_name):
    """Public, non-stopword methods as `(name, line)` — the q3 ground truth.

    The question asks for PUBLIC methods, so private helpers and struct fields
    must not enter the GT (a correct answer that omitted them previously looked
    incomplete). Extracted here so the test guards the production path rather
    than reimplementing the filter.
    """
    return [
        (m, l)
        for (m, l, is_pub) in impl_methods(path, type_name)
        if is_pub and m.lower() not in STOP
    ]


def symbols(db, files, tag=None):
    """All definitions in `files` as (phrase, file_path, line_1idx, tag).

    One query + grouping in Python: the per-file `WHERE fm.file_path = ?`
    form has no supporting index (occurrence is indexed by phrase_id, not
    file_id) and takes ~1s per file on a 2.8k-file corpus.
    """
    fs = set(files)
    out = []
    cur = db.execute(
        """SELECT p.phrase, fm.file_path, o.line, o.tag FROM occurrence o
           JOIN phrases p ON p.id = o.phrase_id
           JOIN file_map fm ON fm.id = o.file_id
           WHERE o.is_def = 1 AND fm.is_source = 1"""
    )
    for ph, f, line, t in cur:
        if f not in fs:
            continue
        if tag is None or t == tag:
            # o.line is 0-indexed; every tool and consumer uses 1-indexed.
            out.append((ph, f, line + 1, t))
    return out


_FILE_CACHE = {}


def _read_cached_lines(file_path):
    if file_path not in _FILE_CACHE:
        try:
            with open(file_path, encoding="utf-8", errors="replace") as fh:
                _FILE_CACHE[file_path] = fh.read().splitlines()
        except OSError:
            _FILE_CACHE[file_path] = []
    return _FILE_CACHE[file_path]


def _strip_strings_and_comments(line):
    """Remove string literals and line comments from a source line.

    A symbol mentioned only inside a string ("... write_vocab() must be ...")
    is not a call site. Grammar-free: byte scan with quote/escape tracking.
    """
    out = []
    i = 0
    n = len(line)
    in_str = None  # quote char or None
    while i < n:
        c = line[i]
        if in_str:
            if c == "\\":
                i += 2
                continue
            if c == in_str:
                in_str = None
            i += 1
            continue
        if c in ("'", '"'):
            in_str = c
            i += 1
            continue
        if c == "#":
            break
        if c == "/" and i + 1 < n and line[i + 1] == "/":
            break
        out.append(c)
        i += 1
    return "".join(out)


def _is_real_call(file_path, line_0idx, phrase):
    """True if `phrase` appears as a code token (not in a string/comment).

    Case-insensitive: phrases are stored lowercased (`getfiletypecategoryby...`)
    while the source may be camelCase (`getFileTypeCategoryByExtension`).
    """
    lines = _read_cached_lines(file_path)
    if line_0idx >= len(lines):
        return False
    code = _strip_strings_and_comments(lines[line_0idx])
    return bool(re.search(rf"(?<![A-Za-z0-9_]){re.escape(phrase)}(?![A-Za-z0-9_])", code, re.I))


def callers_of(db, phrase, files):
    out = []
    cur = db.execute(
        """SELECT fm.file_path, o.line FROM occurrence o
           JOIN phrases p ON p.id = o.phrase_id
           JOIN file_map fm ON fm.id = o.file_id
           WHERE p.phrase = ? AND o.is_def = 0 AND fm.is_source = 1""",
        (phrase,),
    )
    for f, line in cur:
        if f in files and _is_real_call(f, line, phrase):
            out.append((f, line + 1))  # 0-indexed -> 1-indexed
    return out


def nondef_count(db, phrase):
    """Count real (non-string) call sites of `phrase`."""
    n = 0
    cur = db.execute(
        """SELECT fm.file_path, o.line FROM occurrence o
           JOIN phrases p ON p.id = o.phrase_id
           JOIN file_map fm ON fm.id = o.file_id
           WHERE p.phrase = ? AND o.is_def = 0 AND fm.is_source = 1""",
        (phrase,),
    )
    for f, line in cur:
        if _is_real_call(f, line, phrase):
            n += 1
    return n


def def_sites(db, phrase, files):
    out = []
    cur = db.execute(
        """SELECT fm.file_path, o.line FROM occurrence o
           JOIN phrases p ON p.id = o.phrase_id
           JOIN file_map fm ON fm.id = o.file_id
           WHERE p.phrase = ? AND o.is_def = 1 AND fm.is_source = 1""",
        (phrase,),
    )
    for f, line in cur:
        if f in files:
            out.append((f, line))
    return out


def generate(index_path, seed=42, rust_only=True, skip=("bench", "scripts", "fixtures", "configs", "tests")):
    db = sqlite3.connect(index_path)
    db.row_factory = sqlite3.Row
    repo = os.path.basename(os.path.dirname(os.path.dirname(index_path)))
    rng = random.Random(f"{repo}:{seed}")

    files = rust_files(db, skip=skip) if rust_only else source_files(db, skip=skip)
    if not files:
        files = source_files(db, skip=skip)
    if not files:
        print(json.dumps({"repo": repo, "questions": [], "n_files": 0}))
        return

    all_syms = symbols(db, files)
    func_syms = [s for s in all_syms if s[3] == 1 and is_ident(s[0]) and s[0].lower() not in STOP]
    type_syms = [s for s in all_syms if s[2] in (2,) and is_ident(s[0])]
    rng.shuffle(func_syms)

    questions = []
    used = set()

    def add(qid, q, gt):
        questions.append({"query_id": qid, "question": q, "gt": gt})

    # q1: where is a function defined — prefer snake_case, unambiguous defs
    for (ph, f, line, tag) in func_syms:
        if ph.lower() in used or len(ph) < 5 or "_" not in ph:
            continue  # avoid generic words like `path`, `run`, `start`
        if len(def_sites(db, ph, files)) != 1:
            continue  # ambiguous — multiple defs
        if nondef_count(db, ph) < 1:
            continue  # no usages anywhere — not interesting for def
        used.add(ph.lower())
        add("q1_def",
            f"Where is the function `{ph}` defined? Give the file path and line number.",
            [{"sym": ph, "file": f, "line": line}])
        break

    # q2: who calls X
    for (ph, f, line, tag) in func_syms:
        if ph.lower() in used or len(ph) < 4:
            continue
        callers = callers_of(db, ph, files)
        if 2 <= len(callers) <= 8:
            used.add(ph.lower())
            add("q2_callers",
                f"Which functions or modules call `{ph}`? List each caller file:line site.",
                [{"sym": ph, "file": c[0], "line": c[1]} for c in callers[:6]])
            break

    # q3: methods on a type (from source, grammar-free)
    for (ph, f, line, tag) in type_syms:
        if ph.lower() in used or not re.match(r"^[A-Z]", ph):
            continue
        methods = public_methods(f, ph)
        if len(methods) >= 3:
            used.add(ph.lower())
            add("q3_methods",
                f"List the public methods defined on the type `{ph}` (its impl block is in {os.path.basename(f)} around line {line + 1}).",
                [{"sym": m, "file": f, "line": l} for (m, l) in methods[:8]])
            break

    # q4: struct fields (from source, grammar-free)
    for (ph, f, line, tag) in type_syms:
        if ph.lower() in used or not re.match(r"^[A-Z]", ph):
            continue
        fields = struct_fields(f, ph, line)
        if len(fields) >= 2:
            used.add(ph.lower())
            add("q4_fields",
                f"List the fields of the struct `{ph}` (defined in {os.path.basename(f)} at line {line + 1}).",
                [{"sym": nm, "file": f, "line": ln} for (nm, ln) in fields])
            break

    # q5: dead code in a module.
    # Only emitted when the tool itself reports dead candidates in a module whose
    # definitions carry a visibility marker — otherwise the question asks for
    # something the corpus cannot express (e.g. "pub functions" in a TypeScript
    # or C++ tree). The answer is still derived from the same occurrence data the
    # tool uses, so it is verifiable, not Rust-specific.
    mods = {}
    for (ph, f, line, tag) in func_syms:
        if nondef_count(db, ph) == 0:
            mod = f.rsplit("/", 1)[0]
            mods.setdefault(mod, []).append((ph, f, line))
    if mods:
        mod = max(mods, key=lambda m: len(mods[m]))
        sample = mods[mod][:5]
        visible = 0
        for (_s, fl, ln) in sample:
            try:
                src = _read_cached_lines(fl)[ln - 1]
            except IndexError:
                continue
            # A declaration carrying `pub`/`export`/`public` (any language's
            # visibility spelling) — grammar-free substring test on the
            # definition line only.
            if re.search(r"\b(pub|export|public)\b", _strip_strings_and_comments(src)):
                visible += 1
        if sample and visible >= 1:
            add("q5_dead",
                f"Find unused exported/public functions in `{mod}/`. "
                f"List name + file:line for any you find.",
                [{"sym": s, "file": fl, "line": ln} for (s, fl, ln) in sample])

    # q6: which struct implements Default (derived)
    default_impls = []
    for f in files:
        lines = read_lines(f)
        for i, t in enumerate(lines):
            if re.search(r"\b(derive\([^)]*Default[^)]*\)|impl\s+Default\s+for\s+([A-Za-z_][A-Za-z0-9_]*))", t):
                m = re.search(r"for\s+([A-Za-z_][A-Za-z0-9_]*)", t)
                name = m.group(1) if m else None
                if name:
                    default_impls.append((name, f, i + 1))
    if default_impls and not any(q["query_id"] == "q4_fields" for q in questions):
        sample = default_impls[:4]
        add("q6_default",
            "Which structs in this codebase implement (or derive) the `Default` trait? List name + file:line.",
            [{"sym": n, "file": fl, "line": ln} for (n, fl, ln) in sample])

    print(json.dumps({"repo": repo, "questions": questions, "n_files": len(files)}, indent=2))


if __name__ == "__main__":
    idx = sys.argv[1] if len(sys.argv) > 1 else ".reliary/index.sqlite"
    seed = int(sys.argv[2]) if len(sys.argv) > 2 else 42
    generate(idx, seed)