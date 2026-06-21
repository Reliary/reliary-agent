//! Smoke tests for the adaptive sift pipeline.
//!
//! These tests document which content types the adaptive pipeline DOES compress
//! and which it doesn't. They were extracted from the break-ceiling-p1 smoke
//! investigation. The findings:
//!
//! | Content type                     | Compression |
//! |----------------------------------|-------------|
//! | JSON array                       | none        |
//! | pytest tabular                   | none        |
//! | grep output                      | minimal     |
//! | ls -la                           | none        |
//! | cargo REPEAT (same crate)        | 95% savings |
//! | cargo MIXED (different crates)   | none        |
//!
//! The adaptive pipeline fundamentally cannot compress cargo output where each
//! line has a unique crate name — skeleton hashes differ per line.

use reliary_sift::*;

fn compress_via_adaptive(content: &str) -> String {
    let classified = classify::classify(content);
    let raw_lines: Vec<(String, classify::Line)> = classified.iter()
        .map(|l| (l.text.clone(), l.clone()))
        .collect();
    let strategy = classify::detect_strategy(&raw_lines);
    filter::format_output(&classified, strategy)
}

#[test]
fn cargo_repeat_same_crate_compresses() {
    // Real cargo: every crate line has a different name, so this never happens
    // in practice. But if the same crate is compiled repeatedly (rare), adaptive
    // sift compresses 95%+.
    let content: String = (0..30)
        .map(|i| format!("   Compiling serde v1.0.{}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let compressed = compress_via_adaptive(&content);
    assert!(
        compressed.len() < content.len() / 2,
        "Repeated same-crate cargo should compress <50%: got {}%",
        compressed.len() as f64 / content.len() as f64 * 100.0
    );
}

#[test]
fn cargo_mixed_crates_does_not_compress() {
    // Real cargo: different crates per line. Each line has unique skeleton.
    let crates = ["serde", "tokio", "regex", "anyhow", "thiserror"];
    let content: String = (0..24)
        .map(|i| format!("   Compiling {} 1.{}.0", crates[i % 5], i))
        .collect::<Vec<_>>()
        .join("\n");
    let compressed = compress_via_adaptive(&content);
    // No compression on mixed-crate cargo — this documents the limitation.
    assert!(
        compressed.len() >= content.len() * 9 / 10,
        "Mixed-crate cargo should NOT compress much: got {}% (original {} chars)",
        compressed.len() as f64 / content.len() as f64 * 100.0,
        content.len()
    );
}

#[test]
fn test_passed_runs_collapse_to_ok_count() {
    // Test ... ok lines collapse to [N ok] — this DOES work.
    let content = (0..15)
        .map(|i| format!("test test_{} ... ok", i))
        .collect::<Vec<_>>()
        .join("\n");
    let compressed = compress_via_adaptive(&content);
    assert!(
        compressed.contains("[15 ok]") || compressed.contains("[") && compressed.len() < content.len(),
        "Test ... ok runs should collapse: got '{}'",
        &compressed[..compressed.len().min(100)]
    );
}
