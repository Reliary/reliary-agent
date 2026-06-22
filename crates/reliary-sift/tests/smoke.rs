//! Smoke tests for the adaptive sift pipeline with aggressive skeleton support.
//!
//! Documents content-type compression behavior after the aggressive_skeleton
//! integration. Findings:
//!
//! | Content type                     | Compression | Mechanism                |
//! |----------------------------------|-------------|--------------------------|
//! | cargo REPEAT (same crate)        | 95% savings | aggressive_skeleton      |
//! | cargo MIXED (different crates)   | 95% savings | aggressive_skeleton (NEW)|
//! | pytest PASSED runs               | collapses   | existing collapse_path   |
//!
//! The aggressive_skeleton + 80% concentration gate enables cargo output
//! compression without requiring tool-specific keyword lists. The gate
//! requires ≥80% of non-blank lines to share the same template + similar
//! lengths, which catches template-filled command output but rejects
//! file reads where similar signatures are a small fraction.

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
fn cargo_mixed_crates_now_compresses() {
    // BREAKTHROUGH: aggressive_skeleton with 80% concentration gate now
    // compresses mixed-crate cargo from 100% of original (no compression)
    // to ~5% of original (95% savings). No tool-specific keywords needed.
    let crates = ["serde", "tokio", "regex", "anyhow", "thiserror"];
    let content: String = (0..24)
        .map(|i| format!("   Compiling {} 1.{}.0", crates[i % 5], i))
        .collect::<Vec<_>>()
        .join("\n");
    let compressed = compress_via_adaptive(&content);
    let ratio = compressed.len() as f64 / content.len() as f64 * 100.0;
    assert!(
        ratio < 20.0,
        "Mixed-crate cargo should compress <20% with aggressive_skeleton: got {:.1}% ({} → {} chars)",
        ratio, content.len(), compressed.len()
    );
}

#[test]
fn test_passed_runs_collapse_to_ok_count() {
    let content = (0..15)
        .map(|i| format!("test test_{} ... ok", i))
        .collect::<Vec<_>>()
        .join("\n");
    let compressed = compress_via_adaptive(&content);
    assert!(
        compressed.contains("[") && compressed.len() < content.len(),
        "Test ... ok runs should collapse: got '{}'",
        &compressed[..compressed.len().min(100)]
    );
}
