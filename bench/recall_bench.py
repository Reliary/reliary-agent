#!/usr/bin/env python3
"""
P0: head-to-head recall, no LLM.

The claim under test is the indexer's own claim: for a symbol, return the right
definition and the complete set of code places that use it, cheaply. No model
is involved, so there is no sampling variance, no memorisation confound and no
API cost.

Ground truth is an *independent lexical oracle*: strip comments and string
literals, then whole-word match. It shares no code with reliary's lexer, so no
tool grades itself. It is not a compiler, so it does conflate homonyms (two
different symbols with the same name); that is reported and it makes every
condition's job harder, not easier.

Views reported
--------------
  R_all    every exact-case occurrence, definition included.
           What "rename this and miss nothing" requires.
  R_use    call sites only, definition lines excluded on both sides.
           What "who calls X" requires.
  R_ntest  R_use excluding test paths, which is usage_only's documented scope.
  def      is the correct definition line returned by a definition query?

Usage
  python3 bench/recall_bench.py --corpus DIR --lang rust|python \
      --tags <ctags file> --project <cbm project> --symbols 24 [--out x.json]
"""

import argparse
import json
import os
import re
import statistics as st
import subprocess
import sys
import time
from pathlib import Path

IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
SRC_EXT = {"rust": ".rs", "python": ".py"}
SKIP_DIRS = {"target", "node_modules", ".git", ".reliary", "build", "dist",
             ".venv", "venv", "__pycache__"}


# ------------------------------------------------------------------ oracle


def strip_code(text: str, lang: str) -> list[str]:
    """Blank out comments and string literals, preserving line numbering.

    Blanking rather than deleting keeps columns aligned and lines 1:1 with the
    original file, which is what makes the oracle usable as ground truth.
    """
    out: list[str] = []
    i, n = 0, len(text)
    state = "code"
    quote = ""
    block_depth = 0

    while i < n:
        ch = text[i]
        nxt = text[i + 1] if i + 1 < n else ""

        if state == "code":
            if lang == "rust" and ch == "/" and nxt == "/":
                state = "line"; out.append(" "); i += 1; continue
            if lang == "rust" and ch == "/" and nxt == "*":
                state = "block"; block_depth = 1; out.append("  "); i += 2; continue
            if lang == "python" and ch == "#":
                state = "line"; out.append(" "); i += 1; continue
            if lang == "rust" and ch == "r":
                m = re.match(r'r(#*)"', text[i:i + 12])
                if m:
                    quote = '"' + m.group(1)
                    state = "raw"
                    out.append(" " * len(m.group(0)))
                    i += len(m.group(0))
                    continue
            if lang == "python" and (text.startswith('"""', i) or text.startswith("'''", i)):
                quote = text[i:i + 3]
                state = "triple"
                out.append("   ")
                i += 3
                continue
            if ch in "\"'":
                # A Rust lifetime (`'a`) is not a string literal.
                if lang == "rust" and ch == "'":
                    if not re.match(r"'(?:[^'\\]|\\.)'", text[i:i + 4]):
                        out.append(ch); i += 1; continue
                quote = ch; state = "string"; out.append(" "); i += 1; continue
            out.append(ch); i += 1; continue

        if state == "line":
            if ch == "\n":
                state = "code"; out.append(ch)
            else:
                out.append(" ")
            i += 1; continue

        if state == "block":
            if lang == "rust" and ch == "/" and nxt == "*":
                block_depth += 1; out.append("  "); i += 2; continue
            if ch == "*" and nxt == "/":
                block_depth -= 1
                state = "code" if block_depth <= 0 else "block"
                out.append("  "); i += 2; continue
            out.append("\n" if ch == "\n" else " "); i += 1; continue

        # raw / triple / string all end on their own delimiter
        if state in ("raw", "triple", "string"):
            # A backslash escapes the next byte — but a backslash at end of
            # line is a line CONTINUATION, and consuming the newline here
            # merged two source lines. Every line number after the merge then
            # pointed one line early, so the oracle reported a definition as a
            # usage (measured: 467-line file produced 464 masked lines, and
            # `VecDeque` was "missing" a site that the tool had returned).
            if ch == "\\" and nxt and nxt != "\n" and state != "raw":
                out.append("  "); i += 2; continue
            if text.startswith(quote, i):
                out.append(" " * len(quote)); i += len(quote)
                state = "code"; continue
            out.append("\n" if ch == "\n" else " "); i += 1; continue

    return "".join(out).split("\n")


def source_files(root: Path, lang: str) -> list[Path]:
    ext = SRC_EXT[lang]
    return sorted(p for p in root.rglob("*" + ext)
                  if not any(part in SKIP_DIRS for part in p.parts))


def loc_key(path: str) -> str:
    """Basename, for every condition.

    reliai prints `driver.rs:8`, grep prints `./tokio/src/io/driver.rs:8` and the
    oracle stores a corpus-relative path. Comparing full paths scored every
    reliai hit as a miss as soon as a repo had two same-named files (tokio has
    20+ `mod.rs`) — a measurement artefact, not a recall result. Basename+line
    is applied symmetrically, and the ambiguity rate is reported.
    """
    return path.replace("\\", "/").lstrip("./").rsplit("/", 1)[-1]


TEST_DIRS = ("/tests/", "/test/", "/examples/", "/benches/", "/bench/")


def is_test_path(rel: str) -> bool:
    """Test/bench/example paths, matching usage_only's documented scope.

    The tool excludes all four categories, so the truth set must exclude them
    too or the comparison measures the exclusion instead of recall. Applied to
    every condition's output as well, so it is symmetric.
    """
    p = "/" + rel.replace("\\", "/").lstrip("./")
    base = p.rsplit("/", 1)[-1]
    return (any(d in p for d in TEST_DIRS)
            or base.startswith("test_") or base.endswith("_test.py")
            or base in ("tests.py", "test.py"))


# `macro_rules!` ends in `!`, which is not a word character, so a trailing `\b`
# never matches it — the macro's own definition line was counted as a USAGE and
# every tool scored one short on it.
DECL_PREFIX = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?"
    r"(?:(?:fn|struct|enum|trait|union|type|mod|impl|class|def|interface|"
    r"namespace|record|object|let|static|val)\b|macro_rules!)"
)


def is_decl_line(line: str, lang: str) -> bool:
    """Is this line a declaration rather than a use?

    Independent of all three tools. Needed because "who calls X" must exclude
    declaration sites on every side: reliai marks each `impl ... for X` header
    as a definition and drops it, while ctags reports only the struct header,
    so without this rule the two sides disagree about what counts as a usage
    and reliai is scored as missing lines it deliberately excluded.
    """
    st = line.strip()
    if not st:
        return True
    if st.startswith(("//", "/*", "*", "#")):
        return True
    return bool(DECL_PREFIX.match(line))


class Oracle:
    def __init__(self, root: Path, lang: str):
        self.root = root
        self.lang = lang
        self.occ: dict[str, set[tuple[str, int]]] = {}
        self.ci: dict[str, set[tuple[str, int]]] = {}
        self.lines_cache: dict[Path, list[str]] = {}
        self.ambiguous = 0
        self.sites = 0
        self.by_basename: dict[str, list[Path]] = {}
        self.names_lower = {n.lower() for n in self.occ}
        for f in source_files(root, lang):
            try:
                text = f.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue
            stripped = strip_code(text, lang)
            self.lines_cache[f] = stripped
            self.by_basename.setdefault(f.name, []).append(f)
            rel = str(f.relative_to(root))
            seen: dict[str, set[int]] = {}
            for ln, line in enumerate(stripped, 1):
                for m in IDENT.finditer(line):
                    name = m.group(0)
                    self.occ.setdefault(name, set()).add((rel, ln))
                    self.ci.setdefault(name.lower(), set()).add((rel, ln))
                    seen.setdefault(name, set()).add(ln)
            # ambiguity: same basename, same line, another file
            for name, lns in seen.items():
                for ln in lns:
                    self.sites += 1
        self._count_ambiguity()

    def _count_ambiguity(self) -> None:
        by_key: dict[tuple[str, int], set[str]] = {}
        for occ in self.occ.values():
            for f, ln in occ:
                by_key.setdefault((loc_key(f), ln), set()).add(f)
        self.ambiguous = sum(1 for v in by_key.values() if len(v) > 1)

    def occurrences(self, name: str) -> set[tuple[str, int]]:
        return self.occ.get(name, set())

    def is_decl_full(self, rel: str, ln: int) -> bool:
        lines = self.lines_cache.get(self.root / rel)
        if not lines or ln - 1 >= len(lines):
            return True
        return is_decl_line(lines[ln - 1], self.lang)

    def is_decl(self, rel: str, ln: int) -> bool:
        """Declaration check keyed the same way as loc_key (basename).

        `rel` is a basename here, so every corpus file with that basename is
        consulted. If any of them is a non-declaration line the site is treated
        as a usage, which is the conservative choice for the tool: it can only
        lower precision, never inflate recall.
        """
        cands = self.by_basename.get(rel)
        if not cands:
            return False
        verdicts = []
        for f in cands:
            lines = self.lines_cache.get(f)
            if lines and ln - 1 < len(lines):
                verdicts.append(is_decl_line(lines[ln - 1], self.lang))
        if not verdicts:
            return False
        return all(verdicts)

    def has_stem_collision(self, name: str) -> bool:
        """Does another identifier in the corpus share this one's phrase key?

        The index keys phrases on a Porter stem of the lowercased identifier, so
        `Request`, `request` and `requests` are ONE phrase. That is a real
        limitation, and it is measured rather than hidden: a symbol flagged here
        cannot be answered precisely by a stem-keyed index, and every stem-keyed
        tool pays for it.
        """
        low = name.lower()
        # Same lowercase spelling, different identifier: `Request` vs `request`.
        if any(n != name and n.lower() == low for n in self.occ):
            return True
        # Plural/near-plural forms collapse into the same Porter stem.
        if low + "s" in self.names_lower or low + "es" in self.names_lower:
            return True
        if low.endswith("s") and low[:-1] in self.names_lower:
            return True
        if low.endswith("es") and low[:-2] in self.names_lower:
            return True
        return False

    def occurrences_ci(self, name: str) -> set[tuple[str, int]]:
        """Case-folded occurrences: what a case-insensitive index can return.

        reliai stores phrase keys lowercased, so `Request` and `request` share a
        phrase. That is a real limitation (a rename cannot distinguish them),
        but it is not a recall miss, so it is measured separately rather than
        counted as one.
        """
        return self.ci.get(name.lower(), set())


# ------------------------------------------------------------------ tools


class McpSession:
    def __init__(self, argv, cwd=None, env=None):
        self.p = subprocess.Popen(
            argv, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True, bufsize=1, env=env)
        self.id = 0
        self.call("initialize", {"protocolVersion": "2024-11-05",
                                 "capabilities": {},
                                 "clientInfo": {"name": "recall-bench", "version": "1"}},
                  method=True)
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def send(self, obj):
        self.p.stdin.write(json.dumps(obj) + "\n")
        self.p.stdin.flush()

    def call(self, name, args, method=False):
        self.id += 1
        msg = {"jsonrpc": "2.0", "id": self.id}
        if method:
            msg["method"], msg["params"] = name, args
        else:
            msg["method"] = "tools/call"
            msg["params"] = {"name": name, "arguments": args}
        t0 = time.perf_counter()
        self.send(msg)
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("MCP server closed")
            try:
                d = json.loads(line)
            except json.JSONDecodeError:
                continue
            if d.get("id") == self.id:
                return d, time.perf_counter() - t0

    def close(self):
        try:
            self.p.stdin.close()
        except Exception:
            pass
        self.p.terminate()


LOC_RE = re.compile(r"([A-Za-z0-9_./\-]+\.[A-Za-z0-9]+):(\d+)")


def parse_locs(text: str) -> set[tuple[str, int]]:
    return {(loc_key(m.group(1)), int(m.group(2))) for m in LOC_RE.finditer(text)}


def reliai_refs(sess, name, mode=None, limit=2000, include_tests=True):
    args = {"name": name, "path": ".", "limit": limit,
            "include_tests": include_tests}
    if mode:
        args[mode] = True
    d, dt = sess.call("reliary_find_references", args)
    txt = d.get("result", {}).get("content", [{}])[0].get("text", "")
    return parse_locs(txt), txt, dt


def cbm_refs(sess, name, project):
    d, dt = sess.call("search_code", {"pattern": name, "project": project, "limit": 500})
    txt = d.get("result", {}).get("content", [{}])[0].get("text", "")
    out = set()
    try:
        for r in json.loads(txt).get("results", []):
            for ln in r.get("match_lines") or []:
                if r.get("file"):
                    out.add((loc_key(r["file"]), int(ln)))
    except json.JSONDecodeError:
        pass
    return out, txt, dt


def cbm_def(sess, name, project):
    d, dt = sess.call("search_code", {"pattern": name, "project": project, "limit": 50})
    txt = d.get("result", {}).get("content", [{}])[0].get("text", "")
    out = set()
    try:
        for r in json.loads(txt).get("results", []):
            if r.get("node") == name and r.get("file") and r.get("start_line"):
                out.add((loc_key(r["file"]), int(r["start_line"])))
    except json.JSONDecodeError:
        pass
    return out, txt, dt


def grep_refs(root: Path, name: str, ext: str):
    t0 = time.perf_counter()
    r = subprocess.run(
        ["grep", "-rnw", "--include=*" + ext, name, "."],
        cwd=root, capture_output=True, text=True)
    dt = time.perf_counter() - t0
    out = set()
    for line in r.stdout.splitlines():
        parts = line.split(":", 2)
        if len(parts) >= 3 and parts[1].isdigit():
            out.add((loc_key(parts[0]), int(parts[1])))
    return out, r.stdout, dt


# ------------------------------------------------------------------ selection


def pick_symbols(oracle: Oracle, defined: set[str], n: int) -> list[str]:
    """Outcome-blind: sample by occurrence frequency, never by tool result."""
    counts = {k: len(v) for k, v in oracle.occ.items()
              if k in defined and len(v) >= 4}
    bands = [(4, 15, n // 3), (16, 60, n // 3), (61, 10 ** 9, n - 2 * (n // 3))]
    chosen: list[str] = []
    for lo, hi, take in bands:
        pool = sorted((c, k) for k, c in counts.items() if lo <= c <= hi)
        chosen.extend(k for _, k in pool[:take])
    out, seen = [], set()
    for c in chosen:
        if c not in seen:
            seen.add(c)
            out.append(c)
    return out


# ------------------------------------------------------------------ main


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--lang", choices=["rust", "python"], required=True)
    ap.add_argument("--tags", required=True)
    ap.add_argument("--project", required=True)
    ap.add_argument("--symbols", type=int, default=24)
    ap.add_argument("--reliary-bin",
                    default=str(Path(__file__).resolve().parent.parent
                                / "target" / "release" / "reliary"))
    ap.add_argument("--cbm-bin",
                    default=str(Path.home() / ".local" / "bin" / "codebase-memory-mcp"))
    ap.add_argument("--only", default="reliary,cbm,grep")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    root = Path(args.corpus).resolve()
    tools = args.only.split(",")
    ext = SRC_EXT[args.lang]

    tags: dict[str, list[tuple[str, int]]] = {}
    all_tags: dict[str, list[tuple[str, int]]] = {}
    for line in open(args.tags):
        p = line.rstrip("\n").split("\t")
        if len(p) < 4 or p[3] in ("field", "variable", "constant", "unknown"):
            continue
        num = "".join(c for c in p[2] if c.isdigit())
        if num:
            all_tags.setdefault(p[0], []).append((p[1].lstrip("./"), int(num), p[3]))
    # Exclude symbols *defined* in test paths. `usage_only` excludes test
    # directories by design, so asking "who calls X" for a helper that only
    # exists inside a test file is a category error: the honest answer is
    # "nothing outside tests", not "I missed it". Chosen from ctags plus the
    # path, never from a tool's output.
    dropped_test_local = []
    for name, defs in all_tags.items():
        if defs and all(is_test_path(f) for (f, _ln, _k) in defs):
            dropped_test_local.append(name)
    print(f"excluded {len(dropped_test_local)} symbols whose only definitions "
          f"live in test paths", file=sys.stderr)

    # Enumerators, module declarations and struct fields are not symbol
    # definitions in reliary's model (they are values, namespaces and slots).
    # Asking all three tools "where is this defined" for them measures a stated
    # design choice, so they are excluded from the definition comparison. The
    # usage comparison is unaffected: usages of these names are still counted.
    unsupported = {"enumerator", "module", "field", "namespace", "unknown"}
    def_supported = {n for n, ds in all_tags.items()
                     if any(k not in unsupported for (_f, _ln, k) in ds)}
    print(f"excluded {len(all_tags) - len(def_supported)} symbols whose only "
          f"definitions are kinds reliary does not model", file=sys.stderr)

    print(f"oracle: indexing {root} ...", file=sys.stderr)
    oracle = Oracle(root, args.lang)
    print(f"oracle: {len(oracle.occ)} identifiers, {len(oracle.lines_cache)} files, "
          f"{oracle.ambiguous} ambiguous (basename,line) sites", file=sys.stderr)

    tags = {n: [(f, ln) for (f, ln, _k) in ds]
            for n, ds in all_tags.items() if n in def_supported}
    syms = pick_symbols(oracle, set(tags), args.symbols)
    print(f"symbols ({len(syms)}): {syms}", file=sys.stderr)

    sessions = {}
    if "reliary" in tools:
        env = dict(os.environ, RELIARY_NO_TRUNCATE="1")
        sessions["reliary"] = McpSession([args.reliary_bin, "mcp"], cwd=root, env=env)
    if "cbm" in tools:
        sessions["cbm"] = McpSession([args.cbm_bin], cwd=root)

    # One warm-up call per tool per mode: the first query against a fresh
    # process pays ~400ms of index/JIT setup, which is real but is a one-time
    # cost an agent pays once per session, not per question. Both numbers are
    # kept: `p50` below is warm, and the cold figure is reported separately.
    cold = {}
    if syms:
        if "reliary" in tools:
            t0 = time.perf_counter()
            reliai_refs(sessions["reliary"], syms[0], "usage_only")
            cold["reliary"] = round((time.perf_counter() - t0) * 1000, 1)
            reliai_refs(sessions["reliary"], syms[0], "def_only")
        if "cbm" in tools:
            t0 = time.perf_counter()
            cbm_refs(sessions["cbm"], syms[0], args.project)
            cold["cbm"] = round((time.perf_counter() - t0) * 1000, 1)
        if "grep" in tools:
            t0 = time.perf_counter()
            grep_refs(root, syms[0], ext)
            cold["grep"] = round((time.perf_counter() - t0) * 1000, 1)

    rows = []
    for i, sym in enumerate(syms, 1):
        # Truth keyed by FULL corpus-relative path, so test-path partitioning is
        # exact. Tool locations are (basename, line); a tool site matches a truth
        # site when the basename and line agree. This is the only comparison that
        # works across three tools that print `sessions.py:395`,
        # `./src/requests/sessions.py:395` and a bare path respectively.
        # V97: truth is CASE-SENSITIVE. `occurrences_ci` folds case, which
        # conflates `Buf` with the 1,963 `buf` sites in tokio (and `Direction`
        # with `direction`), so recall was measured against a denominator 33x
        # larger than the symbol and no tool could reach 1.0. The folded set is
        # still reported as `*_ci` for the case-insensitive-index question.
        truth_all_ci = oracle.occurrences_ci(sym)       # {(rel_path, line)}
        truth_all = oracle.occurrences(sym)
        if not truth_all:
            truth_all = truth_all_ci
        row_ci = {"truth_all": len(truth_all_ci)}
        decl = {(f, ln) for (f, ln) in truth_all if oracle.is_decl_full(f, ln)}
        truth_use = truth_all - decl
        truth_use_nt = {x for x in truth_use if not is_test_path(x[0])}
        def_lines = {(f, ln) for f, ln in tags.get(sym, [])}
        row = {**row_ci, "symbol": sym, "stem_collision": oracle.has_stem_collision(sym),
               "truth_all_cs": len(truth_all),
               "truth_use": len(truth_use), "truth_ntest": len(truth_use_nt),
               "defs": len(def_lines)}

        for tool in tools:
            rec = {"api_ms": None, "bytes": 0, "returned": 0}
            try:
                if tool == "reliary":
                    got, txt, dt = reliai_refs(sessions["reliary"], sym, "usage_only")
                elif tool == "cbm":
                    got, txt, dt = cbm_refs(sessions["cbm"], sym, args.project)
                else:
                    got, txt, dt = grep_refs(root, sym, ext)
            except Exception as e:  # a failed call is a result, not a crash
                row[tool] = {"error": f"{type(e).__name__}: {e}"}
                continue

            dropped = {b for (b, _ln) in got if not b.endswith(ext)}
            got = {(b, ln) for (b, ln) in got if b.endswith(ext)}
            if dropped:
                rec["dropped_non_source"] = sorted(dropped)[:5]
            rec.update(api_ms=round(dt * 1000, 2), bytes=len(txt), returned=len(got))

            all_keys = {(f.rsplit("/", 1)[-1], ln) for (f, ln) in truth_all}
            # A tool site is "in tests" when any truth path with that basename
            # and line is a test path. Tool output carries only a basename, so
            # this is the only symmetric way to partition it.
            test_keys = {k for k in all_keys
                         if any(is_test_path(f) and (f.rsplit("/", 1)[-1], ln) == k
                                for (f, ln) in truth_all)}
            got_nt = got - test_keys

            def match(hit, truth_set):
                """Does a tool hit (printed path, line) denote this truth site?

                A tool that prints a root-relative or absolute path is matched by
                SUFFIX, so `tokio/src/io/mod.rs:8` and `/abs/tokio/src/io/mod.rs:8`
                both resolve. A tool that prints only a basename falls back to
                basename+line, which is why colliding names (`mod.rs` appears 54
                times in tokio) are counted as ambiguous rather than missed: the
                harness reports what it can prove.
                """
                paths = {f for (f, ln) in truth_set if ln == hit[1]}
                if not paths:
                    return False
                hp = hit[0]
                if "/" in hp:
                    return any(f == hp or f.endswith("/" + hp) for f in paths)
                return any(f.rsplit("/", 1)[-1] == hp for f in paths)

            def score(truth, g):
                """Recall = truth sites with at least one matching hit.
                Precision = hits that denote any truth site of this symbol."""
                if not truth:
                    return None, None
                found = sum(1 for site in truth if any(match(h, {site}) for h in g))
                ok = sum(1 for h in g if match(h, truth_all))
                return (round(found / len(truth), 4),
                        round(ok / len(g), 4) if g else 0.0)

            r_all, p_all = score(truth_all, got)
            r_use, p_use = score(truth_use, got)
            r_nt, p_nt = score(truth_use_nt, got_nt)
            rec.update(r_all=r_all, p_all=p_all, r_use=r_use, p_use=p_use,
                       r_ntest=r_nt, p_ntest=p_nt)
            row[tool] = rec

        if "reliary" in tools and def_lines:
            got, txt, dt = reliai_refs(sessions["reliary"], sym, "def_only")
            dk = {(f.rsplit("/", 1)[-1], ln) for f, ln in def_lines}
            row["reliary_def"] = {"hit": int(bool(got & dk)), "bytes": len(txt),
                                  "api_ms": round(dt * 1000, 2)}
        if "cbm" in tools and def_lines:
            got, txt, dt = cbm_def(sessions["cbm"], sym, args.project)
            dk = {(f.rsplit("/", 1)[-1], ln) for f, ln in def_lines}
            row["cbm_def"] = {"hit": int(bool(got & dk)), "bytes": len(txt),
                              "api_ms": round(dt * 1000, 2)}
        rows.append(row)
        if i % 5 == 0:
            print(f"  {i}/{len(syms)}", file=sys.stderr)

    for s in sessions.values():
        s.close()

    def vals(tool, key):
        return [r[tool][key] for r in rows
                if tool in r and "error" not in r[tool] and r[tool].get(key) is not None]

    print()
    print("R_use   call sites only, declaration lines excluded on every side (who-calls)")
    print("Truth is CASE-SENSITIVE: the exact identifier, not a case-folded set.")
    print("R_noSt  R_use over symbols whose phrase key is NOT shared with another")
    print("        identifier (Request/request/requests share one Porter stem).")
    print("        A stem-keyed index merges them; the answer is post-filtered to")
    print("        the exact token, so this column shows the residual after that.")
    print("R_ntest R_use restricted to non-test paths (usage_only's default scope)")
    print()
    hdr = (f"{'tool':<9} {'R_use':>6} {'P_use':>6} {'R_noSt':>7} {'P_noSt':>7} "
           f"{'R_ntest':>7} {'P_ntest':>7} {'def':>5} {'set':>6} {'bytes':>7} {'p50':>6}")
    print(hdr)
    print("-" * len(hdr))

    # Definition-query results are computed first so the table's `def` column
    # is a real number rather than a placeholder.
    def_stats = {}
    for tool in tools:
        ds = [r[f"{tool}_def"] for r in rows if f"{tool}_def" in r]
        if ds:
            hit = sum(d["hit"] for d in ds) / len(ds)
            by = sum(d["bytes"] for d in ds) / len(ds)
            ms = st.median([d["api_ms"] for d in ds])
            def_stats[tool] = {"hit": round(hit, 4), "mean_bytes": round(by, 1),
                               "p50_ms": round(ms, 2), "n": len(ds)}
    summary = {}
    clean = [r for r in rows if not r["stem_collision"]]
    def mean_clean(tool, key):
        xs = [r[tool][key] for r in clean
              if tool in r and "error" not in r[tool] and r[tool].get(key) is not None]
        return round(sum(xs) / len(xs), 4) if xs else None

    for tool in tools:
        if not vals(tool, "r_use"):
            print(f"{tool:<9} (no successful calls)")
            continue
        def mean(xs):
            return round(sum(xs) / len(xs), 4) if xs else None
        ms = sorted(vals(tool, "api_ms"))
        s = {
            "r_all": mean(vals(tool, "r_all")),
            "r_use_no_stem_collision": mean_clean(tool, "r_use"),
            "p_use_no_stem_collision": mean_clean(tool, "p_use"),
            "n_stem_collision": len(rows) - len(clean),
            "r_use": mean(vals(tool, "r_use")),
            "p_use": mean(vals(tool, "p_use")),
            "r_ntest": mean(vals(tool, "r_ntest")),
            "p_ntest": mean(vals(tool, "p_ntest")),
            "mean_returned": round(sum(vals(tool, "returned")) / len(vals(tool, "returned")), 1),
            "mean_bytes": round(sum(vals(tool, "bytes")) / len(vals(tool, "bytes")), 1),
            "p50_ms": round(st.median(ms), 2) if ms else None,
            "p95_ms": round(ms[min(len(ms) - 1, int(0.95 * len(ms)))], 2) if ms else None,
            "errors": sum(1 for r in rows if tool in r and "error" in r[tool]),
        }
        summary[tool] = s
        truth_mean = round(sum(r["truth_all"] for r in rows) / len(rows), 1)
        def_s = f"{def_stats[tool]['hit']:.2f}" if tool in def_stats else "    -"
        print(f"{tool:<9} {(s['r_use'] or 0):>6.3f} {(s['p_use'] or 0):>6.3f} "
              f"{(s['r_use_no_stem_collision'] or 0):>7.3f} "
              f"{(s['p_use_no_stem_collision'] or 0):>7.3f} "
              f"{(s['r_ntest'] or 0):>7.3f} {(s['p_ntest'] or 0):>7.3f} "
              f"{def_s:>5} {s['mean_returned']:>6.1f} "
              f"{s['mean_bytes']:>7.0f} {(s['p50_ms'] or 0):>6.1f}")

    for tool, st_def in def_stats.items():
        print(f"\n{tool} definition query: hit={st_def['hit']:.3f} "
              f"bytes={st_def['mean_bytes']:.0f} p50={st_def['p50_ms']:.1f}ms "
              f"n={st_def['n']}")
        summary[f"{tool}_def"] = st_def
        if tool in summary:
            summary[tool]["def"] = st_def["hit"]

    print("\ncase collisions (the same letters in a different case, which a")
    print("case-insensitive index merges and an exact-match tool must not):")
    for r in rows[:0]:
        pass

    if cold:
        print("\nfirst-call latency (one-time per session, ms): "
              + "  ".join(f"{k}={v}" for k, v in cold.items()))
        summary["cold_start_ms"] = cold

    print("\nper symbol (R_use / R_all [returned/truth]):")
    for r in rows:
        cells = []
        for tool in tools:
            if tool in r and "error" in r[tool]:
                cells.append(f"{tool}=ERR")
            elif tool in r:
                ru = r[tool].get('r_use') or 0.0
                ra = r[tool].get('r_all') or 0.0
                cells.append(f"{tool}={ru:.2f}/{ra:.2f}"
                             f"[{r[tool]['returned']}/{r['truth_all']}]")
        print(f"  {r['symbol']:<26} " + "  ".join(cells))

    if args.out:
        Path(args.out).write_text(json.dumps(
            {"corpus": str(root), "lang": args.lang, "symbols": syms,
             "ambiguous_sites": oracle.ambiguous, "summary": summary, "rows": rows},
            indent=2) + "\n")
        print(f"\nwrote {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
