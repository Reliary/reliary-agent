"""Ground truth for the 10 long-bench queries against the reliary corpus.
Built by inspecting $HOME/src/reliary8 on 2026-07-15."""

# q1: Methods on StructuralResult struct
q1_structural_methods = {
    "types": ["StructuralResult"],
    "methods": {
        "tag": "i32",
        "is_def": "bool",
        "defined_name": "Option<&str>",
    },
    "accept": ["tag", "is_def", "defined_name"],
}

# q2: Callers of classify_structural
q2_classify_structural_callers = {
    "callers": [
        ("crates/reliary-search/src/symbol.rs", "structural"),
        ("crates/reliary-search/src/brace_graph.rs", "classify"),
        ("crates/reliary-search/src/callgraph_v2.rs", "classify"),
    ],
    "min_count": 2,
}

# q3: Definition of classify_structural
q3_def_classify_structural = {
    "file": "crates/reliary-search/src/structural.rs",
    "line": 31,
    "signature": "pub fn classify_structural",
}

# q4: Call chain from classify_structural
q4_callgraph_predict_role = {
    "callees": ["scan_identifiers", "scan_delimiters", "predict_role", "porter_stem"],
    "min_count": 2,
}

# q5: Callers of build_brace_graph
q5_brace_graph_callers = {
    "callers": [
        ("crates/reliary-search/src/compat.rs", "build"),
        ("crates/reliary-search/src/file_meta.rs", "build"),
        ("crates/reliary-search/src/scope_types.rs", "build"),
        ("crates/reliary-search/src/brace_graph.rs", "build"),
    ],
    "min_count": 2,
}

# q6: Definition of StructuralResult struct
q6_structural_struct_def = {
    "file": "crates/reliary-search/src/structural.rs",
    "line": 16,
    "name": "StructuralResult",
}

# q7: Public methods on BraceNode
q7_structural_struct_methods = {
    "methods": ["new", "find_enclosing", "find_by_role", "node_count", "method_calls_in"],
    "min_count": 3,
}

# q8: Inner methods called by reliary_find_references
q8_find_references_chain = {
    "callees": ["find_references_pattern_hybrid", "find_references_auto", "find_references_type_flow"],
    "min_count": 2,
}

# q9: Structs implementing Default
q9_consume_method_impls = {
    "types": ["StructuralResult", "BraceNode", "OccHit"],
    "min_count": 1,
}

# q10: Dead code in reliary-search
q10_dead_code = {
    "functions": [],
    "accept_keywords": ["dead", "unused"],
    "min_count": 0,
}

GROUND_TRUTH = {
    "q1_structural_methods": q1_structural_methods,
    "q2_classify_structural_callers": q2_classify_structural_callers,
    "q3_def_classify_structural": q3_def_classify_structural,
    "q4_callgraph_predict_role": q4_callgraph_predict_role,
    "q5_brace_graph_callers": q5_brace_graph_callers,
    "q6_structural_struct_def": q6_structural_struct_def,
    "q7_structural_struct_methods": q7_structural_struct_methods,
    "q8_find_references_chain": q8_find_references_chain,
    "q9_consume_method_impls": q9_consume_method_impls,
    "q10_dead_code": q10_dead_code,
}