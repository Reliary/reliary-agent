//! Tests for information-preserving zone truncation.
//!
//! The mechanism scores lines by:
//! 1. Custom scorer function (typically FTS5 DF-based)
//! 2. Error bonus (FAILED, error[, panic, Traceback, etc.)
//! 3. Position bonus (first/last 3 lines)
//!
//! Then picks top-N by score and reassembles preserving original order with
//! markers for dropped runs.

use reliary_sift::*;

fn make_long_content() -> String {
    let mut lines = Vec::new();
    // 50 boilerplate lines
    for i in 0..50 {
        lines.push(format!("   Compiling crate{} v1.0.{}", i, i));
    }
    // An error in the middle
    lines.push("error[E0308]: mismatched types".to_string());
    lines.push("  --> src/foo.rs:42:5".to_string());
    lines.push("   |".to_string());
    lines.push("42 |     let x: usize = \"string\";".to_string());
    lines.push("   |                  ^^^^^^^^^^ expected usize, found &str".to_string());
    // More boilerplate
    for i in 50..100 {
        lines.push(format!("   Compiling crate{} v1.0.{}", i, i));
    }
    lines.join("\n")
}

#[test]
fn zone_truncate_info_preserves_error_in_middle() {
    let content = make_long_content();
    let total_lines = content.lines().count();
    assert!(total_lines > 100);

    // Scorer: low score for boilerplate "Compiling" lines, neutral for others
    let truncated = zone_truncate_info(&content, 10, Some(|line: &str| {
        if line.contains("Compiling") { 0.0 } else { 5.0 }
    }));

    // Error line MUST be preserved (is_error_line bonus = +10)
    assert!(
        truncated.contains("error[E0308]"),
        "error line should be preserved, got: {}",
        &truncated[..truncated.len().min(300)]
    );
    assert!(
        truncated.contains("expected usize"),
        "error detail should be preserved"
    );
    // File:line reference MUST be preserved
    assert!(
        truncated.contains("src/foo.rs:42:5"),
        "file:line ref should be preserved"
    );
}

#[test]
fn zone_truncate_info_uses_marker_for_dropped_lines() {
    let content = make_long_content();
    let truncated = zone_truncate_info(&content, 10, Some(|line: &str| {
        if line.contains("Compiling") { 0.0 } else { 5.0 }
    }));

    // Should have at least one marker for dropped lines
    assert!(
        truncated.contains("[…") && truncated.contains("lines…]"),
        "should have dropped-line markers: {}",
        &truncated[..truncated.len().min(300)]
    );
}

#[test]
fn zone_truncate_info_short_content_unchanged() {
    let content = "line 1\nline 2\nline 3\nerror[E0308]";
    let truncated = zone_truncate_info(content, 10, Some(|_line: &str| 1.0));
    assert_eq!(truncated, content);
}

#[test]
fn zone_truncate_info_preserves_order() {
    let content = make_long_content();
    let truncated = zone_truncate_info(&content, 15, Some(|line: &str| {
        if line.contains("Compiling") { 0.0 } else { 5.0 }
    }));

    // Original order must be preserved in kept lines (excluding markers)
    let orig_lines: Vec<&str> = content.lines().collect();
    let trunc_lines: Vec<&str> = truncated
        .lines()
        .filter(|l| !l.starts_with("[…") && !l.starts_with("[..."))
        .collect();

    // Each kept line should appear in same order as original
    let mut orig_iter = orig_lines.iter().peekable();
    for trunc_line in &trunc_lines {
        loop {
            if let Some(orig) = orig_iter.peek() {
                if orig == &trunc_line {
                    break;
                }
                orig_iter.next();
            } else {
                panic!("Truncated line not found in original: {}", trunc_line);
            }
        }
        orig_iter.next();
    }
}

#[test]
fn zone_truncate_info_custom_scorer_works() {
    let content = "important\n".to_string()
        + &"noise\n".repeat(50)
        + "critical\n"
        + &"noise\n".repeat(50)
        + "important\n";

    // Scorer: give "important" and "critical" high scores, "noise" low
    let truncated = zone_truncate_info(&content, 10, Some(|line: &str| {
        if line == "important" || line == "critical" {
            100.0
        } else {
            0.0
        }
    }));

    // Both important lines + critical should survive (3 high-score lines)
    let important_count = truncated.matches("important").count();
    let critical_count = truncated.matches("critical").count();
    assert!(important_count >= 1, "important lines should be preserved");
    assert!(critical_count >= 1, "critical line should be preserved");
}

#[test]
fn zone_truncate_info_falls_back_without_scorer() {
    let content = "a\n".repeat(300);
    let truncated = zone_truncate_info(&content, 10, None::<fn(&str) -> f64>);
    // Falls back to standard zone_truncate: head 5 + tail 5
    // Should still be < original length
    assert!(truncated.len() < content.len());
}
