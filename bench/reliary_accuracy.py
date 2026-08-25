#!/usr/bin/env python3
"""
Accuracy scorer for the reliary self-benchmark (Option E).

Uses reliary_ground_truth.py instead of tokio ground truth.
Same scoring logic: precision, recall, F1, 0-3 per query.
"""

import json
import sys
import re
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from reliary_ground_truth import GROUND_TRUTH

SCORE_THRESHOLDS = [(0.70, 3), (0.40, 2), (0.15, 1), (0.0, 0)]


def extract_symbols_from_answer(answer_text):
    answer = answer_text or ""
    symbols = set()
    file_lines = set()
    # Qualified: CamelCase::snake_case
    for m in re.finditer(r"\b([A-Z][A-Za-z0-9]*(?:::[a-z_][A-Za-z0-9_]*)+)\b", answer):
        symbols.add(m.group(1))
    # Unqualified method name
    for m in re.finditer(r"\b([a-z_][a-z_0-9]{2,})\b", answer):
        name = m.group(1)
        if name not in ("the", "and", "for", "are", "not", "has", "was", "but",
                        "can", "all", "any", "new", "get", "set", "add", "run",
                        "use", "try", "see", "key", "end", "may", "let",
                        "pub", "fn", "ref", "mut", "mod", "box", "impl",
                        "trait", "enum", "self", "match", "where", "like",
                        "result", "error", "none", "some", "true",
                        "false", "file", "line", "code", "type", "call",
                        "also", "this", "that", "from", "into", "with",
                        "dead", "unused", "callee", "caller", "def",
                        "string", "list", "tool", "name", "find",
                        "definition", "implementations", "implementor",
                        "method", "struct", "module", "indexed", "answer",
                        "context", "based", "using", "available",
                        "api", "bar", "foo", "baz", "tokio", "reliary",
                        "consistent", "delegation", "chain"):
            symbols.add(name)
    # CamelCase (struct names)
    for m in re.finditer(r"\b([A-Z][A-Za-z0-9]{2,})\b", answer):
        symbols.add(m.group(1))
    # file:line references — handle both .rs and just .rs:N
    for m in re.finditer(r"([\w\-/]+\.rs)[^\d]{0,30}(\d+)", answer):
        path = m.group(1)
        line = int(m.group(2))
        norm_path = path
        for prefix in ("crates/reliary-search/src/", "src/", "crates/"):
            if norm_path.startswith(prefix):
                norm_path = norm_path[len(prefix):]
                break
        file_lines.add((norm_path, line))
        if '/' in norm_path:
            base = norm_path.rsplit('/', 1)[-1]
            file_lines.add((base, line))
    return symbols, file_lines


def score_q1(gt, answer):
    """Methods on StructuralResult — accept any of {tag, is_def, defined_name}."""
    methods = gt["methods"]
    symbols, _ = extract_symbols_from_answer(answer)
    found = sum(1 for m in methods if m in symbols)
    recall = found / len(methods) if methods else 0
    precision = recall
    f1 = recall
    score = 3 if f1 >= 0.70 else (2 if f1 >= 0.40 else (1 if f1 >= 0.15 else 0))
    return {"precision": precision, "recall": recall, "f1": f1, "score": score,
            "matches": list(methods.keys())[:found], "misses": []}


def score_q3(gt, answer):
    """Single definition: classify_structural at structural.rs:31."""
    correct_file = gt["file"]
    correct_line = gt["line"]
    _, mentioned_fl = extract_symbols_from_answer(answer)
    hits = any(correct_file in fp or fp.endswith(correct_file) for (fp, ln) in mentioned_fl)
    has_signature = "fn classify_structural" in answer or "pub fn" in answer
    recall = 1.0 if hits else 0.0
    if hits and has_signature:
        score = 3
    elif hits:
        score = 2
    elif has_signature:
        score = 1
    else:
        score = 0
    return {"precision": 1.0 if hits else 0.0, "recall": recall, "f1": recall, "score": score,
            "matches": [f"{correct_file}:{correct_line}"] if hits else [],
            "misses": [f"{correct_file}:{correct_line}"] if not hits else []}


def score_q5(gt, answer):
    """Callers of build_brace_graph."""
    callers = gt["callers"]
    _, mentioned_fl = extract_symbols_from_answer(answer)
    correct_files = {c[0] for c in callers}
    mentioned_files = {fp for (fp, ln) in mentioned_fl}
    file_hits = correct_files & mentioned_files
    recall = len(file_hits) / len(correct_files) if correct_files else 0
    precision = recall
    f1 = recall
    score = 3 if f1 >= 0.70 else (2 if f1 >= 0.40 else (1 if f1 >= 0.15 else 0))
    return {"precision": precision, "recall": recall, "f1": f1, "score": score,
            "matches": list(file_hits), "misses": []}


def score_q6(gt, answer):
    """StructuralResult struct definition at structural.rs:16."""
    correct_file = gt["file"]
    correct_line = gt["line"]
    _, mentioned_fl = extract_symbols_from_answer(answer)
    hits = any(correct_file in fp or fp.endswith(correct_file) for (fp, ln) in mentioned_fl)
    has_struct = "struct StructuralResult" in answer or "pub struct StructuralResult" in answer
    if hits and has_struct:
        score = 3
    elif hits:
        score = 2
    elif has_struct:
        score = 1
    else:
        score = 0
    return {"precision": 1.0 if hits else 0.0, "recall": 1.0 if hits else 0.0,
            "f1": 1.0 if hits else 0.0, "score": score,
            "matches": [f"{correct_file}:{correct_line}"] if hits else [],
            "misses": []}


def score_callers_or_callees(gt, answer):
    """Generic: count mentioned callees/callers."""
    targets = gt.get("callees", gt.get("callers", []))
    if targets and isinstance(targets[0], tuple):
        # callers format: [(file, name), ...]
        correct_names = {t[1] for t in targets}
        _, mentioned_fl = extract_symbols_from_answer(answer)
        correct_files = {t[0] for t in targets}
        mentioned_files = {fp for (fp, _) in mentioned_fl}
        name_hits = 0  # caller names in answer
        file_hits = correct_files & mentioned_files
        recall = len(file_hits) / len(correct_files) if correct_files else 0
    else:
        correct_names = set(targets)
        mentioned_syms, _ = extract_symbols_from_answer(answer)
        # Match against leaf name (e.g., "predict_role" from "predict_role_with_stem")
        name_hits = sum(1 for cn in correct_names if cn in mentioned_syms or
                        any(cn in ms for ms in mentioned_syms))
        recall = name_hits / len(correct_names) if correct_names else 0
    f1 = recall
    score = 3 if f1 >= 0.70 else (2 if f1 >= 0.40 else (1 if f1 >= 0.15 else 0))
    return {"precision": recall, "recall": recall, "f1": f1, "score": score,
            "matches": list(correct_names)[:name_hits], "misses": []}


def score_accept_keywords(gt, answer):
    """Fallback: check accept_keywords present in answer."""
    accept = gt.get("accept", gt.get("accept_keywords", []))
    answer_lower = (answer or "").lower()
    hits = sum(1 for kw in accept if kw.lower() in answer_lower)
    recall = hits / len(accept) if accept else 0
    f1 = recall
    score = 3 if f1 >= 0.70 else (2 if f1 >= 0.40 else (1 if f1 >= 0.15 else 0))
    return {"precision": recall, "recall": recall, "f1": f1, "score": score,
            "matches": accept[:hits], "misses": []}


def score_query(query_id, gt, answer):
    answer = answer or ""
    if not answer or len(answer) < 5:
        return {"precision": 0, "recall": 0, "f1": 0, "score": 0,
                "matches": [], "misses": []}
    if query_id == "q1_structural_methods":
        return score_q1(gt, answer)
    if query_id == "q3_def_classify_structural":
        return score_q3(gt, answer)
    if query_id == "q5_brace_graph_callers":
        return score_q5(gt, answer)
    if query_id == "q6_structural_struct_def":
        return score_q6(gt, answer)
    if "callees" in gt or "callers" in gt:
        return score_callers_or_callees(gt, answer)
    return score_accept_keywords(gt, answer)


def score_session(answers, ground_truth=None):
    if ground_truth is None:
        ground_truth = GROUND_TRUTH
    per_query = []
    total = 0
    for query_id, answer_text in answers:
        gt = ground_truth.get(query_id)
        if gt is None:
            per_query.append({"query_id": query_id, "score": 0, "note": "no ground truth"})
            continue
        result = score_query(query_id, gt, answer_text)
        result["query_id"] = query_id
        per_query.append(result)
        total += result["score"]
    n = max(len(per_query), 1)
    return {
        "total_score": total,
        "max_score": len(answers) * 3,
        "mean_f1": sum(p.get("f1", 0) for p in per_query) / n,
        "per_query": per_query,
    }


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: reliary_accuracy.py <jsonl_bench_file>")
        sys.exit(1)
    with open(sys.argv[1]) as f:
        runs = [json.loads(l) for l in f if l.strip()]
    for run in runs:
        cond = run.get("cond", "?")
        seed = run.get("seed", "?")
        answers = [(q["query_id"], q.get("answer", "")) for q in run.get("queries", [])]
        if not answers:
            continue
        result = score_session(answers)
        print(f"\n=== {cond} seed={seed} ===")
        print(f"  Accuracy total: {result['total_score']}/{result['max_score']}")
        print(f"  Mean F1: {result['mean_f1']:.3f}")
        print(f"  Keyword total (from run): {run.get('total_score', '?')}/30")
        for q in result["per_query"]:
            print(f"    {q['query_id']}: accuracy={q['score']}/3 f1={q.get('f1', 0):.2f} matches={q.get('matches', [])}")