#!/usr/bin/env python3
"""V64: Deterministic fact-verification bench.

Replaces the LLM judge as the PRIMARY score. Extracts (symbol, file, line)
claims from model answers and the ground truth, verifies each claim against
reliary's own index (occurrence table), and scores precision/recall/F1.

Zero LLM, zero variance, near-zero cost. The tool verifies its own answers
against the same index the model used.

Usage:
  python3 bench/deterministic_verify.py --input bench/results/v63g_deadscope_prompt.jsonl
"""
import argparse
import json
import os
import re
import sqlite3
import statistics as st
import sys

sys.path.insert(0, os.path.dirname(__file__))
from reliary_judge_gt import GROUND_TRUTH

# --- Fact extraction -------------------------------------------------------

# M1: claim extractor broadened for format fairness across A/B/C.
# Accepts every citation form the three conditions actually emit:
#   A:   "X is defined at file.rs:31", "name (file.rs:27)", "file (lines 8, 56)"
#   B:   "name — file.rs:26", "file.rs:17-23"
#   C:   "impl block at file.rs:25", then "line 37" (file from header context)
# Also: multi-language extensions, ranges, bare "line N" with current-file inherit.
_ENGLISH_STOP = {
    "is", "at", "in", "the", "a", "an", "and", "or", "of", "to", "for", "on",
    "with", "as", "by", "from", "that", "this", "it", "its", "be", "are",
    "was", "were", "has", "have", "had", "not", "no", "yes", "if", "but",
    "defined", "declared", "located", "found", "called", "returns", "return",
    "takes", "accepts", "impl", "block", "line", "lines", "public", "private",
    # GT prose must not invent symbol names: "primary definition (file.rs:31)"
    # extracted symbol=`definition`, which no answer could ever match.
    "definition", "function", "method", "methods", "field", "fields",
    "struct", "enum", "trait", "type", "types", "derive", "derives",
    "implements", "implementation", "primary", "signature", "body",
    "parameter", "parameters", "argument", "arguments", "caller", "callers",
    "callee", "callees", "symbol", "name", "identifier",
}
# Source extensions (grammar-free: no language detection, just citation shapes)
_EXT = r"(?:rs|py|go|ts|tsx|js|jsx|c|cc|cpp|h|hpp|java|rb|php|cs|kt|swift|scala)"
_SYM = r"[A-Za-z_][A-Za-z0-9_]*"
# "symbol at/in file.ext:31" / "symbol at file.ext line 31"
SYM_AT_FILE_LINE = re.compile(
    rf"({_SYM})(?:::{_SYM})?\s+(?:at|in)\s+"
    rf"([A-Za-z0-9_./-]+\.{_EXT})(?::(\d+)|\s+line\s+(\d+))"
)
# "X is defined at file.ext:31" / "X is declared in file.ext:31"
SYM_IS_LOCATED = re.compile(
    rf"\b({_SYM})\s+(?:is\s+)?(?:defined|declared|located|found)\s+"
    rf"(?:at|in)\s+([A-Za-z0-9_./-]+\.{_EXT}):(\d+)"
)
# bare "file.ext:31" (ranges: file.ext:17-23 captures 17)
FILE_LINE = re.compile(rf"([A-Za-z0-9_./-]+\.{_EXT}):(\d+)")
# "(file.ext:31)" — methods paren form; symbol association handled separately
PAREN_FILE_LINE = re.compile(rf"\(([A-Za-z0-9_./-]+\.{_EXT}):(\d+)\)")
# "name (file.ext:27)" / "fn name (file.ext:27)" — symbol-associated methods
SYM_PAREN_FILE_LINE = re.compile(
    rf"\b({_SYM})\s+\(([A-Za-z0-9_./-]+\.{_EXT}):(\d+)\)"
)
# "name — file.rs:26" (B em-dash / spaced hyphen separator).
# ASCII hyphen MUST have surrounding spaces — otherwise `reliary-search/src/x.rs:16`
# is misparsed as symbol=`reliary` + file=`search/src/x.rs`.
SYM_DASH_FILE_LINE = re.compile(
    rf"\b({_SYM})\s*[—–]\s*([A-Za-z0-9_./-]+\.{_EXT}):(\d+)"
    rf"|\b({_SYM})\s+-\s+([A-Za-z0-9_./-]+\.{_EXT}):(\d+)"
)
# "in file.ext at line(s) N, M" / "file.ext at lines 45-46, 70-71"
_LINE_NUM = r"(\d+(?:\s*[-–]\s*\d+)?(?:\s*,\s*\d+(?:\s*[-–]\s*\d+)?)*)"
FILE_AT_LINES = re.compile(
    rf"([A-Za-z0-9_./-]+\.{_EXT})\s+(?:at\s+)?lines?\s+{_LINE_NUM}"
)
# "file.ext (lines 8, 56 in fn ...)" / "file.ext (line 19)" / "file.ext (line 17-23)"
FILE_PAREN_LINES = re.compile(
    rf"([A-Za-z0-9_./-]+\.{_EXT})\s*\(\s*lines?\s+{_LINE_NUM}"
)
# "defined in file.ext at line 154"
SYM_IN_FILE_LINE = re.compile(
    rf"({_SYM})\s+(?:is\s+)?defined\s+in\s+"
    rf"([A-Za-z0-9_./-]+\.{_EXT})\s+at\s+line\s+(\d+)"
)
# "file.ext line 93" / "file.ext, line 93" (no "at")
FILE_WORD_LINE = re.compile(
    rf"([A-Za-z0-9_./-]+\.{_EXT})\s*,?\s+line\s+(\d+)"
)
# bare "line 37" / "lines 45-46" — inherits current-file context (C format)
BARE_LINE = re.compile(r"\blines?\s+(\d+(?:\s*[-–]\s*\d+)?(?:\s*,\s*\d+(?:\s*[-–]\s*\d+)?)*)")
# any source-file mention (no line required) — seeds current-file for bare lines
FILE_MENTION = re.compile(rf"([A-Za-z0-9_./-]+\.{_EXT})\b")
# "scan_delimiters — line 603" (symbol then bare line, file from context)
SYM_DASH_BARE_LINE = re.compile(rf"\b({_SYM})\s*[—–-]\s*(?:line\s+)?(\d+)\b")
# "test_rust_fn_def (989), test_python_def (1024)" — symbol then a bare
# line number in parens, with the file inherited from a section header
# (all three conditions emit this when listing many callers under one file).
SYM_PAREN_BARE_LINE = re.compile(rf"\b({_SYM})\s*\(\s*(\d+(?:\s*,\s*\d+)*)\s*\)")


def _is_symbol_shape(cand):
    """True for identifier shapes that code answers actually cite.

    Rejects bare English lowercase words (`usage`, `finds`, `before`) that
    happen to sit next to `in file.rs:N` — those became phantom claims and
    scored as false positives. Accepts snake_case, Capitalized/PascalCase,
    and ALL_CAPS acronyms.
    """
    # Case-insensitive: sentence-initial prose ("Struct defined at file.rs:16")
    # must not become a symbol claim. English stop words are never symbols.
    if not cand or cand.lower() in _ENGLISH_STOP:
        return False
    if "_" in cand:
        return True
    # Capitalized word or PascalCase (`Default`, `OpEntry`, `FileInfo`)
    if cand[0].isupper() and any(c.islower() for c in cand):
        return True
    # ALL_CAPS acronym (`ID`, `URL`, `HTTP`)
    if cand.isupper() and len(cand) >= 2:
        return True
    # bare lowercase single-word: drop (English vs bare fn name is
    # ambiguous without a keyword list — file-only claim still verifies
    # the location, which is the precision-critical half).
    return False


def _expand_line_nums(spec):
    """Expand '45-46, 70-71' or '8, 56' into individual ints (capped)."""
    out = []
    for part in re.split(r"\s*,\s*", spec.strip()):
        m = re.match(r"^(\d+)\s*[-–]\s*(\d+)$", part)
        if m:
            a, b = int(m.group(1)), int(m.group(2))
            if a > b:
                a, b = b, a
            # Cap ranges — huge spans are prose, not precise claims
            if b - a <= 20:
                out.extend(range(a, b + 1))
            else:
                out.append(a)
                out.append(b)
        elif part.isdigit():
            out.append(int(part))
    return out


def extract_facts(text):
    """Return set of (symbol, basename, line) claims from answer/GT text.

    M1: format-symmetric. Every condition's citation style yields the same
    (symbol?, file, line) triples when the answer is factually identical.
    Bare "line N" inherits the most recently mentioned file on that line
    (C often cites the file once in a header, then bare line numbers).
    """
    text = text.replace("`", "")  # strip markdown backticks — symmetric
    facts = set()
    for m in SYM_AT_FILE_LINE.finditer(text):
        sym = m.group(1)
        path = m.group(2)
        line = int(m.group(3) or m.group(4))
        if not _is_symbol_shape(sym):
            sym = ""
        facts.add((sym, os.path.basename(path), line))
    for m in SYM_IS_LOCATED.finditer(text):
        sym, path, line = m.group(1), m.group(2), int(m.group(3))
        if not _is_symbol_shape(sym):
            sym = ""
        facts.add((sym, os.path.basename(path), line))
    for m in SYM_PAREN_FILE_LINE.finditer(text):
        sym, path, line = m.group(1), m.group(2), int(m.group(3))
        if not _is_symbol_shape(sym):
            sym = ""
        facts.add((sym, os.path.basename(path), line))
    for m in SYM_DASH_FILE_LINE.finditer(text):
        if m.group(1) is not None:
            sym, path, line = m.group(1), m.group(2), int(m.group(3))
        else:
            sym, path, line = m.group(4), m.group(5), int(m.group(6))
        if not _is_symbol_shape(sym):
            sym = ""
        facts.add((sym, os.path.basename(path), line))
    for m in FILE_LINE.finditer(text):
        path, line = m.group(1), int(m.group(2))
        facts.add(("", os.path.basename(path), line))
    for m in PAREN_FILE_LINE.finditer(text):
        path, line = m.group(1), int(m.group(2))
        facts.add(("", os.path.basename(path), line))
    for m in SYM_IN_FILE_LINE.finditer(text):
        sym, path, line = m.group(1), m.group(2), int(m.group(3))
        if not _is_symbol_shape(sym):
            sym = ""
        facts.add((sym, os.path.basename(path), line))
    for m in FILE_AT_LINES.finditer(text):
        path = os.path.basename(m.group(1))
        for n in _expand_line_nums(m.group(2)):
            facts.add(("", path, n))
    for m in FILE_PAREN_LINES.finditer(text):
        path = os.path.basename(m.group(1))
        for n in _expand_line_nums(m.group(2)):
            facts.add(("", path, n))
    for m in FILE_WORD_LINE.finditer(text):
        path, line = m.group(1), int(m.group(2))
        facts.add(("", os.path.basename(path), line))
    # Bare "line N" with current-file inherit — line-scoped, not whole-answer
    # (a later section's file must not claim an earlier section's bare lines).
    # current_file seeds from ANY file mention on the line (with or without :N).
    for block in re.split(r"\n\s*\n|\n(?=[A-Z][^\n]{0,80}\n)", text):
        current = None
        for ln in block.splitlines():
            fm = list(FILE_MENTION.finditer(ln))
            if fm:
                current = os.path.basename(fm[-1].group(1))
            for m in BARE_LINE.finditer(ln):
                if current is None:
                    continue
                span = m.span()
                prefix = ln[: span[0]]
                if re.search(rf"\.{_EXT}:(\d+)?$", prefix) or re.search(
                    rf"\.{_EXT}\s*,?\s+lines?$", prefix
                ):
                    continue
                for n in _expand_line_nums(m.group(1)):
                    facts.add(("", current, n))
            # "name — line 603" with file from earlier line in block
            if current is None:
                continue
            for m in SYM_DASH_BARE_LINE.finditer(ln):
                # Skip if this is "name — file:line" (has a file between dash and number)
                between = ln[m.start(2) - 20 : m.start(2)]
                if re.search(rf"\.{_EXT}:", ln[m.start() : m.end() + 10]):
                    continue
                # Skip if the number is part of "file:LINE" on this line
                if FILE_LINE.search(ln) and f":{m.group(2)}" in ln:
                    continue
                sym = m.group(1)
                if sym.lower() in _ENGLISH_STOP:
                    continue
                # Only treat as bare-line inherit when no file appears after the dash
                after_dash = ln[m.end(1) : m.start(2)]
                if re.search(rf"\.{_EXT}", after_dash):
                    continue
                facts.add((sym, current, int(m.group(2))))
            # "test_foo (989), test_bar (1024)" under a file header — the
            # file context is on an earlier line in the block.
            for m in SYM_PAREN_BARE_LINE.finditer(ln):
                sym = m.group(1)
                if not _is_symbol_shape(sym):
                    continue
                # Skip "name (file.rs:12)" — that IS a file:line, handled above.
                if re.search(rf"\.{_EXT}", ln[m.start(2) : m.end(2) + 6]):
                    continue
                for n in _expand_line_nums(m.group(2)):
                    facts.add((sym, current, n))
    # One citation = one claim: if (sym, file, line) exists, drop ("", file, line).
    # Otherwise SYM_IS_LOCATED + FILE_LINE double-count the same span and skew P/R.
    located = {(b, l) for (s, b, l) in facts if s}
    facts = {
        (s, b, l) for (s, b, l) in facts if s or (b, l) not in located
    }
    # List form: "X is called from a.rs:1, b.rs:2, c.rs:3" — the symbol is
    # stated once as the subject and bare file:line entries follow. Conservative
    # and grammar-free: require >=2 citations on the line AND exactly one
    # symbol-shaped token; a prose lead-in ("See foo.rs:1") has one citation
    # and is therefore NOT credited.
    for ln in text.splitlines():
        cites = list(FILE_LINE.finditer(ln))
        if len(cites) < 2:
            continue
        # Exclude tokens that are part of a file path (basenames like
        # `convert_hf_to_gguf` would otherwise count as symbols).
        path_spans = [m.span(1) for m in cites]
        cands = []
        for t in re.finditer(rf"({_SYM})", ln):
            if any(s <= t.start(1) < e for s, e in path_spans):
                continue
            val = t.group(1)
            if _is_symbol_shape(val) and val not in cands:
                cands.append(val)
        if len(cands) != 1:
            continue
        sym = cands[0]
        for m in cites:
            base = os.path.basename(m.group(1))
            line = int(m.group(2))
            if ("", base, line) in facts:
                facts.discard(("", base, line))
                facts.add((sym, base, line))
    return facts


# --- Index verification ---------------------------------------------------

def open_index(corpus):
    db_path = os.path.join(corpus, ".reliary", "index.sqlite")
    if not os.path.exists(db_path):
        raise SystemExit(f"no index at {db_path} — run `reliary trust` first")
    return sqlite3.connect(db_path)


def load_occurrence(db):
    """Load (phrase, file_basename, line) triples for all def+usage rows.
    ~200K rows on this corpus — fine in memory."""
    occ = set()
    cur = db.execute(
        "SELECT p.phrase, f.file_path, o.line FROM occurrence o "
        "JOIN phrases p ON p.id = o.phrase_id "
        "JOIN file_map f ON f.id = o.file_id"
    )
    for phrase, path, line in cur:
        occ.add((phrase, os.path.basename(path), line))
    return occ


# V66: bare-symbol extraction for q4/q9/q10-style facts. A GT may name symbols
# without file:line (callee lists, impl lists, dead-code lists). These verify as
# "symbol exists as a def in the index" — deterministic, symmetric.
BARE_SYM = re.compile(r"\b([a-z_][a-z0-9_]{4,40})\b")
PASCAL_SYM = re.compile(r"\b([A-Z][A-Za-z0-9]{2,40})\b")


def extract_bare_symbols(text, stop=None):
    """Lowercased identifier-like symbol names mentioned in text.

    V77: two filters keep English prose out of claims (q4 forensic: prose
    words like "appearing"/"before" were counted as claims, tanking
    precision to 0.25 while all real helpers verified):
    1. Strip file-extension citation spans (foo.rs:12, bar.py) first —
       they are locations, and their basenames are not symbol claims.
    2. Keep only identifier-shaped tokens: snake_case (contains `_`) or
       PascalCase with an internal capital (`OpEntry`, not `The`/`Std`).
    """
    if stop is None:
        stop = STOPWORDS
    # Locations, not symbols: "op_table.rs:27", "full_file.rs", "a.b.3".
    text = re.sub(r"[A-Za-z0-9_/-]+\.[A-Za-z]{1,5}(?::\d+)?", " ", text)
    syms = set()
    for m in BARE_SYM.finditer(text):
        s = m.group(1)
        if "_" not in s:
            continue
        if s not in stop:
            syms.add(s)
    for m in PASCAL_SYM.finditer(text):
        s = m.group(1)
        if not any(c.isupper() for c in s[1:]):
            continue
        if s.lower() not in stop:
            syms.add(s.lower())
    return syms


STOPWORDS = {
    "structural", "result", "crate", "index", "tools", "function", "pub", "self",
    "with", "from", "that", "this", "line", "file", "path", "true", "false",
    "which", "where", "these", "those", "them", "then", "when", "what", "list",
    "each", "give", "name", "find", "call", "calls", "called", "callers",
    "called_from", "defined", "definition", "returns", "return", "string",
    "option", "bool", "vec", "string_type", "brace", "graph", "node", "fields",
    "field", "methods", "method", "public", "struct", "impl", "default",
    "workspace", "corpus", "bench", "answer", "correct", "facts", "fact",
    "exist", "exists", "count", "counts", "rows", "sites", "site", "module",
    "modules", "tree", "tests", "test", "keyword", "keywords", "source",
    "validated", "audited", "candidates", "candidate", "evidence", "zero",
    "occurrences", "occurrence", "plain", "data", "free", "functions",
    "function_name", "helps", "helper", "helpers", "constructs", "tries",
    "exact", "query", "queries", "fallback", "scan", "scans", "primary",
    "ranks", "first", "second", "third", "internally", "internal", "strategy",
    "strategies", "chain", "order",
    # V66c: struct field names and std methods are not "helpers"
    "tag", "is_def", "defined_name", "start_line", "end_line", "role",
    "first_line_text", "children", "trim_start", "starts_with", "len",
    "callee",
}


def load_defs(db):
    """Set of (phrase_lower) that exist as defs (is_def=1, tag in 1,2) in source files."""
    defs = set()
    cur = db.execute(
        "SELECT DISTINCT p.phrase FROM occurrence o "
        "JOIN phrases p ON p.id = o.phrase_id "
        "JOIN file_map f ON f.id = o.file_id "
        "WHERE o.is_def = 1 AND o.tag IN (1, 2) AND f.is_source = 1"
    )
    for (phrase,) in cur:
        defs.add(phrase.lower())
    return defs


def load_dead_symbols(db, corpus):
    """Set of phrase_lower that are defs with zero non-def occurrences in source files."""
    dead = set()
    cur = db.execute(
        """SELECT p.phrase, o.file_id, o.line FROM occurrence o
           JOIN phrases p ON p.id = o.phrase_id
           WHERE o.is_def = 1 AND o.tag = 1"""
    )
    for phrase, file_id, line in cur:
        if phrase.lower() in dead:
            continue
        n = db.execute(
            """SELECT COUNT(*) FROM occurrence o
               JOIN file_map f ON f.id = o.file_id
               WHERE o.phrase_id = (SELECT id FROM phrases WHERE phrase = ?1)
                 AND o.is_def = 0 AND f.is_source = 1""",
            (phrase,),
        ).fetchone()[0]
        if n == 0:
            dead.add(phrase.lower())
    return dead


def verify_facts(facts, occ, tol=1, corpus=None):
    """Return (verified, false, unverifiable) counts.

    M1: file-only claims (no symbol) must hit an occurrence at file:line±tol,
    not merely "file exists" — the old check gave free precision to any answer
    that named a real file.
    """
    verified = 0
    false = 0
    unverifiable = 0
    for sym, base, line in facts:
        if not sym:
            # file:line-only claim — require an occurrence near that line
            hit = any(b == base and abs(l - line) <= tol for (_, b, l) in occ)
            if hit:
                verified += 1
            else:
                false += 1
            continue
        # symbol claim — check (phrase, file, line±tol) exists
        hit = any(
            p == sym and b == base and abs(l - line) <= tol
            for (p, b, l) in occ
        )
        if hit:
            verified += 1
        else:
            # symbol may be stemmed differently — try case-insensitive
            hit2 = any(
                p.lower() == sym.lower() and b == base and abs(l - line) <= tol
                for (p, b, l) in occ
            )
            if hit2:
                verified += 1
            else:
                # Noise-keyword symbols (`new`, `bool`, ...) are deliberately
                # dropped from the occurrence table. A correct claim would
                # score false — fall back to the source line itself.
                if corpus and _source_has_symbol(corpus, base, line, sym, _src_cache):
                    verified += 1
                elif corpus and _is_enclosing_function(corpus, base, line, sym, _src_cache):
                    # Caller claims cite the ENCLOSING function, not the callee
                    # on the call line: "predict_role at type_flow.rs:42" means
                    # line 42 is inside fn predict_role. Grammar-free: scan
                    # backward for a definition-shaped line naming `sym`.
                    verified += 1
                else:
                    false += 1
    return verified, false, unverifiable


_src_cache: dict = {}


def _load_source_lines(corpus, base, cache):
    if base not in cache:
        found = None
        for root, dirs, files in os.walk(corpus):
            dirs[:] = [d for d in dirs if not d.startswith(".") and d not in
                       ("target", "node_modules", "bench", "docs")]
            if base in files:
                found = os.path.join(root, base)
                break
        if found is None:
            cache[base] = None
        else:
            try:
                with open(found, encoding="utf-8", errors="replace") as fh:
                    cache[base] = fh.read().splitlines()
            except OSError:
                cache[base] = None
    return cache[base]


def _is_enclosing_function(corpus, base, line, sym, cache, max_scan=400):
    """True if `sym` is the enclosing function of source line (1-indexed).

    Grammar-free: walk backward from the claimed line; the first
    definition-shaped line (`fn sym`, `sym(`, `def sym`, `func sym`) whose
    name is `sym` proves the claim. Stops at any other definition-shaped
    line first (that would be a nearer enclosing scope).
    """
    lines = _load_source_lines(corpus, base, cache)
    if not lines:
        return False
    idx = min(line - 1, len(lines) - 1)
    if idx < 0:
        return False
    # The claimed line must be an executable call site, not a blank or a
    # declaration-only line — otherwise the claim is "X is near line N",
    # which is not what a caller citation asserts.
    if "(" not in lines[idx]:
        return False
    # definition-shaped: identifier followed by `(` — no keyword list
    def_re = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*[\(<]")
    sym_l = sym.lower()
    scanned = 0
    while idx >= 0 and scanned < max_scan:
        s = lines[idx]
        stripped = s.strip()
        if stripped and not stripped.startswith(("//", "#", "*", "/*")):
            for m in def_re.finditer(s):
                name = m.group(1)
                if name.lower() == sym_l:
                    return True
                # a different definition-shaped name here is a nearer scope
                if idx < line - 1 and not name.lower() in (
                    "if", "while", "for", "match", "switch", "return",
                    "sizeof", "typeof", "catch", "function",
                ):
                    # only treat as scope boundary when it looks like a def
                    # (has `(` and is not a call mid-expression — heuristic:
                    # starts near beginning of line after optional modifiers)
                    prefix = s[: m.start(1)].strip()
                    if prefix in ("", "pub", "pub(crate)", "async", "unsafe",
                                  "extern", "fn", "def", "func", "function",
                                  "const", "static", "final", "void",
                                  "public", "private", "protected"):
                        if name.lower() != sym_l:
                            return False
        idx -= 1
        scanned += 1
    return False


def _source_has_symbol(corpus, base, line, sym, cache, tol=1):
    """True if `sym` appears as a whole token on source line (1-indexed, ±tol)."""
    lines = _load_source_lines(corpus, base, cache)
    if not lines:
        return False
    pat = re.compile(rf"(?<![A-Za-z0-9_]){re.escape(sym)}(?![A-Za-z0-9_])")
    for off in range(-tol, tol + 1):
        idx = line - 1 + off
        if 0 <= idx < len(lines) and pat.search(lines[idx]):
            return True
    return False


# --- Scoring ---------------------------------------------------------------

def score_query(answer, gt_text, occ, tol=1, defs=None, dead=None, query_id="",
                corpus=None):
    ans_facts = extract_facts(answer)
    gt_facts = extract_facts(gt_text)

    # V66: symbol-level fact types for GTs that name symbols without file:line.
    #   - "symbol-exists" (q4/q9): named symbol must exist as a def in the index
    #   - "dead-symbol" (q10): named symbol must be a def with zero callers
    # Both are deterministic and symmetric across conditions.
    sym_level = defs is not None and (not gt_facts or query_id == "q4_callgraph_predict_role")
    if sym_level:
        ans_syms = extract_bare_symbols(answer)
        gt_syms = extract_bare_symbols(gt_text)
        # q4 (and any symbol-exists query): precision = cited helpers that exist
        # as defs; recall = GT helpers cited
        if not gt_syms:
            return 0.0, 0.0, 0.0, 0, 0, 0
        if not ans_syms:
            return 0.0, 0.0, 0.0, 0, 0, len(gt_syms)
        good = sum(1 for s in ans_syms if s in defs)
        precision = good / len(ans_syms)
        found = sum(1 for s in gt_syms if s in ans_syms)
        recall = found / len(gt_syms)
        f1 = 2 * precision * recall / (precision + recall) if (precision + recall) else 0.0
        return precision, recall, f1, len(ans_syms), good, len(gt_syms)

    # Precision: claims in the answer that verify
    verified, false, _ = verify_facts(ans_facts, occ, corpus=corpus)
    precision = verified / len(ans_facts) if ans_facts else 0.0

    # Recall: GT facts (with file:line) present in the answer
    # A GT fact is "present" if the answer contains the same (file, line±tol)
    # with the same symbol (or a case-insensitive match). File-only GT facts
    # (no symbol) count if the answer cites the same file at line±tol.
    #
    # Caller-list exception: for a "who calls X / where is X used" question the
    # GT symbol on every fact is the *queried subject* (X), while a correct
    # answer cites the *enclosing caller* at that site (`f.rs:42 in fn caller`).
    # Requiring the answer to repeat X on each line scored correct caller lists
    # as 0 recall (found on the tokio familiarity bench, both conditions). When
    # the GT symbol is the question's backticked subject, match on location.
    subj = ""
    m = re.search(r"`([^`]+)`", gt_text)
    if m:
        subj = m.group(1).strip().lower()

    gt_verifiable = gt_facts  # all GT facts with file:line are checkable
    if not gt_verifiable:
        recall = 0.0
    else:
        found = 0
        for sym, base, line in gt_verifiable:
            if sym and sym.lower() != subj:
                present = any(
                    (s.lower() == sym.lower() and b == base and abs(l - line) <= tol)
                    for (s, b, l) in ans_facts
                )
            else:
                present = any(
                    (b == base and abs(l - line) <= tol)
                    for (s, b, l) in ans_facts
                )
            if present:
                found += 1
        recall = found / len(gt_verifiable)

    f1 = 2 * precision * recall / (precision + recall) if (precision + recall) else 0.0
    return precision, recall, f1, len(ans_facts), verified, len(gt_verifiable)


# --- Main ------------------------------------------------------------------

def load_auto_gt(gt_path):
    """Convert auto_questions.py facts JSON into {qid: prose_with_facts} for score_query.

    `gt[].line` is already 1-indexed (auto_questions counts `j + 1`), as are the
    lines tool output reports. Do not add one.
    """
    with open(gt_path) as fh:
        data = json.load(fh)
    out = {}
    for q in data.get("questions", []):
        parts = []
        for g in q.get("gt", []):
            f = os.path.basename(g["file"])
            parts.append(f"{g['sym']} at {f}:{g['line']}")
        out[q["query_id"]] = (q.get("question", "") + "\nGround truth: " + ", ".join(parts))
    return out


def main():
    global GROUND_TRUTH
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", required=True)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--tol", type=int, default=1)
    ap.add_argument("--gt", default=None, help="auto_questions.py facts JSON (overrides reliary_judge_gt)")
    args = ap.parse_args()

    db = open_index(args.corpus)
    occ = load_occurrence(db)
    defs = load_defs(db)
    dead = load_dead_symbols(db, args.corpus)
    print(f"index: {len(occ)} occurrence rows, {len(defs)} defs, {len(dead)} dead symbols")

    if args.gt:
        GROUND_TRUTH = load_auto_gt(args.gt)
        print(f"GT: {len(GROUND_TRUTH)} queries from {args.gt}")

    runs = []
    with open(args.input) as f:
        for line in f:
            d = json.loads(line)
            if "queries" in d:
                runs.append(d)

    per_cond = {}
    for run in runs:
        cond = run.get("cond", "?")
        seed = run.get("seed", "?")
        q_scores = []
        for q in run.get("queries", []):
            qid = q.get("query_id", "?")
            answer = q.get("answer") or ""
            gt = GROUND_TRUTH.get(qid, "")
            if not gt:
                continue
            p, r, f1, n_claims, n_verified, n_gt = score_query(
                answer, gt, occ, args.tol, defs=defs, dead=dead, query_id=qid,
                corpus=args.corpus,
            )
            q_scores.append((qid, p, r, f1, n_claims, n_verified, n_gt))
        per_cond.setdefault(cond, []).append((seed, q_scores))

    # V64b: claim-weighted aggregation (primary) — total verified / total claimed.
    # Per-query mean poisons precision with zero-claim queries (P=0 despite no
    # false claims) and recall with zero-GT-fact queries. Report both:
    #   weighted P/R/F1 = the primary score
    #   coverage        = share of queries making >=1 verifiable claim
    print(f"\n{'Cond':<6} {'Seed':<6} {'Pw':>6} {'Rw':>6} {'F1w':>6} {'cov':>5}  {'claims':>7} {'verified':>8} {'GT-facts':>9}")
    for cond in sorted(per_cond):
        for seed, q_scores in per_cond[cond]:
            n_claims = sum(s[4] for s in q_scores)
            n_verified = sum(s[5] for s in q_scores)
            gt_queries = [s for s in q_scores if s[6] > 0]
            n_gt = sum(s[6] for s in gt_queries)
            n_found = sum(round(s[2] * s[6]) for s in gt_queries)  # recall*n_gt ≈ found count
            pw = n_verified / n_claims if n_claims else 0.0
            rw = n_found / n_gt if n_gt else 0.0
            f1w = 2 * pw * rw / (pw + rw) if (pw + rw) else 0.0
            cov = sum(1 for s in q_scores if s[4] > 0) / len(q_scores) if q_scores else 0.0
            print(f"{cond:<6} {seed:<6} {pw:>6.2f} {rw:>6.2f} {f1w:>6.2f} {cov:>5.2f}  {n_claims:>7} {n_verified:>8} {n_gt:>9}")

    # Condition means over seeds
    print("\nCondition summary (claim-weighted, seed-averaged):")
    for cond in sorted(per_cond):
        pws, rws, f1ws, covs = [], [], [], []
        for seed, q_scores in per_cond[cond]:
            n_claims = sum(s[4] for s in q_scores)
            n_verified = sum(s[5] for s in q_scores)
            gt_queries = [s for s in q_scores if s[6] > 0]
            n_gt = sum(s[6] for s in gt_queries)
            n_found = sum(round(s[2] * s[6]) for s in gt_queries)
            pw = n_verified / n_claims if n_claims else 0.0
            rw = n_found / n_gt if n_gt else 0.0
            f1w = 2 * pw * rw / (pw + rw) if (pw + rw) else 0.0
            cov = sum(1 for s in q_scores if s[4] > 0) / len(q_scores) if q_scores else 0.0
            pws.append(pw); rws.append(rw); f1ws.append(f1w); covs.append(cov)
        print(f"  {cond:<6} P={st.mean(pws):.3f} R={st.mean(rws):.3f} F1={st.mean(f1ws):.3f} coverage={st.mean(covs):.2f}")

    # Per-query detail for condition A
    print("\nPer-query (condition A, all seeds):")
    for cond in sorted(per_cond):
        if cond != "A":
            continue
        by_qid = {}
        for seed, q_scores in per_cond[cond]:
            for qid, p, r, f1, nc, nv, ng in q_scores:
                by_qid.setdefault(qid, []).append((p, r, f1, nc, nv, ng))
        for qid in sorted(by_qid):
            rows = by_qid[qid]
            p = st.mean([x[0] for x in rows])
            r = st.mean([x[1] for x in rows])
            f1 = st.mean([x[2] for x in rows])
            print(f"  {qid:<32} P={p:.2f} R={r:.2f} F1={f1:.2f}  (claims {rows[0][3]}, verified {rows[0][4]}, GT {rows[0][5]})")


if __name__ == "__main__":
    main()
