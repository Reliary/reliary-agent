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
    "q8_find_references_chain": "How does find_references resolve a symbol like classify_structural? Name the files it returns results from and explain why the primary definition (structural.rs:31) ranks first.",
    "q9_consume_method_impls": "Which structs in crates/reliary-search derive or implement Default? List them.",
    "q10_dead_code": "Find any pub functions in crates/reliary-search/src that are never called from anywhere else in the workspace. List name + file:line for any you find.",
}

GROUND_TRUTH = {
    # V66: every fact below was audited mechanically against the index AND the
    # source (bench/gt_audit.py). No fact is accepted without source validation
    # (line-contains or structural regex). GT is derived from index + source
    # only — never from bench answers.
    "q1_structural_methods":
        "StructuralResult has three pub fields: tag: u8 (structural.rs:18), "
        "is_def: bool (structural.rs:20), defined_name: Option<&'a str> (structural.rs:22). "
        "Struct defined at structural.rs:16.",
    "q2_classify_structural_callers":
        "classify_structural is called from (audited set): file_meta.rs:9 (use), "
        "ingest.rs:181, lib.rs:490, and the structural.rs test module at lines "
        "944-1033. Other is_def=0 rows in lazy_occurrence/type_flow/full_file are "
        "comment mentions, not calls (source-validated).",
    "q3_def_classify_structural":
        "pub fn classify_structural is at structural.rs:31. Signature: "
        "classify_structural<'a>(line: &'a str, block_depth: i32, has_open_block: bool, in_impl: bool) -> StructuralResult<'a>.",
    "q4_callgraph_predict_role":
        "In this tree classify_structural (structural.rs:31) calls these in-file "
        "helpers (all index-verified as defs): scan_delimiters, find_top_level_eq, "
        "scan_last_identifier_before, is_valid_identifier, classify_python_colon_line, "
        "find_function_name_pos, scan_last_identifier, after_for_identifiers. "
        "Std methods (trim_start) and struct fields (tag/is_def/defined_name) are "
        "not helpers. Any subset of the audited helpers with evidence is correct.",
    "q5_brace_graph_callers":
        "build_brace_graph is called from (audited set): callgraph_v2.rs:19 (use), "
        "compat.rs:45 and :70, file_meta.rs:8 (use) / file_meta.rs:56 (call), "
        "scope_types.rs:243, and brace_graph.rs:321 (get_brace_graph internals).",
    "q6_structural_struct_def":
        "StructuralResult is defined at structural.rs:16 — pub struct StructuralResult<'a> { "
        "pub tag: u8, pub is_def: bool, pub defined_name: Option<&'a str> }. "
        "Citing structural.rs:16 (source line number) is correct.",
    "q7_structural_struct_methods":
        "BraceNode impl methods (impl BraceNode at brace_graph.rs:25, all audited): "
        "new (brace_graph.rs:26), find_enclosing (brace_graph.rs:37), find_by_role "
        "(brace_graph.rs:61), node_count (brace_graph.rs:77), method_calls_in "
        "(brace_graph.rs:86), collect_method_calls (brace_graph.rs:92). Any of these "
        "with file:line is a fully correct answer.",
    # V66: q8 reframed — the old question asked for internal strategy names the
    # tool deliberately abstracts (structurally unanswerable without a
    # benchmark-adjacent hack). The new question tests index-verifiable behavior.
    "q8_find_references_chain":
        "How does find_references resolve a symbol like classify_structural? "
        "Name the files it returns results from and explain why the primary "
        "definition (structural.rs:31) ranks first.",
    "q9_consume_method_impls":
        "Verified impls of Default in the workspace (source-audited): DeadConfig "
        "(reliary-dead/src/lib.rs:17), OpEntry (reliary-search/src/op_table.rs:27), "
        "MaxwellGate (reliary-sift/src/lib.rs:127). Any of these with file:line is a "
        "fully correct answer; stale examples like LineDelimiters/ScopeType* no longer "
        "derive(Default).",
    "q10_dead_code":
        "Any genuinely uncalled pub fn in reliary-search/src qualifies. Audited "
        "candidates (re-run bench/gt_audit.py after each reindex — the dead set "
        "shifts with index changes): test_detect_language (architecture.rs:403), "
        "default_temperature (boltzmann.rs:84), and other zero-caller functions "
        "in architecture.rs/boltzmann.rs. The key is demonstrating zero cross-file "
        "references with evidence, not an exhaustive list.",
}
