use ahash::AHashMap;
use std::sync::LazyLock;
use regex::Regex;

/// D2: Combined regex alternation — single pass over text instead of 10.
/// Matches all 10 patterns at once, replaces with single space.
static COMBINED_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Build alternation: each pattern's source joined with |
    // We hand-build this from the same patterns above.
    let parts = [
        r"\b(Let me (?:analyze|look|check|review|see|think|consider)\b[^.]*\.)",
        r"\b(I (?:can|would|will) need to)[^.]*\.",
        r"\b(In order to)[^.]*\.",
        r"\b(First(?:,|ly)? let me)[^.]*\.",
        r"\b(Based on (?:the|this|my|our))[^.]*\.?",
        r"\b(This means that)[^.]*\.",
        r"\b(The (?:next|final|first) step)[^.]*\.",
        r"\b(Now I(?: can| will|'ll| need to| should))[^.,;]*[,;.]?",
        r"\b(Alright|Okay|So,?|Well,?|Now,?)\s*",
        r"\b(?:essentially|basically|simply|actually|obviously|clearly|currently)\b",
    ];
    let combined = format!("(?i)(?:{})", parts.join("|"));
    Regex::new(&combined).unwrap()
});

/// Dictionary entry: a symbol known to the FTS5 index.
#[derive(Clone, Debug)]
pub struct DictEntry {
    pub symbol: String,
    pub frequency: u32,
}

/// Compression dictionary: maps known project symbols to frequency for tailored compression.
#[derive(Clone, Debug)]
pub struct CompressionDict {
    pub entries: Vec<DictEntry>,
    /// D1: AhoCorasick automaton for single-pass multi-pattern replace.
    /// Built once from entries, reused across all apply() calls.
    automaton: Option<aho_corasick::AhoCorasick>,
}

/// Simple grammar-free phrase extraction from source text.
/// Returns top-N key-value candidates found in the text.
pub fn extract_phrases(text: &str) -> Vec<(String, String)> {
    static SIG_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(?:pub\s+)?(?:fn|def|class|fun|function)\s+(\w+)").unwrap()
    });
    let mut pairs = Vec::new();
    // D6: Iterate directly without collecting into Vec first.
    let mut last_match = String::new();
    for line in text.lines().take(15) {
        let trimmed = line.trim();
        if let Some(caps) = SIG_RE.captures(trimmed) {
            last_match = caps[1].to_string();
        } else if !last_match.is_empty() && trimmed.len() > 5 && !trimmed.starts_with("use ") {
            pairs.push((last_match.clone(), trimmed.chars().take(40).collect()));
            last_match.clear();
        }
    }
    pairs
}

/// Build a compression dictionary from a list of symbols.
pub fn build_dict(symbols: &[String]) -> CompressionDict {
    let mut seen: AHashMap<&str, u32> = AHashMap::new();
    for s in symbols {
        *seen.entry(s.as_str()).or_insert(0) += 1;
    }
    let mut entries: Vec<DictEntry> = seen.into_iter()
        .filter(|(_, freq)| *freq > 1)
        .map(|(symbol, frequency)| DictEntry { symbol: symbol.to_string(), frequency })
        .collect();
    entries.sort_by_key(|b| std::cmp::Reverse(b.frequency));
    // D1: Build AhoCorasick automaton for single-pass replace.
    let automaton = if !entries.is_empty() {
        aho_corasick::AhoCorasick::builder()
            .match_kind(aho_corasick::MatchKind::LeftmostLongest)
            .build(entries.iter().map(|e| e.symbol.as_str()))
            .ok()
    } else {
        None
    };
    CompressionDict { entries, automaton }
}

impl CompressionDict {
    /// Apply dictionary: replace known long phrases with compact references.
    /// D1: AhoCorasick single-pass replace — eliminates O(n×m) scan.
    pub fn apply(&self, text: &str) -> String {
        let Some(ac) = &self.automaton else { return text.to_string(); };
        let mut result = String::with_capacity(text.len());
        let mut last_end = 0;
        for mat in ac.find_iter(text) {
            // Append text before match
            result.push_str(&text[last_end..mat.start()]);
            // Replace with [symbol]
            let entry = &self.entries[mat.pattern().as_usize()];
            result.push('[');
            result.push_str(&entry.symbol);
            result.push(']');
            last_end = mat.end();
        }
        result.push_str(&text[last_end..]);
        result
    }
}

/// Inline reasoning compression — port of gate.js v0.3.0 compressReasoning.
/// Returns compressed text if at least 40% smaller, else None.
pub fn compress_reasoning(text: &str, dict: Option<&CompressionDict>) -> Option<String> {
    let original_len = text.len();
    if original_len < 200 { return None; }
    if text.contains("```") { return None; }

    // D7: Skip the intermediate clone when dict is present.
    let mut t = if let Some(d) = dict { d.apply(text) } else { text.to_string() };

    // D2: Single combined regex instead of 10 separate.
    t = COMBINED_PATTERN.replace_all(&t, " ").into_owned();
    t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    // M7: Integer comparison instead of f64 — t.len() * 5 < original_len * 3.
    if t.len() * 5 < original_len * 3 { Some(t) } else { None }
}

// ============================================================================
// SRCR — Signal-Preserving Compression Rate
// ============================================================================
//
// SRCR measures compression quality. Returns three values:
//   (srcr, preservation_rate, compression_rate)
//
//   srcr = preservation_rate * compression_rate
//
// Both are in [0.0, 1.0]. High srcr = aggressive compression that kept signal.
// Low srcr = either didn't compress much, OR compressed but lost signal.
//
// Use case: gate compression behind a quality floor. If srcr < floor,
// the compression was destructive and we ship the original instead.
//
// Grammar-free: uses pure substring matching on extracted identifier-like
// substrings (alphanumeric runs ≥ 4 chars). No AST, no parser, no language
// detection.

/// Extract preservation targets from text — alphanumeric runs ≥ 4 chars.
/// These are the substrings whose presence in compressed output signals that
/// we kept project-specific content (function names, error codes, paths).
fn extract_preservation_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            if current.len() >= 4 {
                // M8: mem::take moves buffer — zero-copy instead of clone+clear.
                targets.push(std::mem::take(&mut current));
            }
            current.clear();
        }
    }
    if current.len() >= 4 {
        targets.push(current);
    }
    targets
}

/// Compute preservation rate: fraction of UNIQUE preservation targets that
/// appear in the compressed output. Uses HashSet semantics — duplicates in
/// original are counted once. This matches sift's preservation behavior
/// (collapsing duplicate lines via [N+ more] doesn't lose signal).
pub fn preservation_hit_rate(original: &str, compressed: &str) -> f64 {
    use std::collections::HashSet;
    let targets = extract_preservation_targets(original);
    if targets.is_empty() {
        return 1.0; // no targets = nothing to lose = full preservation
    }
    let unique: HashSet<&str> = targets.iter().map(|s| s.as_str()).collect();
    let preserved = unique.iter().filter(|t| compressed.contains(*t)).count();
    preserved as f64 / unique.len() as f64
}

/// Full SRCR computation. Returns (srcr, preservation, compression) tuple.
///   preservation = preservation_hit_rate(original, compressed)
///   compression = 1.0 - (compressed_len / original_len)  [0.0 = no compression, 1.0 = full]
///   srcr = preservation * compression
pub fn compute_srcr(original: &str, compressed: &str) -> (f64, f64, f64) {
    let preservation = preservation_hit_rate(original, compressed);
    let original_len = original.len() as f64;
    let compressed_len = compressed.len() as f64;
    let compression = if original_len > 0.0 {
        (1.0 - compressed_len / original_len).max(0.0)
    } else {
        0.0
    };
    (preservation * compression, preservation, compression)
}

/// Convenience: compute SRCR for a compression candidate.
/// Skips computation for too-small inputs (returns neutral 0.0, 1.0, 0.0).
pub fn srcr_for_compression(original: &str, compressed: &str) -> (f64, f64, f64) {
    if original.len() < 50 || compressed.len() >= original.len() {
        return (0.0, 1.0, 0.0); // Nothing compressed or too small to measure
    }
    compute_srcr(original, compressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_phrases() {
        let text = "pub fn validate_config(role: &str) {\n    load_config(role)\n}";
        let p = extract_phrases(text);
        assert!(!p.is_empty());
    }

    #[test]
    fn test_build_dict() {
        let symbols = vec!["validate_config".to_string(), "line_zone".to_string(), "validate_config".to_string()];
        let dict = build_dict(&symbols);
        assert!(!dict.entries.is_empty() && dict.entries.len() <= 2);
    }

    #[test]
    fn test_compress_reasoning() {
        // Must be > 300 chars to bypass the length guard, contain enough fluff for 40%+ reduction
        let text = "Let me analyze this bug and check the function. I need to verify the threshold. Let me consider the edge cases here. I think there is a bug in the comparison. Let me verify my understanding by tracing through the code. Based on this analysis I believe the fix should change the operator. Alright I will apply the fix now. Essentially the problem is a logic error in config. Let me check the validator module too. Lets consider what happens when the input is empty. I believe the issue is the threshold comparison. Let me think about what happens with valid data. Alright this should work after the change.";
        assert!(text.len() > 300, "test text must be > 300 chars, got {}", text.len());
        let r = compress_reasoning(text, None);
        assert!(r.is_some(), "compression should return Some for long fluff text ({} chars)", text.len());
        let c = r.unwrap();
        assert!((c.len() as f64) < text.len() as f64 * 0.75, "compressed text should be shorter ({} vs {})", c.len(), text.len());
    }

    #[test]
    fn test_short_text_skipped() {
        assert!(compress_reasoning("a b c", None).is_none());
    }

    // SRCR tests

    #[test]
    fn test_extract_preservation_targets() {
        let text = "error[E0308] at src/lib.rs:42\nFAILED: test_001";
        let targets = extract_preservation_targets(text);
        assert!(targets.contains(&"E0308".to_string()));
        // Note: "lib" is only 3 chars (threshold), so it's skipped
        assert!(targets.contains(&"FAILED".to_string()));
        assert!(targets.contains(&"test_001".to_string()));
    }

    #[test]
    fn test_extract_preservation_targets_short_skipped() {
        let text = "a b c def ghij";
        let targets = extract_preservation_targets(text);
        // "def" is only 3 chars, skipped. "ghij" is 4, kept.
        assert!(!targets.contains(&"def".to_string()));
        assert!(targets.contains(&"ghij".to_string()));
    }

    #[test]
    fn test_preservation_hit_rate_full() {
        let original = "error[E0308] at src/lib.rs:42\nFAILED: test_001";
        let compressed = original; // unchanged
        assert!((preservation_hit_rate(original, compressed) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_preservation_hit_rate_unique_dedup() {
        // 5 duplicate "error[E0308]" lines collapse to 1 in compressed
        // SRCR counts unique targets only → still preserved
        let original = "error[E0308] a\nerror[E0308] b\nerror[E0308] c\nerror[E0308] d\nerror[E0308] e";
        let compressed = "error[E0308] a [4+ more]";
        let rate = preservation_hit_rate(original, compressed);
        assert!((rate - 1.0).abs() < 0.001, "Unique target E0308 preserved: got {}", rate);
    }

    #[test]
    fn test_preservation_hit_rate_partial() {
        let original = "error[E0308] at src/lib.rs:42\nFAILED: test_001 with assert_eq failed";
        let compressed = "error[E0308] at src/lib.rs:42 [collapsed rest]";
        // "lib" is < 4 chars so not a target. FAILED and test_001 lost.
        // Only E0308 preserved (1 of 3 unique: E0308, FAILED, test_001)
        let rate = preservation_hit_rate(original, compressed);
        assert!(rate < 0.5, "Expected low preservation, got {}", rate);
        assert!(rate > 0.0, "Expected non-zero, got {}", rate);
    }

    #[test]
    fn test_preservation_hit_rate_no_targets() {
        // Text with no targets ≥ 4 chars
        let original = "a b c d e f g";
        let rate = preservation_hit_rate(original, original);
        assert!((rate - 1.0).abs() < 0.001, "No targets = full preservation");
    }

    #[test]
    fn test_compute_srcr_high_compression_high_preservation() {
        // Sift-style collapse: 5 duplicate lines → 1 line + [N+ more]
        let original = "error[E0308] mismatched types at src/lib.rs:42\n\
                       error[E0308] mismatched types at src/lib.rs:43\n\
                       error[E0308] mismatched types at src/lib.rs:44\n\
                       error[E0308] mismatched types at src/lib.rs:45\n\
                       error[E0308] mismatched types at src/lib.rs:46";
        let compressed = "error[E0308] mismatched types at src/lib.rs:42 [4+ more]";
        let (srcr, pres, comp) = compute_srcr(original, compressed);
        assert!(pres > 0.5, "preservation should be high, got {}", pres);
        assert!(comp > 0.5, "compression should be high, got {}", comp);
        assert!(srcr > 0.25, "srcr should be > 0.25, got {}", srcr);
    }

    #[test]
    fn test_srcr_for_compression_small_skipped() {
        let (srcr, _, _) = srcr_for_compression("hi", "hi");
        assert_eq!(srcr, 0.0); // too small
    }

    #[test]
    fn test_srcr_for_compression_unchanged() {
        let text = "error[E0308] at src/lib.rs:42 with some more context here for length";
        let (srcr, _, comp) = srcr_for_compression(text, text);
        assert_eq!(comp, 0.0); // no compression
        assert_eq!(srcr, 0.0);
    }
}
