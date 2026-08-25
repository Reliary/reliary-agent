//! Arc 27 Lever 6 — Boltzmann uncertainty quantification for find-references.
//!
//! Convert raw similarity scores into calibrated probabilities using a soft-max
//! over energy (negative log score). Report top-N hits WITH probabilities and
//! percentile. Hard cutoff: discard hits below probability tau (default 0.05).

/// One hit with calibrated probability.
#[derive(Clone, Debug, PartialEq)]
pub struct ProbabilisticHit {
    /// Original identifier: file path.
    pub file: String,
    /// Line number (1-based).
    pub line: i32,
    /// Column number (0-based).
    pub col: i32,
    /// Raw similarity score from type_flow.
    pub score: f32,
    /// Calibrated probability under Boltzmann distribution.
    pub probability: f32,
    /// Percentile rank (1.0 = highest probability, 0.0 = lowest).
    pub percentile: f32,
}

/// Raw scored hit (passed in by caller).
#[derive(Clone, Debug, PartialEq)]
pub struct RawHit {
    pub file: String,
    pub line: i32,
    pub col: i32,
    pub score: f32,
}

/// Compute Boltzmann probabilities for a list of raw scores.
/// Direct softmax over scores: P ∝ exp(score / T). Higher score → higher prob.
/// Returns probabilities that sum to ~1.0.
pub fn softmax_with_temperature(scores: &[f32], temperature: f32) -> Vec<f32> {
    if scores.is_empty() { return Vec::new(); }
    if temperature <= 0.0 { return vec![0.0; scores.len()]; }
    // Apply temperature: scaled = score / T.
    let scaled: Vec<f32> = scores.iter().map(|s| s / temperature).collect();
    // Standard softmax (numerically stable).
    let max = scaled.iter().fold(f32::NEG_INFINITY, |a, b| a.max(*b));
    let mut exps: Vec<f32> = scaled.iter().map(|e| (e - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    if sum == 0.0 || sum.is_nan() {
        return vec![1.0 / scores.len() as f32; scores.len()];
    }
    for x in exps.iter_mut() { *x /= sum; }
    exps
}

/// Convert raw hits to probabilistic hits with calibration.
pub fn calibrate_hits(hits: &[RawHit], temperature: f32, tau: f32) -> Vec<ProbabilisticHit> {
    let scores: Vec<f32> = hits.iter().map(|h| h.score).collect();
    let probs = softmax_with_temperature(&scores, temperature);
    let n = hits.len();
    // Compute percentile (rank-based: 1.0 for highest, 0.0 for lowest).
    let mut sorted_indices: Vec<usize> = (0..n).collect();
    sorted_indices.sort_by(|&a, &b| probs[b].partial_cmp(&probs[a]).unwrap_or(std::cmp::Ordering::Equal));
    let mut percentiles = vec![0.0f32; n];
    for (rank, &idx) in sorted_indices.iter().enumerate() {
        percentiles[idx] = if n > 1 { 1.0 - rank as f32 / (n - 1) as f32 } else { 1.0 };
    }

    let mut out = Vec::new();
    for (i, h) in hits.iter().enumerate() {
        // Hard cutoff: discard hits below tau.
        if probs[i] < tau { continue; }
        out.push(ProbabilisticHit {
            file: h.file.clone(),
            line: h.line,
            col: h.col,
            score: h.score,
            probability: probs[i],
            percentile: percentiles[i],
        });
    }
    // Sort by probability descending.
    out.sort_by(|a, b| b.probability.partial_cmp(&a.probability).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Calibration parameters tuned by empirical analysis.
pub fn default_temperature(n_hits: usize) -> f32 {
    // Higher temperature (more uniform) when many hits; lower (sharper) for few.
    if n_hits < 10 { 1.5 } else if n_hits < 100 { 2.0 } else { 3.0 }
}

/// Confidence interval for a probability (Wilson score 95% CI).
pub fn confidence_interval_95(prob: f32, n: usize) -> (f32, f32) {
    if n == 0 { return (0.0, 1.0); }
    let z = 1.96; // 95% CI.
    let n_f = n as f32;
    let denom = 1.0 + z * z / n_f;
    let center = (prob + z * z / (2.0 * n_f)) / denom;
    let spread = z * (prob * (1.0 - prob) / n_f + z * z / (4.0 * n_f * n_f)).sqrt() / denom;
    ((center - spread).max(0.0), (center + spread).min(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_softmax_sums_to_one() {
        let probs = softmax_with_temperature(&[0.5, 0.7, 0.9], 1.0);
        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 0.001, "sum={}", sum);
    }

    #[test]
    fn test_softmax_temperature_diversity() {
        // High T → closer to uniform.
        let low_t = softmax_with_temperature(&[0.1, 0.5, 0.9], 0.5);
        let high_t = softmax_with_temperature(&[0.1, 0.5, 0.9], 5.0);
        assert!(low_t[2] > high_t[2], "high T should flatten distribution");
    }

    #[test]
    fn test_calibrate_hits_cuts_tau() {
        let hits = vec![
            RawHit { file: "a.rs".into(), line: 1, col: 0, score: 5.0 },
            RawHit { file: "b.rs".into(), line: 2, col: 0, score: 1.0 },
            RawHit { file: "c.rs".into(), line: 3, col: 0, score: 0.1 },
        ];
        // With scores 5, 1, 0.1 and T=1, the low score 0.1 should drop below tau=0.05.
        let calibrated = calibrate_hits(&hits, 1.0, 0.05);
        assert!(calibrated.len() < 3, "low-score hit should be cut, got {} hits", calibrated.len());
        assert!(calibrated.len() >= 1);
    }

    #[test]
    fn test_calibrate_hits_returns_top_prob_first() {
        let hits = vec![
            RawHit { file: "a.rs".into(), line: 1, col: 0, score: 0.5 },
            RawHit { file: "b.rs".into(), line: 2, col: 0, score: 0.9 },
            RawHit { file: "c.rs".into(), line: 3, col: 0, score: 0.7 },
        ];
        let calibrated = calibrate_hits(&hits, 1.0, 0.0);
        assert_eq!(calibrated[0].file, "b.rs", "highest prob first");
    }

    #[test]
    fn test_calibrate_hits_empty() {
        let calibrated = calibrate_hits(&[], 1.0, 0.0);
        assert!(calibrated.is_empty());
    }

    #[test]
    fn test_confidence_interval() {
        let (lo, hi) = confidence_interval_95(0.5, 100);
        assert!(lo < 0.5 && hi > 0.5, "interval should contain 0.5");
        assert!(lo >= 0.0 && hi <= 1.0);
    }
}
