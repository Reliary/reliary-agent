#!/usr/bin/env python3
"""Translate a question+GT set from an original corpus onto its obfuscated twin.

Both corpora then receive the *same* task structure (same symbol identities,
same files, same lines) — only lexical names differ. Without this, the question
generator independently picks different symbols on each corpus and the two runs
are not comparable.

Symbols map through the obfuscator's identifier map; file paths through its
file map (relative path -> relative path, re-rooted at the obfuscated corpus).
GT facts whose symbol is a stem rather than a full identifier, or whose file is
not in the map, are dropped (counted and reported) rather than emitted wrong.

Usage:
  python3 bench/translate_questions.py --questions orig_q.json \
      --orig-root "$HOME/src/tokio-orig" --map /tmp/obf_map.json \
      --corpus "$HOME/src/tokio-obf" --out obf_q.json
"""
import argparse
import json
import os
import re
import sys


def translate_text(text, syms, idents, orig_root, corpus, files):
    """Rewrite only the question's real code references.

    The obfuscator also renames identifier-shaped words inside code comments
    (e.g. "Where", "List"), so substituting the *whole* identifier map would
    corrupt English prose. Translate:
      1. every backticked token (the generator's symbol slots),
      2. every bare occurrence of one of this question's GT symbols,
      3. the corpus root and known file paths.
    """
    def bt(m):
        tok = m.group(1)
        return "`" + (syms.get(tok) or idents.get(tok) or tok) + "`"

    text = re.sub(r"`([^`]+)`", bt, text)
    for orig in sorted(syms, key=len, reverse=True):
        text = re.sub(rf"(?<![A-Za-z0-9_]){re.escape(orig)}(?![A-Za-z0-9_])",
                      syms[orig], text)
    text = text.replace(orig_root, corpus)
    for orel, nrel in sorted(files.items(), key=lambda kv: -len(kv[0])):
        text = text.replace(orel, nrel)
        base = orel.rsplit("/", 1)[-1]
        if base != orel:
            text = text.replace(base, nrel.rsplit("/", 1)[-1])
    return text


def resolve_stem(stem, orig_file, line, idents):
    """Resolve a stemmed GT symbol to the real identifier on its source line.

    The phrase table stores some identifiers stemmed (`initializ` for
    `initialize`), so the GT occasionally names a stem. Read the original
    source line, take the identifiers there, and return the pseudonym of the
    one whose lowercased form starts with the stem (longest match wins).
    """
    try:
        with open(orig_file, encoding="utf-8", errors="replace") as fh:
            lines = fh.readlines()
    except OSError:
        return None
    if not (1 <= line <= len(lines)):
        return None
    toks = re.findall(r"[A-Za-z_][A-Za-z0-9_]*", lines[line - 1])
    cands = [t for t in toks if t.lower().startswith(stem.lower()) and t in idents]
    if not cands:
        return None
    cands.sort(key=len, reverse=True)
    return idents[cands[0]]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--questions", required=True)
    ap.add_argument("--orig-root", required=True)
    ap.add_argument("--map", required=True)
    ap.add_argument("--corpus", required=True, help="obfuscated corpus root")
    ap.add_argument("--out", required=True)
    ap.add_argument("--drop-incomplete", action="store_true",
                    help="drop any question with an unmapped fact, so the two "
                         "corpora receive an identical, complete task set")
    args = ap.parse_args()

    with open(args.questions) as fh:
        qd = json.load(fh)
    with open(args.map) as fh:
        m = json.load(fh)
    idents = m["idents"]          # orig identifier -> pseudonym
    files = m["files"]            # orig rel path -> new rel path

    orig_root = os.path.abspath(args.orig_root)
    corpus = os.path.abspath(args.corpus)

    n_q = n_facts = n_dropped = 0
    kept = []
    for q in qd["questions"]:
        # Resolve this question's own symbols (GT facts + backticked slots).
        syms = {}
        new_gt = []
        dropped_here = 0
        for g in q["gt"]:
            ns = idents.get(g["sym"]) or resolve_stem(
                g["sym"], g["file"], g["line"], idents)
            if ns is None:
                dropped_here += 1
                continue
            syms[g["sym"]] = ns
            try:
                rel = os.path.relpath(os.path.abspath(g["file"]), orig_root)
            except ValueError:
                dropped_here += 1
                continue
            nrel = files.get(rel)
            if nrel is None:
                dropped_here += 1
                continue
            new_gt.append({"sym": ns, "file": os.path.join(corpus, nrel),
                           "line": g["line"]})
            n_facts += 1
        for tok in re.findall(r"`([^`]+)`", q["question"]):
            if tok in idents:
                syms.setdefault(tok, idents[tok])
        q["question"] = translate_text(
            q["question"], syms, idents, orig_root, corpus, files)
        n_dropped += dropped_here
        if args.drop_incomplete and dropped_here:
            continue
        q["gt"] = new_gt
        kept.append(q)
    qd["questions"] = kept
    n_q = len(kept)

    qd["repo"] = os.path.basename(corpus.rstrip("/"))
    with open(args.out, "w") as fh:
        json.dump(qd, fh, indent=2)
    print(f"translated {n_q} questions, {n_facts} facts, dropped {n_dropped} "
          f"(unmapped symbol/file) -> {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
