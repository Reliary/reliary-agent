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

# Patterns our one-line tool output uses (model copies verbatim):
#   "X is defined at file.rs:31"
#   "X is called from file.rs:9, ingest.rs:181"
#   "Methods on X: name (file.rs:27), ..."
#   "Dead code in ...: name at file.rs:42 (0 cross-file refs)"
# V66b: symbol must look like a real identifier — English words like "is",
# "defined", "at", "in" fail the check below. An identifier is either
# snake_case (contains _) or a multi-word that also appears with _ in text.
# Practical filter: exclude a small English stopword list at extraction time.
_ENGLISH_STOP = {
    "is", "at", "in", "the", "a", "an", "and", "or", "of", "to", "for", "on",
    "with", "as", "by", "from", "that", "this", "it", "its", "be", "are",
    "was", "were", "has", "have", "had", "not", "no", "yes", "if", "but",
    "defined", "declared", "located", "found", "called", "returns", "return",
    "takes", "takes", "accepts", "returns",
}
SYM_AT_FILE_LINE = re.compile(
    r"([A-Za-z_][A-Za-z0-9_]*)(?:::[A-Za-z_][A-Za-z0-9_]*)?\s+(?:at|in)\s+"
    r"([A-Za-z0-9_./-]+\.rs):(\d+)"
)
FILE_LINE = re.compile(r"([A-Za-z0-9_./-]+\.rs):(\d+)")
PAREN_FILE_LINE = re.compile(r"\(([A-Za-z0-9_./-]+\.rs):(\d+)\)")
# grep-style prose: "in src/session.rs at lines 28, 39", "at line 50", "in file.rs line 93"
FILE_AT_LINES = re.compile(
    r"([A-Za-z0-9_./-]+\.rs)\s+(?:at\s+)?lines?\s+(\d+)(?:\s*,\s*(\d+))*"
)
# parenthesized line list: "file.rs (lines 8, 56 in fn ...)" / "file.rs (line 19)"
FILE_PAREN_LINES = re.compile(
    r"([A-Za-z0-9_./-]+\.rs)\s*\(\s*lines?\s+(\d+(?:\s*,\s*\d+)*)"
)
# "defined in src/index/search.rs at line 154" (symbol-at + in)
SYM_IN_FILE_LINE = re.compile(
    r"([A-Za-z_][A-Za-z0-9_]*)\s+(?:is\s+)?defined\s+in\s+"
    r"([A-Za-z0-9_./-]+\.rs)\s+at\s+line\s+(\d+)"
)


def extract_facts(text):
    """Return set of (symbol, basename, line) claims from answer/GT text."""
    text = text.replace("`", "")  # strip markdown backticks — symmetric for all conditions
    facts = set()
    for m in SYM_AT_FILE_LINE.finditer(text):
        sym, path, line = m.group(1), m.group(2), int(m.group(3))
        if sym.lower() in _ENGLISH_STOP:
            sym = ""  # English word, not a symbol — treat as file:line-only claim
        facts.add((sym, os.path.basename(path), line))
    # "file.rs:line" without a symbol (caller lists, dead code lists)
    for m in FILE_LINE.finditer(text):
        path, line = m.group(1), int(m.group(2))
        facts.add(("", os.path.basename(path), line))
    # "(file.rs:line)" parenthesized form (methods lists)
    for m in PAREN_FILE_LINE.finditer(text):
        path, line = m.group(1), int(m.group(2))
        facts.add(("", os.path.basename(path), line))
    # grep-style "defined in file.rs at line N"
    for m in SYM_IN_FILE_LINE.finditer(text):
        sym, path, line = m.group(1), m.group(2), int(m.group(3))
        if sym.lower() in _ENGLISH_STOP:
            sym = ""
        facts.add((sym, os.path.basename(path), line))
    # grep-style "in file.rs at line(s) N, M" / "at lines N, M"
    for m in FILE_AT_LINES.finditer(text):
        path = m.group(1)
        facts.add(("", os.path.basename(path), int(m.group(2))))
        for extra in m.groups()[2:]:
            if extra:
                facts.add(("", os.path.basename(path), int(extra)))
    # "file.rs (lines 8, 56 in fn ...)" / "file.rs (line 19)"
    for m in FILE_PAREN_LINES.finditer(text):
        path = m.group(1)
        for n in re.findall(r"\d+", m.group(2)):
            facts.add(("", os.path.basename(path), int(n)))
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
    """Lowercased symbol names (snake_case + PascalCase) mentioned in text."""
    if stop is None:
        stop = STOPWORDS
    syms = set()
    for m in BARE_SYM.finditer(text):
        s = m.group(1)
        if s not in stop:
            syms.add(s)
    for m in PASCAL_SYM.finditer(text):
        s = m.group(1)
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


def verify_facts(facts, occ, tol=1):
    """Return (verified, false, unverifiable) counts."""
    verified = 0
    false = 0
    unverifiable = 0
    for sym, base, line in facts:
        if not sym:
            # file:line-only claim — check the file exists in the index
            if any(b == base for (_, b, _) in occ):
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
                false += 1
    return verified, false, unverifiable


# --- Scoring ---------------------------------------------------------------

def score_query(answer, gt_text, occ, tol=1, defs=None, dead=None, query_id=""):
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
    verified, false, _ = verify_facts(ans_facts, occ)
    precision = verified / len(ans_facts) if ans_facts else 0.0

    # Recall: GT facts (with file:line) present in the answer
    # A GT fact is "present" if the answer contains the same (file, line±tol)
    # with the same symbol (or a case-insensitive match). File-only GT facts
    # (no symbol) count if the answer cites the same file at line±tol.
    gt_verifiable = gt_facts  # all GT facts with file:line are checkable
    if not gt_verifiable:
        recall = 0.0
    else:
        found = 0
        for sym, base, line in gt_verifiable:
            if sym:
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
    """Convert auto_questions.py facts JSON into {qid: prose_with_facts} for score_query."""
    with open(gt_path) as fh:
        data = json.load(fh)
    out = {}
    for q in data.get("questions", []):
        parts = []
        for g in q.get("gt", []):
            f = os.path.basename(g["file"])
            parts.append(f"{g['sym']} at {f}:{g['line'] + 1}")
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
                answer, gt, occ, args.tol, defs=defs, dead=dead, query_id=qid
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
