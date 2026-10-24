#!/usr/bin/env python3
"""Convert auto_questions.py output into the session-question format.

`auto_questions.py` emits mechanically-verified GT facts for any repository.
The benchmark runner wants `[{"id", "question", "rubric"}]`. This bridges the
two so a foreign corpus can be measured without hand-written questions.

The rubric is derived from the GT facts' symbols, so the keyword smoke test
stays meaningful; the deterministic verifier is the real scorer and reads the
same facts JSON via `--gt`.

Usage:
  python3 bench/auto_questions_to_session.py questions.json > session.json
"""
import json
import sys


def convert(doc):
    out = []
    for q in doc["questions"]:
        syms = [g["sym"] for g in q.get("gt", [])]
        out.append({
            "id": q["query_id"],
            "question": q["question"],
            "rubric": {
                "accept": syms[:8],
                "min_count": max(1, min_count_for(q)),
                "max_count": max(len(syms), 2) * 2,
            },
        })
    return out


def min_count_for(q):
    # A definition question has one right answer; a list question wants the
    # listed facts. Derive from the GT shape rather than the question wording.
    return 1 if len(q.get("gt", [])) <= 1 else min(len(q["gt"]), 3)


if __name__ == "__main__":
    src = sys.argv[1] if len(sys.argv) > 1 else "-"
    doc = json.load(sys.stdin if src == "-" else open(src))
    json.dump(convert(doc), sys.stdout, indent=2)
    print(file=sys.stderr)
