//! V14: Determinism tests for sift pipeline.
//!
//! Provider KV cache requires identical input to produce identical compressed output.
//! These tests verify byte-identical output across repeated calls.
//! Failures indicate non-determinism that would bust provider caches.

use reliary_sift::classify;
use reliary_sift::filter::format_output;
use reliary_sift::classify_line;

fn compress_via_adaptive(content: &str) -> String {
    let classified = classify::classify(content);
    let raw_lines: Vec<(String, _)> = classified.iter()
        .map(|l| (l.text.clone(), l.clone()))
        .collect();
    let strategy = classify::detect_strategy(&raw_lines);
    format_output(&classified, strategy, true)
}

fn check_deterministic(label: &str, input: &str, runs: usize) {
    let first = compress_via_adaptive(input);
    for i in 1..runs {
        let nth = compress_via_adaptive(input);
        assert_eq!(
            first, nth,
            "[{}] Non-deterministic output on run {} (input len {})",
            label, i, input.len()
        );
    }
}

#[test]
fn determinism_cargo_repeat() {
    let input: String = (0..30)
        .map(|i| format!("   Compiling serde v1.0.{}", i))
        .collect::<Vec<_>>()
        .join("\n");
    check_deterministic("cargo_repeat", &input, 10);
}

#[test]
fn determinism_cargo_mixed() {
    let crates = ["serde", "tokio", "regex", "anyhow", "thiserror"];
    let input: String = (0..24)
        .map(|i| format!("   Compiling {} 1.{}.0", crates[i % 5], i))
        .collect::<Vec<_>>()
        .join("\n");
    check_deterministic("cargo_mixed", &input, 10);
}

#[test]
fn determinism_pytest_passed() {
    let input = (0..15)
        .map(|i| format!("test test_{} ... ok", i))
        .collect::<Vec<_>>()
        .join("\n");
    check_deterministic("pytest_passed", &input, 10);
}

#[test]
fn determinism_git_diff() {
    let input = r#"diff --git a/src/lib.rs b/src/lib.rs
index 1234567..abcdef0 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,5 +1,7 @@
 fn main() {
     println!("hello");
+    let x = 42;
+    println!("{}", x);
 }
 fn other() {
     return;
"#;
    check_deterministic("git_diff", input, 10);
}

#[test]
fn determinism_compiler_error() {
    let input = r#"error[E0432]: unresolved import `foo`
  --> src/main.rs:1:5
   |
1  | use foo::bar;
   |     ^^^ help: a similar name exists in the crate

error: aborting due to 1 previous error
"#;
    check_deterministic("compiler_error", input, 10);
}

#[test]
fn determinism_grep_results() {
    let input = "src/lib.rs:42:fn main() {\nsrc/lib.rs:43:    println!(\"hi\");\nsrc/main.rs:10:fn other() {\nsrc/main.rs:11:    return;\n";
    check_deterministic("grep_results", input, 10);
}

#[test]
fn determinism_clusters_global_order() {
    // The BTreeMap fix in V14 must produce stable cluster ordering.
    let input: String = (0..30)
        .map(|i| format!("   Compiling crate_{} v1.0.{}", i % 5, i))
        .collect::<Vec<_>>()
        .join("\n");
    let first = classify::find_clusters_global_with_default(&input);
    for _ in 1..10 {
        let nth = classify::find_clusters_global_with_default(&input);
        assert_eq!(
            first.len(),
            nth.len(),
            "Cluster count non-deterministic"
        );
        for (a, b) in first.iter().zip(nth.iter()) {
            assert_eq!(
                a.skeleton_key, b.skeleton_key,
                "Cluster key non-deterministic"
            );
            assert_eq!(
                a.lines, b.lines,
                "Cluster line contents non-deterministic"
            );
        }
    }
}

#[test]
fn determinism_classify_lines() {
    // Per-line classification must be deterministic.
    let input = "fn main() {\n    println!(\"hi\");\n}\n";
    let first = classify::classify(input);
    for _ in 1..10 {
        let nth = classify::classify(input);
        assert_eq!(first.len(), nth.len());
        for (a, b) in first.iter().zip(nth.iter()) {
            assert_eq!(a.text, b.text);
            assert_eq!(a.skeleton_key, b.skeleton_key);
            assert_eq!(a.is_error, b.is_error);
            assert_eq!(a.is_progress, b.is_progress);
        }
    }
}

#[test]
fn determinism_classify_line_single() {
    // Per-line classification must be deterministic and consistent.
    let input = "error: undefined variable";
    let first = classify_line(input);
    for _ in 0..10 {
        let nth = classify_line(input);
        assert_eq!(first, nth);
    }
}