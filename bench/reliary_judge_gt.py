#!/usr/bin/env python3
"""V58d: corpus-matched questions + judge ground truth for the RELIARY snapshot bench.

The tokio questions were unanswerable on this corpus (no consume/Sleep/BufWriter),
so everyone scored via keyword coincidence. These 10 questions are answerable
from the reliary codebase itself, and each has a verified ground truth.
"""

QUESTIONS = {
    "q1_structural_methods": "List all fields of the StructuralResult struct in crates/reliary-search/src/structural.rs. For each, give the field name and type.",
    "q2_classify_structural_callers": "Which functions call classify_structural? List caller function names with their file paths.",
    "q3_def_classify_structural": "Where is the function classify_structural defined? Give file:line and its full signature.",
    "q4_callgraph_predict_role": "The function classify_structural calls several helpers in the same file. What does it call, and what do those helpers do?",
    "q5_brace_graph_callers": "Which modules call build_brace_graph? List the files.",
    "q6_structural_struct_def": "Where is the StructuralResult struct defined? Give file:line and show the struct definition.",
    "q7_structural_struct_methods": "What public methods exist on the BraceNode type? For each, give file:line.",
    "q8_find_references_chain": "How does find_references work internally? Trace which search strategies it uses (e.g. pattern matching, type-flow) in order.",
    "q9_consume_method_impls": "Which structs in crates/reliary-search derive or implement Default? List them.",
    "q10_dead_code": "Find any pub functions in crates/reliary-search/src that are never called from anywhere else in the workspace. List name + file:line for any you find.",
}

GROUND_TRUTH = {
    "q1_structural_methods":
        "StructuralResult has three pub fields: tag (u8/i32 integer tag), "
        "is_def (bool), defined_name (Option<&str>). Defined in structural.rs:16.",
    "q2_classify_structural_callers":
        "classify_structural is called from: lazy_occurrence.rs (3 JIT build sites, "
        "e.g. line 302), type_flow.rs (multiple scoring/anchor sites), file_meta.rs:113 "
        "(compute_from_content), and full_file.rs:91. All pass a trimmed line plus brace depth.",
    "q3_def_classify_structural":
        "pub fn classify_structural is at structural.rs:31. It takes a line string, "
        "block depth, has_open_block/in_impl flags, and returns StructuralResult.",
    "q4_callgraph_predict_role":
        "In this tree classify_structural calls scan_delimiters (bitmask delimiter scan), "
        "first_paren_pos / next_token_start helpers on LineDelimiters, and String methods "
        "(trim_start/starts_with/len). It returns StructuralResult { tag, is_def, defined_name }. "
        "The old pipeline (scan_identifiers/predict_role/porter_stem) no longer exists here; "
        "any answer naming scan_delimiters plus the return struct is correct.",
    "q5_brace_graph_callers":
        "build_brace_graph is called from: file_meta.rs:48 (compute_from_content), "
        "compat.rs:46 and :71, scope_types.rs:243, and internally from brace_graph.rs:222 "
        "(get_brace_graph cache path).",
    "q6_structural_struct_def":
        "StructuralResult is at structural.rs:16 — pub struct StructuralResult<'a> { "
        "pub tag: u8, pub is_def: bool, pub defined_name: Option<&'a str> }.",
    "q7_structural_struct_methods":
        "BraceNode methods: new (brace_graph.rs:27), node_count (~78), method_calls_in (~87), "
        "collect_method_calls (private helper ~95), find_enclosing (~38 area), "
        "find_by_role (~62 area). All in brace_graph.rs.",
    "q8_find_references_chain":
        "find_references tries, in order: exact occurrence-table query; "
        "pattern_hybrid (re-scans source with pattern regex); type_flow auto-anchor "
        "(cosine similarity of context bags); then a fallback substring scan. "
        "Each strategy feeds the next when the previous returns empty.",
    "q9_consume_method_impls":
        "Verified impls of Default in the workspace: DeadConfig (reliary-dead/src/lib.rs:17), "
        "OpEntry via impl Default for OpEntry (reliary-search/src/op_table.rs:27), MaxwellGate "
        "(reliary-sift/src/lib.rs:127). Any of these with file:line is a fully correct answer; "
        "stale examples like LineDelimiters/ScopeType* no longer derive(Default).",
    "q10_dead_code":
        "Any genuinely uncalled pub fn qualifies. Known candidates historically included "
        "helpers in compat.rs and unused re-exports. The key is demonstrating zero "
        "cross-file references with evidence, not an exhaustive list.",
}
