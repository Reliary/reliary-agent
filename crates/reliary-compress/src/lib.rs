use ahash::AHashMap;

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
    let mut pairs = Vec::new();
    let lines: Vec<&str> = text.lines().take(15).collect();
    let sig_re = regex_lite::Regex::new(r"^\s*(?:pub\s+)?(?:fn|def|class|fun|function)\s+(\w+)").unwrap();
    let mut last_match = String::new();
    for line in &lines {
        let trimmed = line.trim();
        if let Some(caps) = sig_re.captures(trimmed) {
            last_match = caps[1].to_string();
        } else if !last_match.is_empty() && trimmed.len() > 5 && !trimmed.starts_with("use ") {
            pairs.push((last_match.clone(), trimmed.chars().take(40).collect()));
            last_match.clear();
        }
    }
    pairs
}

fn phash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Build a compression dictionary from a list of symbols.
pub fn build_dict(symbols: &[String]) -> CompressionDict {
    let mut seen: AHashMap<u64, u32> = AHashMap::new();
    for s in symbols {
        let h = phash(s);
        *seen.entry(h).or_insert(0) += 1;
    }
    let mut entries: Vec<DictEntry> = seen.into_iter()
        .filter(|(_, freq)| *freq > 1)
        .map(|(h, freq)| DictEntry {
            symbol: symbols.iter().find(|s| phash(s) == h).unwrap_or(&String::new()).clone(),
            frequency: freq,
        })
        .collect();
    entries.sort_by(|a, b| b.frequency.cmp(&a.frequency));
    CompressionDict { entries }
}

impl CompressionDict {
    /// Apply dictionary: replace known long phrases with compact references.
    pub fn apply(&self, text: &str) -> String {
        let mut result = text.to_string();
        for entry in &self.entries {
            if result.len() < 1000
                && result.contains(&entry.symbol) {
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
    if original_len < 300 { return None; }
    if text.contains("```") || text.contains("//") || text.contains("/*")
        || text.contains("src/") || text.contains(".rs:") || text.contains(".py:")
        || text.contains("s/") || text.contains(".md")
    { return None; }

    let mut t = text.to_string();
    if let Some(d) = dict { t = d.apply(&t); }

    for pattern in &[
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
    ] {
        if let Ok(re) = regex::Regex::new(pattern) {
            t = re.replace_all(&t, " ").to_string();
        }
    }
    t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if (t.len() as f64) < original_len as f64 * 0.6 { Some(t) } else { None }
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
        assert!(dict.entries.len() >= 1 && dict.entries.len() <= 2);
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
}
