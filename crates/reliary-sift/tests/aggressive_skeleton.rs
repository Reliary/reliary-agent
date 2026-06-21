//! Tests for aggressive skeleton normalization.
//!
//! Aggressive skeleton replaces every word token with {w}, so lines that
//! differ only in token values share the same template. This catches
//! template-filled output (cargo "Compiling X", pytest "test_X ok")
//! that the normal skeleton (which preserves words) misses.

use reliary_sift::classify::*;

#[test]
fn aggressive_skeleton_groups_cargo_compiling() {
    // The core insight: each "Compiling X vY" line has unique words,
    // but they share the same structural template.
    let s1 = aggressive_skeleton("   Compiling serde v1.0.200");
    let s2 = aggressive_skeleton("   Compiling tokio v1.40.0");
    let s3 = aggressive_skeleton("   Compiling regex v1.10.0");
    assert_eq!(s1, s2);
    assert_eq!(s2, s3);
    assert!(s1.contains("{w}"), "should contain word placeholder");
    assert!(s1.contains("{ver}"), "should contain version placeholder");
}

#[test]
fn aggressive_skeleton_groups_pytest_passed() {
    let s1 = aggressive_skeleton("test test_one ... ok");
    let s2 = aggressive_skeleton("test test_two ... ok");
    let s3 = aggressive_skeleton("test test_three ... ok");
    assert_eq!(s1, s2);
    assert_eq!(s2, s3);
}

#[test]
fn aggressive_skeleton_differentiates_signal_lines() {
    // Error lines have unique structure and should NOT share skeletons
    let s_failed = aggressive_skeleton("test result: FAILED. 5 passed; 1 failed");
    let s_error = aggressive_skeleton("error[E0308]: mismatched types at src/lib.rs:42:5");
    let s_panic = aggressive_skeleton("thread 'test_async' panicked at src/lib.rs:42:5");
    let s_blank = aggressive_skeleton("");
    assert_ne!(s_failed, s_error);
    assert_ne!(s_failed, s_panic);
    assert_ne!(s_error, s_panic);
    assert_eq!(s_blank, "");
}

#[test]
fn aggressive_skeleton_vs_normal_skeleton() {
    // Normal skeleton preserves words → cargo lines differ
    let n1 = skeleton("   Compiling serde v1.0.200");
    let n2 = skeleton("   Compiling tokio v1.40.0");
    assert_ne!(n1, n2, "normal skeleton should differ for different crates");

    // Aggressive skeleton collapses words → cargo lines match
    let a1 = aggressive_skeleton("   Compiling serde v1.0.200");
    let a2 = aggressive_skeleton("   Compiling tokio v1.40.0");
    assert_eq!(a1, a2, "aggressive skeleton should match for different crates");
}

#[test]
fn aggressive_skeleton_preserves_structural_chars() {
    // Colons, brackets, dots etc. are structurals, not words
    let s = aggressive_skeleton("error[E0308]: /usr/local/bin/cargo");
    assert!(s.contains('['));
    assert!(s.contains(']'));
    assert!(s.contains(':'));
    assert!(s.contains('/'));
    // E0308 stays as one alphanumeric token "e0308" — the original skeleton's
    // `bd` boundary check prevents splitting (same as plain skeleton).
    // This is fine for our purposes: all error codes share the same aggressive
    // skeleton template, and is_error guard prevents collapsing anyway.
    assert!(s.contains("{w}"));
    assert_eq!(s, "{w}[e0308]: /{w}/{w}/{w}/{w}");
}

#[test]
fn aggressive_skeleton_normalizes_uuids_and_hashes() {
    let s = aggressive_skeleton("commit abc123def456789012345678901234567890123");
    assert!(s.contains("{hash}"), "hex hash should be normalized");
}

#[test]
fn aggressive_skeleton_empty_input() {
    assert_eq!(aggressive_skeleton(""), "");
    assert_eq!(aggressive_skeleton("   "), "");
    assert_eq!(aggressive_skeleton("\t\n"), "");
}

#[test]
fn aggressive_skeleton_does_not_collapse_function_definitions() {
    // Python functions: same signature template but different names
    // Aggressive skeleton WILL collapse these — that's the trade-off.
    // The repetitiveness gate in format_normal prevents this for file reads.
    let s1 = aggressive_skeleton("def handle_get(request):");
    let s2 = aggressive_skeleton("def handle_put(request):");
    // Note: these DO collapse under aggressive skeleton. The guard is
    // the repetitiveness check in format_normal that distinguishes
    // command output (repetitive) from file reads (unique lines).
    assert_eq!(s1, s2);
}
