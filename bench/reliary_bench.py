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
        "question": "List all fields of the StructuralResult struct in crates/reliary-search/src/structural.rs. For each, give the field name and type.",
        "rubric": {"accept": ["tag", "is_def", "defined_name"], "min_count": 2, "max_count": 6},
    },
    {
        "id": "q2_classify_structural_callers",
        "question": "Which functions call classify_structural? List caller function names with their file paths.",
        "rubric": {"accept_keywords": ["classify_structural", "call"], "min_calls": 2},
    },
    {
        "id": "q3_def_classify_structural",
        "question": "Where is the function classify_structural defined? Give file:line and its full signature.",
        "rubric": {"accept_keywords": ["classify_structural", "fn", "structural"], "min_steps": 1},
    },
    {
        "id": "q4_callgraph_predict_role",
        "question": "The function classify_structural calls several helpers in the same file. What does it call, and what do those helpers do?",
        "rubric": {"accept_keywords": ["predict", "tag", "classify", "structural"], "min_steps": 2},
    },
    {
        "id": "q5_brace_graph_callers",
        "question": "Which modules call build_brace_graph? List the files.",
        "rubric": {"accept_keywords": ["build_brace_graph", "call"], "min_calls": 2},
    },
    {
        "id": "q6_structural_struct_def",
        "question": "Where is the StructuralResult struct defined? Give file:line and show the struct definition.",
        "rubric": {"accept_keywords": ["StructuralResult", "struct", "pub"], "min_steps": 1},
    },
    {
        "id": "q7_structural_struct_methods",
        "question": "What public methods exist on the BraceNode type? For each, give file:line.",
        "rubric": {"accept_keywords": ["fn", "pub", "BraceNode"], "min_methods": 2},
    },
    {
        "id": "q8_find_references_chain",
        "question": "How does find_references resolve a symbol like classify_structural? Name the files it returns results from and explain why the primary definition (structural.rs:31) ranks first.",
        "rubric": {"accept_keywords": ["find_references", "structural", "classify"], "min_steps": 1},
    },
    {
        "id": "q9_consume_method_impls",
        "question": "Which structs in crates/reliary-search derive or implement Default? List them.",
        "rubric": {"accept_keywords": ["Default", "impl"], "min_steps": 1},
    },
    {
        "id": "q10_dead_code",
        "question": "Find any pub functions in crates/reliary-search/src that are never called from anywhere else in the workspace. List name + file:line for any you find.",
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