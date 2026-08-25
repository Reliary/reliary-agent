use reliary_pack::{should_slice_for_query, SliceDecision, adaptive_slice, slice_pack_for_query};

#[test]
fn slice_for_bug_questions() {
    assert_eq!(should_slice_for_query("Is there a bug in skeleton()?"), SliceDecision::Slice);
    assert_eq!(should_slice_for_query("What is wrong with the hex threshold?"), SliceDecision::Slice);
    assert_eq!(should_slice_for_query("Why is the zero sentinel missing?"), SliceDecision::Slice);
}

#[test]
fn slice_for_crossref_questions() {
    assert_eq!(should_slice_for_query("Who calls skeleton()?"), SliceDecision::Slice);
    assert_eq!(should_slice_for_query("Find references to find_clusters"), SliceDecision::Slice);
    assert_eq!(should_slice_for_query("Which functions are called by classify_line?"), SliceDecision::Slice);
}

#[test]
fn slice_for_detail_questions() {
    assert_eq!(should_slice_for_query("What is the default value of min_run?"), SliceDecision::Slice);
    assert_eq!(should_slice_for_query("What is the entropy threshold?"), SliceDecision::Slice);
    assert_eq!(should_slice_for_query("What parameter does the function take?"), SliceDecision::Slice);
}

#[test]
fn slice_for_specific_symbol_names() {
    // These are project-specific names the model won't know from training
    assert_eq!(should_slice_for_query("How does find_clusters work?"), SliceDecision::Slice);
    assert_eq!(should_slice_for_query("Explain skeleton_hash"), SliceDecision::Slice);
    assert_eq!(should_slice_for_query("What does detect_strategy do?"), SliceDecision::Slice);
    assert_eq!(should_slice_for_query("Explain callgraph"), SliceDecision::Slice);
    // Generic function names (skeleton, classify_line) alone are NOT indicators
    // — those are common enough the model knows from training
}

#[test]
fn skip_for_what_questions() {
    // "skeleton" and "classify_line" are common enough that the model knows them
    // from training — the slice is unnecessary for general "what does X do" questions
    assert_eq!(should_slice_for_query("What does the skeleton() function do?"), SliceDecision::Skip);
    assert_eq!(should_slice_for_query("What does classify_line return?"), SliceDecision::Skip);
    assert_eq!(should_slice_for_query("What is compress_content?"), SliceDecision::Skip);
}

#[test]
fn skip_for_discriminate_questions() {
    // "classify" and "detect" are common function names — discriminate questions
    // about them don't need the slice (model already knows the structure)
    assert_eq!(should_slice_for_query("Does classify check comments before errors?"), SliceDecision::Skip);
    assert_eq!(should_slice_for_query("Why does detect only check the first line?"), SliceDecision::Skip);
}

#[test]
fn skip_for_arch_impact_review() {
    assert_eq!(should_slice_for_query("How do classify.rs and lib.rs relate to each other?"), SliceDecision::Skip);
    assert_eq!(should_slice_for_query("If I rename skeleton() to normalize(), what breaks?"), SliceDecision::Skip);
    assert_eq!(should_slice_for_query("Review this proposed change. Is it safe?"), SliceDecision::Skip);
}

#[test]
fn case_insensitive() {
    assert_eq!(should_slice_for_query("BUG in skeleton"), SliceDecision::Slice);
    assert_eq!(should_slice_for_query("What is the THRESHOLD?"), SliceDecision::Slice);
    assert_eq!(should_slice_for_query("Find CALLERS of skeleton"), SliceDecision::Slice);
}

#[test]
fn adaptive_slice_returns_empty_for_skip() {
    let pack = "## skeleton\nL2: pub fn skeleton(line: &str) -> String\nL3: UUID dash positions: 8, 13, 18, 23\n";
    let (sliced, decision) = adaptive_slice(pack, "What does skeleton() do?", 5);
    assert_eq!(decision, SliceDecision::Skip);
    assert_eq!(sliced, "");
}

#[test]
fn adaptive_slice_returns_content_for_slice() {
    let pack = "## skeleton\nL2: pub fn skeleton(line: &str) -> String\nL3: UUID dash positions: 8, 13, 18, 23\n\n## classify_line\nL2: pub fn classify_line(line: &str) -> LineType\nL3: Error check runs BEFORE Comment check\n";
    let (sliced, decision) = adaptive_slice(pack, "Is there a bug in skeleton?", 5);
    assert_eq!(decision, SliceDecision::Slice);
    assert!(sliced.contains("skeleton"));
}

#[test]
fn adaptive_slice_matches_explicit_slicer() {
    let pack = "## skeleton\nL2: pub fn skeleton(line: &str) -> String\nL3: UUID dash positions: 8, 13, 18, 23\n\n## classify_line\nL2: pub fn classify_line(line: &str) -> LineType\nL3: Error check runs BEFORE Comment check\n";
    let query = "Find callers of skeleton";
    // Both should slice — the difference is the query string
    let adaptive = adaptive_slice(pack, query, 5);
    let explicit = slice_pack_for_query(pack, "Find callers of skeleton", 5);
    assert_eq!(adaptive.0, explicit);
    // Both should be non-empty since the query triggers Slice
    assert!(!adaptive.0.is_empty());
}
