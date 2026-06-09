/// Structural output compression + Maxwell information-theoretic gate.
/// Ported from sift CLI (stdin/stdout structural compression) and maxwell.

/// Zone truncation: keep first N lines, omit middle, keep last M
pub fn zone_truncate(text: &str, head: usize, tail: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= head + tail { return text.to_string(); }

    let omitted = lines.len() - head - tail;
    let omitted_msg = format!("[... {} lines omitted ...]", omitted);
    let mut result: Vec<&str> = lines.iter().take(head).cloned().collect();
    result.push(&omitted_msg);
    result.extend(lines.iter().rev().take(tail).rev().cloned());
    result.join("\n")
}

/// Collapse repeated blank lines to single blank
pub fn collapse_blanks(text: &str) -> String {
    let mut result = String::new();
    let mut prev_blank = false;
    for line in text.lines() {
        let blank = line.trim().is_empty();
        if blank && prev_blank { continue; }
        result.push_str(line);
        result.push('\n');
        prev_blank = blank;
    }
    result
}

/// Strip trailing whitespace from each line
pub fn strip_trailing(text: &str) -> String {
    text.lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n")
}

/// Maxwell triple-metric filter: entropy, compression ratio, lexical diversity
pub struct MaxwellGate {
    pub entropy_threshold: f64,      // < threshold = too narrow
    pub compression_ratio_max: f64,  // > max = too repetitive
    pub diversity_min: f64,          // < min = too padded
}

impl Default for MaxwellGate {
    fn default() -> Self {
        Self { entropy_threshold: 3.5, compression_ratio_max: 3.0, diversity_min: 0.25 }
    }
}

impl MaxwellGate {
    /// Shannon entropy of text (bits per character)
    fn entropy(&self, text: &str) -> f64 {
        if text.is_empty() { return 0.0; }
        let len = text.len() as f64;
        let mut freq = std::collections::HashMap::new();
        for b in text.bytes() { *freq.entry(b).or_insert(0) += 1; }
        -freq.values().map(|&c| {
            let p = c as f64 / len;
            p * p.log2()
        }).sum::<f64>()
    }

    /// Zlib compression ratio as boilerplate detector
    fn compression_ratio(&self, text: &str) -> f64 {
        use std::io::Write;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        if encoder.write_all(text.as_bytes()).is_err() { return 0.0; }
        let compressed = match encoder.finish() {
            Ok(c) => c,
            Err(_) => return 1.0,
        };
        if compressed.is_empty() { return 1.0; }
        text.len() as f64 / compressed.len() as f64
    }

    /// Lexical diversity: unique words / total words
    fn lexical_diversity(&self, text: &str) -> f64 {
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.is_empty() { return 0.0; }
        let unique: std::collections::HashSet<&str> = words.iter().cloned().collect();
        unique.len() as f64 / words.len() as f64
    }

    /// Score a text passage. Returns None if it fails any gate.
    pub fn score(&self, text: &str) -> Option<(f64, usize)> {
        if text.len() < 50 { return Some((1.0, text.len())); }
        let ent = self.entropy(text);
        let ratio = self.compression_ratio(text);
        let div = self.lexical_diversity(text);

        if ent < self.entropy_threshold { return None; }
        if ratio > self.compression_ratio_max { return None; }
        if div < self.diversity_min { return None; }
        Some((ent.min(8.0) / 8.0, text.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zone_truncate() {
        let text = (0..20).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let compressed = zone_truncate(&text, 3, 2);
        let lines: Vec<&str> = compressed.lines().collect();
        assert_eq!(lines.len(), 6); // 3 head + 1 omitted + 2 tail
        assert!(lines[0].contains("line 0"));
    }

    #[test]
    fn test_collapse_blanks() {
        let text = "a\n\n\nb\n\nc";
        let c = collapse_blanks(text);
        assert_eq!(c, "a\n\nb\n\nc\n");
    }

    #[test]
    fn test_maxwell_gate() {
        let gate = MaxwellGate::default();
        let high_entropy = "This is a normal sentence with varied vocabulary and structure.";
        let result = gate.score(high_entropy);
        assert!(result.is_some());
    }

    #[cfg(feature = "flate2")]
    #[test]
    fn test_compression_ratio() {
        let gate = MaxwellGate::default();
        let repetitive = "aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa ";
        let result = gate.score(repetitive);
        assert!(result.is_none());
    }
}
