"""Reliary self-benchmark — Option E.

Asks 10 code intelligence questions about the reliary codebase itself
(private to the model — no training priors to fight our tool).
"""
import json
import sys
import os

RELIARY_CORPUS = "$HOME/src/reliary8"

SESSION_QUERIES = [
    {
        "id": "q1_structural_methods",
        "question": "List all methods on the StructuralResult struct. For each, give file:line.",
        "rubric": {"accept": ["tag", "is_def", "defined_name"], "min_count": 2, "max_count": 6},
    },
    {
        "id": "q2_classify_structural_callers",
        "question": "Where is classify_structural called from? List file:line for each caller.",
        "rubric": {"accept_keywords": ["classify_structural", "call"], "min_calls": 2},
    },
    {
        "id": "q3_def_classify_structural",
        "question": "Where is classify_structural defined? Give file:line and show the function signature.",
        "rubric": {"accept_keywords": ["classify_structural", "fn", "structural"], "min_steps": 1},
    },
    {
        "id": "q4_callgraph_predict_role",
        "question": "Trace the call chain from classify_structural — what functions does it call directly? List each callee.",
        "rubric": {"accept_keywords": ["predict", "tag", "classify", "structural"], "min_steps": 2},
    },
    {
        "id": "q5_brace_graph_callers",
        "question": "Who calls build_brace_graph? List the top 5 callers with file:line.",
        "rubric": {"accept_keywords": ["build_brace_graph", "call"], "min_calls": 2},
    },
    {
        "id": "q6_structural_struct_def",
        "question": "Find the definition of the StructuralResult struct. Show file:line and the struct definition.",
        "rubric": {"accept_keywords": ["StructuralResult", "struct", "pub"], "min_steps": 1},
    },
    {
        "id": "q7_structural_struct_methods",
        "question": "List all public methods on the BraceNode struct. For each, give file:line.",
        "rubric": {"accept_keywords": ["fn", "pub", "BraceNode"], "min_methods": 2},
    },
    {
        "id": "q8_find_references_chain",
        "question": "When reliary_find_references is called, which inner methods does it call? Trace the delegation chain.",
        "rubric": {"accept_keywords": ["find_references", "pattern", "type_flow"], "min_steps": 2},
    },
    {
        "id": "q9_consume_method_impls",
        "question": "Which structs implement the Default trait? Cross-reference with any state-related types.",
        "rubric": {"accept_keywords": ["Default", "impl"], "min_steps": 1},
    },
    {
        "id": "q10_dead_code",
        "question": "Find any functions in the reliary-search crate that are defined but never called from anywhere in the codebase. List file:line for any you find.",
        "rubric": {"accept_keywords": ["dead", "unused"], "min_steps": 1},
    },
]


def get_queries():
    return SESSION_QUERIES


def get_corpus():
    return RELIARY_CORPUS


if __name__ == "__main__":
    print(f"Corpus: {RELIARY_CORPUS}")
    print(f"Queries: {len(SESSION_QUERIES)}")
    for q in SESSION_QUERIES:
        print(f"  - {q['id']}: {q['question'][:60]}...")