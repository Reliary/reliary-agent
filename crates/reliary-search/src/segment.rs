//! Segmenter spike (SEGMENTER_SPIKE.md steps 1, 1b) — NOT wired into ingest.
//!
//! Step 1b: Utiyama & Isahara (2001) DP segmentation replaces threshold
//! selection. Each candidate segment is scored by its own unigram
//! log-likelihood (uniform-vocabulary blocks earn their segment by fit,
//! not by edge contrast); boundaries pay a BIC penalty; brace events get
//! a discounted cost (disjoint evidence, no shared score pool).
//!
//! Inputs are stems (project vocabulary, not language keywords) and
//! brace event lines. No token values are read anywhere in this module.

use crate::brace_graph::BraceNode;
use ahash::AHashMap;

/// Minimum atomic segment length in lines (Hearst's floor: TextTiling
/// clips segments below 2-5 pseudosentences; we take the low end).
pub const MIN_SEG_LEN: usize = 2;
/// Maximum atomic segment length. Bounds DP cost at O(n*MAX). Blocks
/// longer than this are split; boundary *starts* (what typing needs)
/// are unaffected. Spike cap, revisit in step 4 if recall says so.
pub const MAX_SEG_LEN: usize = 40;
/// Prior evidence of one brace event, in nats (one bit). Applied as a
/// cost discount for segments opening near a brace event.
pub const BRACE_NATS: f64 = std::f64::consts::LN_2;
/// Max distance (lines) from a brace event for the discount.
pub const BRACE_WINDOW: i64 = 1;

/// Feature flag. Default off; spike module is unreachable otherwise.
/// (Nothing in ingest calls this yet — the flag guards future wiring.)
pub fn segment_enabled() -> bool {
    std::env::var("RELIARY_SEGMENTER").as_deref() == Ok("1")
}

/// A contiguous block between two boundaries. Lines are 1-indexed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub start_line: usize,
    pub end_line: usize,
    /// True when the opening boundary coincided with a brace event.
    pub brace_backed: bool,
}

/// Collect every block-open line (1-indexed `start_line`) in the tree.
pub fn brace_events(root: &BraceNode) -> Vec<i64> {
    let mut out = Vec::new();
    collect_opens(root, &mut out, true);
    out.sort_unstable();
    out.dedup();
    out
}

fn collect_opens(node: &BraceNode, out: &mut Vec<i64>, is_root: bool) {
    if !is_root {
        out.push(node.start_line as i64);
    }
    for child in &node.children {
        collect_opens(child, out, false);
    }
}

/// Segment `lines` (1-indexed logic) given brace open events (1-indexed).
/// Returns blocks covering all lines. A2a: takes borrowed lines — the
/// caller no longer clones every line into `Vec<String>` just to feed
/// this; `scan_identifiers` only needs `&str`.
pub fn segment_lines<S: AsRef<str>>(lines: &[S], events: &[i64]) -> Vec<Segment> {
    if lines.is_empty() {
        return Vec::new();
    }
    segment_owned(lines, events)
}

fn segment_owned<S: AsRef<str>>(lines: &[S], events: &[i64]) -> Vec<Segment> {
    let n = lines.len();
    let tokenized: Vec<Vec<String>> =
        lines.iter().map(|l| crate::scan_identifiers(l.as_ref())).collect();
    let total_tokens: usize = tokenized.iter().map(|l| l.len()).sum();
    if total_tokens == 0 {
        // No lexical signal at all: one block, nothing to place.
        return vec![Segment { start_line: 1, end_line: n, brace_backed: false }];
    }

    // seg_cost(a, b): cost of one atomic segment over 1-indexed lines a..=b.
    // Utiyama & Isahara gain formulation: the segment earns its dedicated
    // unigram model only insofar as it beats the file background model,
    // plus a BIC penalty per segment, minus one bit of brace prior.
    // gain = Σ c_seg * ln((c_seg/T_seg) / p_file)
    // Empty (zero-token) spans are unplaceable: infinite cost.
    let mut file_counts: AHashMap<&str, usize> = AHashMap::default();
    for line in &tokenized {
        for tok in line {
            *file_counts.entry(tok.as_str()).or_insert(0) += 1;
        }
    }
    let p_file = |tok: &str| -> f64 {
        file_counts.get(tok).copied().unwrap_or(0) as f64 / total_tokens as f64
    };
    let near_brace = |a: usize| {
        a > 1 && events.iter().any(|e| (*e - a as i64).abs() <= BRACE_WINDOW)
    };
    // Full two-part MDL per segment (Grünwald): naming the support costs
    // V_seg * ln(V_file) nats under a uniform code over the file
    // vocabulary, plus BIC 0.5 * V_seg * ln(T_seg) for parameter values.
    // No free constants: every term is a codelength.
    let log_v_file = (file_counts.len() as f64 + 1.0).ln();
    let seg_cost = |a: usize, b: usize| -> f64 {
        let mut counts: AHashMap<&str, usize> = AHashMap::default();
        let mut t = 0usize;
        for line in &tokenized[a - 1..b.min(n)] {
            for tok in line {
                *counts.entry(tok.as_str()).or_insert(0) += 1;
                t += 1;
            }
        }
        if t == 0 {
            return f64::INFINITY;
        }
        let tf = t as f64;
        // Gain of the dedicated model over the file background model.
        let gain: f64 = counts
            .iter()
            .map(|(tok, &c)| c as f64 * ((c as f64 / tf) / p_file(tok)).ln())
            .sum();
        let v = counts.len() as f64;
        let penalty = v * log_v_file + 0.5 * v * tf.ln();
        let discount = if near_brace(a) { BRACE_NATS } else { 0.0 };
        // Cost = what the segment fails to explain beyond background,
        // plus model complexity. Lower is better; the DP minimizes total.
        -gain + (penalty - discount).max(0.0)
    };

    // DP: dp[i] = min cost covering lines 1..=i. Lengths bounded by
    // MIN/MAX_SEG_LEN (1-line spans allowed only when the file is tiny).
    // Ties keep the earliest split (strict <): deterministic, fewest
    // segments on equal cost.
    let mut dp = vec![f64::INFINITY; n + 1];
    let mut prev: Vec<usize> = vec![0; n + 1];
    dp[0] = 0.0;
    for i in 1..=n {
        let lo = i.saturating_sub(MAX_SEG_LEN).max(1);
        for j in lo..=i {
            let len = i - j + 1;
            if len < MIN_SEG_LEN && n > MIN_SEG_LEN {
                continue;
            }
            if !dp[j - 1].is_finite() {
                continue;
            }
            let c = dp[j - 1] + seg_cost(j, i);
            if c < dp[i] {
                dp[i] = c;
                prev[i] = j;
            }
        }
        // Tiny-file fallback: no legal span (shouldn't happen, but the DP
        // must always cover — take the whole file rather than emit nothing).
        if !dp[i].is_finite() {
            dp[i] = dp[0] + seg_cost(1, i);
            prev[i] = 1;
        }
    }

    // Reconstruct.
    let mut starts = Vec::new();
    let mut i = n;
    while i > 0 {
        let j = prev[i].max(1);
        starts.push(j);
        if j <= 1 {
            break;
        }
        i = j - 1;
    }
    starts.reverse();
    starts.push(n + 1);

    starts
        .windows(2)
        .map(|w| Segment {
            start_line: w[0],
            end_line: w[1] - 1,
            brace_backed: near_brace(w[0]),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_no_segments() {
        assert!(segment_lines::<String>(&[], &[]).is_empty());
    }

    #[test]
    fn single_cohesive_file_is_one_segment() {
        let lines = vec!["int foo() {".to_string(), "return bar;".to_string(), "}".to_string()];
        let segs = segment_lines(&lines, &[1]);
        assert!(!segs.is_empty());
        assert_eq!(segs[0].start_line, 1);
        assert_eq!(segs.last().unwrap().end_line, lines.len());
    }

    #[test]
    fn segments_cover_all_lines_without_gaps() {
        let lines: Vec<String> = (0..20).map(|i| format!("int f{i}(int x) {{ return x; }}")).collect();
        let segs = segment_lines(&lines, &[]);
        let mut next = 1;
        for s in &segs {
            assert_eq!(s.start_line, next);
            next = s.end_line + 1;
        }
        assert_eq!(next - 1, lines.len());
    }

    #[test]
    fn flag_defaults_off() {
        assert!(!segment_enabled());
    }
}
