use ahash::AHashMap;
use std::sync::LazyLock;
use regex::Regex;

/// Pre-compiled reasoning compression patterns — compiled once at startup.
static COMPRESSION_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)\b(Let me (analyze|look|check|review|see|think|consider)\b[^.]*\.)",
        r"(?i)\b(I (?:can|would|will) need to)[^.]*\.",
        r"(?i)\b(In order to)[^.]*\.",
        r"(?i)\b(First(?:,|ly)? let me)[^.]*\.",
        r"(?i)\b(Based on (?:the|this|my|our))[^.]*\.?",
        r"(?i)\b(This means that)[^.]*\.",
        r"(?i)\b(The (?:next|final|first) step)[^.]*\.",
        r"(?i)\b(Now I(?: can| will|'ll| need to| should))[^.,;]*[,;.]?",
        r"(?i)\b(Alright|Okay|So,?|Well,?|Now,?)\s*",
        r"(?i)\bessentially|basically|simply|actually|obviously|clearly|currently\b",
    ]
    .into_iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
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
}

/// Simple grammar-free phrase extraction from source text.
/// Returns top-N key-value candidates found in the text.
pub fn extract_phrases(text: &str) -> Vec<(String, String)> {
    static SIG_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(?:pub\s+)?(?:fn|def|class|fun|function)\s+(\w+)").unwrap()
    });
    let mut pairs = Vec::new();
    let lines: Vec<&str> = text.lines().take(15).collect();
    let mut last_match = String::new();
    for line in &lines {
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
    let mut seen: AHashMap<String, u32> = AHashMap::new();
    for s in symbols {
        *seen.entry(s.clone()).or_insert(0) += 1;
    }
    let mut entries: Vec<DictEntry> = seen.into_iter()
        .filter(|(_, freq)| *freq > 1)
        .map(|(symbol, frequency)| DictEntry { symbol, frequency })
        .collect();
    entries.sort_by_key(|b| std::cmp::Reverse(b.frequency));
    CompressionDict { entries }
}

impl CompressionDict {
    /// Apply dictionary: replace known long phrases with compact references.
    pub fn apply(&self, text: &str) -> String {
        let mut result = text.to_string();
        for entry in &self.entries {
            if result.contains(&entry.symbol) {
                result = result.replace(&entry.symbol, &format!("[{}]", entry.symbol));
            }
        }
        result
    }
}

/// Inline reasoning compression — port of gate.js v0.3.0 compressReasoning.
/// Returns compressed text if at least 40% smaller, else None.
pub fn compress_reasoning(text: &str, dict: Option<&CompressionDict>) -> Option<String> {
    let original_len = text.len();
    if original_len < 200 { return None; }
    if text.contains("```") || text.contains("//") || text.contains("/*")
        || text.contains("src/") || text.contains(".rs:") || text.contains(".py:")
        || text.contains(".md")
    { return None; }

    let mut t = text.to_string();
    if let Some(d) = dict { t = d.apply(&t); }

    for re in COMPRESSION_PATTERNS.iter() {
        t = re.replace_all(&t, " ").to_string();
    }
    t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if (t.len() as f64) < original_len as f64 * 0.6 { Some(t) } else { None }
}

// ── SRCR: Semantic-Relative Compression Ratio ─────────────────────────────────
//
// Ported from llm-semantic-transport's compute_srcr / preservation_hit_rate.
// SRCR = preservation_rate × compression_rate
// Measures compression QUALITY, not just ratio. High SRCR = saved tokens AND
// kept important content. Low SRCR = either didn't compress or destroyed signal.

/// Compute preservation hit rate: fraction of unique target strings present in text.
/// Uses unique strings (HashSet) rather than occurrences — duplicates collapsing
/// is expected compression, not signal loss. Returns 1.0 if no targets.
pub fn preservation_hit_rate(text: &str, targets: &[&str]) -> f64 {
    use std::collections::HashSet;
    let unique: HashSet<&str> = targets.iter().filter(|t| !t.is_empty()).copied().collect();
    if unique.is_empty() {
        return 1.0;
    }
    let hits = unique.iter().filter(|t| text.contains(*t)).count();
    hits as f64 / unique.len() as f64
}

/// SRCR safety floor: returns true if post-compression SRCR is too low,
/// meaning compression destroyed too much signal and original should be used.
/// Returns false if SRCR is acceptable (preservation adequate).
pub fn srcr_below_floor(original: &str, compressed: &str, floor: f64) -> bool {
    let (srcr, _pres, _comp) = srcr_for_compression(original, compressed);
    srcr < floor
}

/// Compute SRCR: preservation_rate × (1 - tokens_out/tokens_in).
/// Range: 0.0 (no compression or total loss) to ~1.0 (perfect compression, full preservation).
pub fn compute_srcr(preservation_rate: f64, tokens_in: usize, tokens_out: usize) -> f64 {
    if tokens_in == 0 {
        return 0.0;
    }
    let compression_rate = 1.0 - (tokens_out as f64 / tokens_in as f64);
    let raw = preservation_rate * compression_rate;
    (raw * 10000.0).round() / 10000.0
}

/// Measure SRCR for a single compression operation.
/// Takes original text, compressed text, and preservation targets.
/// Returns (srcr, preservation_rate, compression_rate).
pub fn measure_srcr(original: &str, compressed: &str, targets: &[&str]) -> (f64, f64, f64) {
    let tokens_in = original.len();
    let tokens_out = compressed.len();
    if tokens_in == 0 {
        return (0.0, 1.0, 0.0);
    }
    let preservation = preservation_hit_rate(compressed, targets);
    let compression_rate = 1.0 - (tokens_out as f64 / tokens_in as f64);
    let srcr = compute_srcr(preservation, tokens_in, tokens_out);
    (srcr, preservation, compression_rate)
}

/// Grammar-free extraction of preservation targets from tool output.
/// Different content types have different "important" tokens:
/// - Error markers (FAILED, error:, E\d+, Traceback, panic, assertion)
/// - File paths and identifiers
///
/// Returns a Vec of string slices (owned to avoid lifetime complexity).
pub fn extract_preservation_targets(content: &str) -> Vec<String> {
    static ERROR_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(FAILED|error:|E\d{4}|Traceback|panic|assertion|warning:)").unwrap()
    });
    static PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b[A-Za-z_][A-Za-z0-9_/.]+\.(?:rs|py|js|ts|go|java|c|cpp|h|md|toml|json|yaml|yml)\b").unwrap()
    });
    static IDENT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b[A-Za-z_][A-Za-z0-9_]{3,40}\b").unwrap()
    });

    let mut targets = Vec::new();

    // Always preserve error markers
    for cap in ERROR_RE.find_iter(content) {
        targets.push(cap.as_str().to_string());
    }

    // Preserve file paths
    for cap in PATH_RE.find_iter(content) {
        targets.push(cap.as_str().to_string());
    }

    // If no errors or paths, extract identifiers (for file reads, code output)
    if targets.is_empty() {
        let mut idents: Vec<String> = IDENT_RE
            .find_iter(content)
            .take(50) // cap to avoid explosion
            .map(|m| m.as_str().to_string())
            .collect();
        idents.sort();
        idents.dedup();
        // Keep top 20 by frequency (grammar-free: count occurrences)
        let mut freq: AHashMap<String, u32> = AHashMap::new();
        for m in IDENT_RE.find_iter(content) {
            *freq.entry(m.as_str().to_string()).or_insert(0) += 1;
        }
        let mut sorted: Vec<(String, u32)> = freq.into_iter().collect();
        sorted.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        targets = sorted.into_iter().take(20).map(|(s, _)| s).collect();
    }

    // Dedup and cap total targets
    targets.sort();
    targets.dedup();
    if targets.len() > 30 {
        targets.truncate(30);
    }
    targets
}

/// Compute SRCR for a compression operation with automatic target extraction.
/// Convenience wrapper: extracts targets from original, then measures.
pub fn srcr_for_compression(original: &str, compressed: &str) -> (f64, f64, f64) {
    if original.len() < 50 || compressed.len() >= original.len() {
        return (0.0, 1.0, 0.0); // Nothing compressed or too small
    }
    let targets_owned = extract_preservation_targets(original);
    let targets: Vec<&str> = targets_owned.iter().map(|s| s.as_str()).collect();
    measure_srcr(original, compressed, &targets)
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

    // ── SRCR tests ──

    #[test]
    fn test_preservation_hit_rate() {
        let text = "error in src/main.rs: E0308 failed to compile";
        let targets = vec!["error", "E0308", "src/main.rs", "missing_thing"];
        let rate = preservation_hit_rate(text, &targets);
        assert!((rate - 0.75).abs() < 0.01, "3 of 4 targets found, got {}", rate);
    }

    #[test]
    fn test_preservation_no_targets() {
        assert!((preservation_hit_rate("anything", &[]) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_srcr() {
        // 50% compression, 100% preservation → SRCR = 0.5
        let s = compute_srcr(1.0, 1000, 500);
        assert!((s - 0.5).abs() < 0.01, "expected 0.5, got {}", s);
    }

    #[test]
    fn test_compute_srcr_zero_input() {
        assert_eq!(compute_srcr(1.0, 0, 0), 0.0);
    }

    #[test]
    fn test_srcr_for_compression_good() {
        // Compress cargo test output: keep errors, drop progress lines
        let original = "Compiling crate v0.1.0\nCompiling dep v1.0\nFinished\nerror[E0308]: mismatched types\n  --> src/main.rs:42:5\nFAILED";
        let compressed = "error[E0308]: mismatched types\n  --> src/main.rs:42:5\nFAILED";
        let (srcr, pres, comp) = srcr_for_compression(original, compressed);
        assert!(pres >= 0.8, "preservation should be high, got {}", pres);
        assert!(comp > 0.3, "compression should be meaningful, got {}", comp);
        assert!(srcr > 0.2, "SRCR should be positive, got {}", srcr);
    }

    #[test]
    fn test_srcr_for_compression_destructive() {
        // Aggressive compression that destroys errors
        let original = "error[E0308]: mismatched types\n  --> src/main.rs:42:5\nFAILED\nassertion failed";
        let compressed = "[compressed: 3 lines]";
        let (srcr, pres, _comp) = srcr_for_compression(original, compressed);
        assert!(pres < 0.5, "preservation should be low, got {}", pres);
        assert!(srcr < 0.5, "SRCR should be low for destructive compression, got {}", srcr);
    }

    #[test]
    fn test_extract_preservation_targets_errors() {
        let content = "running 5 tests\nFAILED test_foo\nerror: E0308 mismatched\n2 passed";
        let targets = extract_preservation_targets(content);
        assert!(targets.iter().any(|t| t.contains("FAILED")), "should extract FAILED");
        assert!(targets.iter().any(|t| t.contains("error") || t.contains("E0308")), "should extract error");
    }

    #[test]
    fn test_extract_preservation_targets_code() {
        let content = "pub fn validate_config(cfg: &Config) -> bool {\n    cfg.threshold > 0\n}";
        let targets = extract_preservation_targets(content);
        assert!(!targets.is_empty(), "should extract identifiers from code");
        assert!(targets.iter().any(|t| t.contains("validate_config")), "should find function name");
    }

    #[test]
    fn test_preservation_unique_duplicates() {
        // 5 identical error markers, 1 in compressed → unique strings (1) all preserved
        let targets = vec!["error[E0308]", "error[E0308]", "error[E0308]", "error[E0308]", "error[E0308]"];
        let text = "error[E0308]: something\n[4+ more]";
        let rate = preservation_hit_rate(text, &targets);
        assert!((rate - 1.0).abs() < 0.01, "unique target IS present, got {}", rate);
    }

    #[test]
    fn test_preservation_unique_partial() {
        // 3 unique targets, 1 in compressed → 0.33
        let targets = vec!["error[E0308]", "src/main.rs", "Traceback"];
        let text = "error[E0308] happened somewhere";
        let rate = preservation_hit_rate(text, &targets);
        assert!((rate - 0.33).abs() < 0.02, "1 of 3 unique, got {}", rate);
    }

    #[test]
    fn test_srcr_high_when_unique_preserved() {
        // Sift-style: 5 identical error lines collapsed to 1 + marker.
        // Unique target IS preserved → SRCR reflects actual savings.
        let original = "error[E0308]: type mismatch\nerror[E0308]: type mismatch\nerror[E0308]: type mismatch\nerror[E0308]: type mismatch\nerror[E0308]: type mismatch";
        let compressed = "error[E0308]: type mismatch\n[4+ more]";
        let (srcr, pres, comp) = srcr_for_compression(original, compressed);
        assert!((pres - 1.0).abs() < 0.01, "unique target preserved, got pres={}", pres);
        assert!(comp > 0.4, "should have meaningful compression, got {}", comp);
        assert!(srcr > 0.4, "SRCR should be high with unique preservation, got {}", srcr);
    }

    #[test]
    fn test_srcr_floor_blocks_destructive() {
        // Aggressive compression that drops the only error token
        let original = "error[E0308]: critical failure at src/lib.rs:100";
        let compressed = "[compressed content]";
        assert!(srcr_below_floor(original, compressed, 0.3), "destructive compression should fail floor");
    }

    #[test]
    fn test_srcr_floor_passes_safe() {
        // Safe compression preserving the error
        let original = "Compiling crate v0.1.0\nCompiling dep v1.0\nerror[E0308]: bad\nFinished";
        let compressed = "error[E0308]: bad\n[3 lines dropped]";
        assert!(!srcr_below_floor(original, compressed, 0.3), "safe compression should pass floor");
    }
}
