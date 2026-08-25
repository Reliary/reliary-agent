//! Unified output compression used by both the proxy and `reliary-agent sift-exec`.
//!
//! Adaptive pipeline ported from the original sift:
//! 1. Line classification (skeleton + error/progress/summary detection)
//! 2. Strategy detection (JSON/Diff/Tabular/Prefixed/Normal)
//! 3. should_drop (removes progress bars, separators, blank runs) + expert formatting
//! 4. Fallthrough: compress_output → compress_content
//! 5. MaxwellGate entropy guard
//!
//! No blind zone truncation — all lines are classified first, then
//! should_drop/filtering removes only noise (progress, separators).
//! Warnings/errors from ANYWHERE in the output survive.
//! Every step is grammar-free: byte DFA + indentation, no keyword lists.

use reliary_sift::classify::{self, Line};
use reliary_sift::filter;

/// Compress any text using the full sift adaptive pipeline.
/// Returns the original text if no compression helps.
pub fn compress(text: &str) -> String {
    // V14: lowered short-circuit threshold from 200 → 50 chars so shorter
    // tool outputs (cargo --version, ls of small dir) still get classified.
    // The freeze cache at the MCP dispatch layer catches KV-cache risk.
    if text.len() < 50 {
        return text.to_string();
    }

    // Stage 1: Classify ALL lines (no blind zone truncation — all
    // error/warning lines must survive regardless of position).
    let lines: Vec<Line> = classify::classify(text);
    if lines.is_empty() {
        return text.to_string();
    }

    // Stage 2: Detect compression strategy from classified lines
    let raw_lines: Vec<(String, Line)> = lines.iter()
        .map(|l| (l.text.clone(), l.clone()))
        .collect();
    let strategy = classify::detect_strategy(&raw_lines);

    // Stage 3: Apply expert compression per strategy (includes should_drop
    // inside format_output — progress bars, separators, short lines stripped)
    let compressed = filter::format_output(&lines, strategy, true);

    // Stage 4: Always try output-collapse (cargo/pytest prefix runs) and
    // pick the shorter result. V14: was gated by "didn't help" but real
    // cargo output (Compiling X vY interleaved with Finished/Running) needs
    // the GLOBAL prefix collapse in compress_output, not just the consecutive
    // runs in the primary pipeline.
    let collapsed = crate::compress_output(text);
    if collapsed.len() < compressed.len() {
        return collapsed;
    }
    if compressed.len() < text.len() {
        return compressed;
    }

    // Stage 5: MaxwellGate — if information-dense, don't force compression
    let gate = reliary_sift::MaxwellGate::default();
    if gate.score(text).is_none() {
        return text.to_string();
    }

    text.to_string()
}

// Re-export from sift/lib.rs for convenience
fn classify_lines(text: &str) -> Vec<reliary_sift::ContentLine> {
    text.lines().enumerate().map(|(i, l)| reliary_sift::ContentLine {
        text: l.to_string(),
        line_type: reliary_sift::classify_line(l),
        index: i,
    }).collect()
}

fn looks_like_content(lines: &[reliary_sift::ContentLine]) -> bool {
    reliary_sift::looks_like_content(lines)
}

fn compress_lines(lines: Vec<reliary_sift::ContentLine>, aggressive: bool) -> Vec<String> {
    reliary_sift::compress_content(lines, aggressive)
}
