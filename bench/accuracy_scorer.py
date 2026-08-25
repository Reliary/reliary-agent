#!/usr/bin/env python3
"""
Accuracy scoring for the long-bench answers.

Compares the model's answer against ground-truth (not keywords).
Returns precision, recall, F1, and a 0-3 score that mirrors the keyword rubric.

Ground truth is in bench/ground_truth.json.
"""

import json
import re
from pathlib import Path

GROUND_TRUTH_PATH = Path(__file__).parent / "ground_truth.json"

# Thresholds map F1 → 0-3 (same shape as keyword rubric for comparability).
SCORE_THRESHOLDS = [(0.70, 3), (0.40, 2), (0.15, 1), (0.0, 0)]


def load_ground_truth():
    """Load the ground-truth dict keyed by query id."""
    import sys
    from pathlib import Path
    sys.path.insert(0, str(Path(__file__).parent))
    import ground_truth
    return {
        "q1_consume_impls": ground_truth.q1_consume_impls,
        "q2_consume_callers": ground_truth.q2_consume_callers,
        "q3_block_on_def": ground_truth.q3_block_on_def,
        "q4_block_on_chain": ground_truth.q4_block_on_chain,
        "q5_spawn_callers": ground_truth.q5_spawn_callers,
        "q6_sleep_def": ground_truth.q6_sleep_def,
        "q7_sleep_methods": ground_truth.q7_sleep_methods,
        "q8_bufwriter_write": ground_truth.q8_bufwriter_write,
        "q9_consume_impls_recheck": ground_truth.q9_consume_impls_recheck,
        "q10_dead_code": ground_truth.q10_dead_code,
    }


def extract_symbols_from_answer(answer_text):
    """
    Heuristic: extract Rust-looking identifiers and file:line references from
    free-text answer. Returns (symbol_set, file_line_set).

    Symbols: bare identifiers like `BufWriter::consume`, `Runtime::block_on`,
    `Sleep::poll`. File:line: `path/to/file.rs:123` or `(line 123)` near a `.rs` ref.
    """
    answer = answer_text or ""
    symbols = set()
    file_lines = set()

    # Standard: CamelCase::snake_case OR CamelCase::snake_case (qualified).
    for m in re.finditer(r"\b([A-Z][A-Za-z0-9]*(?:::[a-z_][A-Za-z0-9_]*)+)\b", answer):
        symbols.add(m.group(1))
    # Unqualified method name: standalone snake_case identifiers ≥ 3 chars
    # (matches tool output like "far_future" without "Sleep::" prefix).
    for m in re.finditer(r"\b([a-z_][a-z_0-9]{2,})\b", answer):
        # Only include if NOT a common English word (heuristic: token appears
        # in tool output format context like "at file.rs:123").
        name = m.group(1)
        # Exclude common stopwords that could be false positives.
        if name not in ("the", "and", "for", "are", "not", "has", "was", "but",
                        "can", "all", "any", "new", "get", "set", "add", "run",
                        "use", "try", "see", "way", "key", "end", "may", "let",
                        "pub", "fn", "ref", "mut", "use", "mod", "box", "impl",
                        "trait", "enum", "self", "match", "where", "like",
                        "tokio", "result", "error", "none", "some", "true",
                        "false", "file", "line", "code", "type", "call",
                        "also", "this", "that", "from", "into", "with"):
            symbols.add(name)
    # Also standalone CamelCase (struct names).
    for m in re.finditer(r"\b([A-Z][A-Za-z0-9]{2,})\b", answer):
        symbols.add(m.group(1))
    # file.rs references: capture path that ends in .rs followed by line number
    # (with optional "(line N)" or "(lines N, M)" spacing).
    for m in re.finditer(r"([\w\-/]+\.rs)[^\d]{0,30}(\d+)", answer):
        path = m.group(1)
        line = int(m.group(2))
        norm_path = path
        for prefix in ("tokio/src/", "src/", "tokio/"):
            if norm_path.startswith(prefix):
                norm_path = norm_path[len(prefix):]
                break
        file_lines.add((norm_path, line))
        # Also add basename-only match (tool may output "take.rs" vs "io/util/take.rs")
        if '/' in norm_path:
            base = norm_path.rsplit('/', 1)[-1]
            file_lines.add((base, line))
    return symbols, file_lines


def score_query(query_id, ground_truth, answer_text):
    """
    Score one answer against its ground truth.
    Returns: {"precision", "recall", "f1", "score", "matches", "misses"}
    """
    gtype = ground_truth["type"]
    correct = ground_truth.get("correct", [])
    min_to_pass = ground_truth.get("min_correct_to_pass", 1)

    if not answer_text or len(answer_text) < 5:
        return {"precision": 0, "recall": 0, "f1": 0, "score": 0, "matches": [], "misses": correct}

    if gtype in ("symbol_list", "dead_symbol_list"):
        mentioned_syms, mentioned_fl = extract_symbols_from_answer(answer_text)
        correct_names = {c["name"] for c in correct}
        correct_files = {(c["file"], c["line"]) for c in correct}

        # Match by symbol name OR by file:line. Both count as a hit.
        name_hits = correct_names & mentioned_syms
        fl_hits = {fl for fl in correct_files if fl in mentioned_fl}

        # Combine: any correct item hit by either method counts.
        hit_names = correct_names & mentioned_syms
        # If a symbol matches, also credit the file:line for that record.
        all_hits = set(hit_names)
        for c in correct:
            if c["name"] in mentioned_syms:
                all_hits.add(c["name"])
            if (c["file"], c["line"]) in mentioned_fl:
                all_hits.add(c["name"])

        precision = len(all_hits) / len(mentioned_syms | mentioned_fl) if (mentioned_syms | mentioned_fl) else 0
        recall = len(all_hits) / len(correct) if correct else 0
        f1 = 2 * precision * recall / (precision + recall) if (precision + recall) > 0 else 0

        score = 0
        for thresh, pts in SCORE_THRESHOLDS:
            if f1 >= thresh:
                score = pts
                break

        # Hard floor: if at least min_to_pass correct items are mentioned,
        # bump to at least 1 point. This rewards partial credit.
        if score == 0 and len(all_hits) >= min_to_pass:
            score = 1

        misses = [c["name"] for c in correct if c["name"] not in all_hits]
        return {
            "precision": precision,
            "recall": recall,
            "f1": f1,
            "score": score,
            "matches": sorted(all_hits),
            "misses": misses,
        }

    if gtype == "single_answer":
        # Reward mentioning the correct file:line pair.
        _, mentioned_fl = extract_symbols_from_answer(answer_text)
        correct_files = {(c["file"], c["line"]) for c in correct}
        hits = correct_files & mentioned_fl
        recall = len(hits) / len(correct_files) if correct_files else 0
        precision = 1.0 if hits else 0.0
        f1 = 2 * precision * recall / (precision + recall) if (precision + recall) > 0 else 0
        score = 3 if f1 >= 0.70 else (2 if f1 >= 0.40 else (1 if f1 >= 0.15 else (1 if hits else 0)))
        return {
            "precision": precision,
            "recall": recall,
            "f1": f1,
            "score": score,
            "matches": [f"{f}:{l}" for f, l in hits],
            "misses": [f"{c['file']}:{c['line']}" for c in correct if (c['file'], c['line']) not in hits],
        }

    if gtype == "call_chain":
        # Reward must_include keywords in answer, plus file:line hits.
        answer_lower = (answer_text or "").lower()
        must_include = ground_truth.get("must_include", [])
        must_hits = sum(1 for kw in must_include if kw.lower() in answer_lower)
        must_recall = must_hits / len(must_include) if must_include else 0

        _, mentioned_fl = extract_symbols_from_answer(answer_text)
        correct_files = {(c["file"], c["line"]) for c in correct}
        fl_hits = correct_files & mentioned_fl
        fl_recall = len(fl_hits) / len(correct_files) if correct_files else 0

        recall = (must_recall + fl_recall) / 2
        f1 = recall  # precision = 1.0 if anything mentioned, but we use recall as proxy
        score = 3 if f1 >= 0.70 else (2 if f1 >= 0.40 else (1 if f1 >= 0.15 else 0))
        if score == 0 and must_hits >= min_to_pass:
            score = 1
        return {
            "precision": 1.0 if (must_hits or fl_hits) else 0.0,
            "recall": recall,
            "f1": f1,
            "score": score,
            "matches": list(must_include[:must_hits]) + [f"{f}:{l}" for f, l in fl_hits],
            "misses": [m for m in must_include if m.lower() not in answer_lower],
        }

    # Fallback: 0.
    return {"precision": 0, "recall": 0, "f1": 0, "score": 0, "matches": [], "misses": []}


def score_session(session_answers, ground_truth=None):
    """
    Score a full session (10 query answers).

    session_answers: list of (query_id, answer_text) tuples.
    Returns: total_score, per_query_scores, mean_precision/recall/f1.
    """
    if ground_truth is None:
        ground_truth = load_ground_truth()

    per_query = []
    total = 0
    precisions, recalls, f1s = [], [], []

    for query_id, answer_text in session_answers:
        gt = ground_truth.get(query_id)
        if gt is None:
            per_query.append({"query_id": query_id, "score": 0, "note": "no ground truth"})
            continue
        result = score_query(query_id, gt, answer_text)
        result["query_id"] = query_id
        per_query.append(result)
        total += result["score"]
        precisions.append(result["precision"])
        recalls.append(result["recall"])
        f1s.append(result["f1"])

    n = max(len(precisions), 1)
    return {
        "total_score": total,
        "max_score": len(session_answers) * 3,
        "mean_precision": sum(precisions) / n,
        "mean_recall": sum(recalls) / n,
        "mean_f1": sum(f1s) / n,
        "per_query": per_query,
    }


if __name__ == "__main__":
    import sys

    if len(sys.argv) < 2:
        print("Usage: accuracy_scorer.py <jsonl_bench_file>")
        sys.exit(1)

    gt = load_ground_truth()
    with open(sys.argv[1]) as f:
        runs = [json.loads(l) for l in f if l.strip()]

    for run in runs:
        cond = run.get("cond", "?")
        seed = run.get("seed", "?")
        answers = [(q["query_id"], q.get("answer", "")) for q in run.get("queries", [])]
        if not answers:
            continue
        result = score_session(answers, gt)
        print(f"\n=== {cond} seed={seed} ===")
        print(f"  Accuracy total: {result['total_score']}/{result['max_score']}")
        print(f"  Mean P/R/F1: {result['mean_precision']:.3f} / {result['mean_recall']:.3f} / {result['mean_f1']:.3f}")
        print(f"  Keyword total (from run): {run.get('total_score', '?')}/30")
        for q in result["per_query"]:
            gt_q = gt.get(q["query_id"], {})
            print(f"    {q['query_id']}: accuracy={q['score']}/3 f1={q['f1']:.2f} matches={len(q.get('matches', []))} misses={len(q.get('misses', []))}")