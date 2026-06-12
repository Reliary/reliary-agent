/// IR reasoning compression, conversation window, edit merge, and compression dictionary.
/// Ported from gate.js (context-engine) with project-specific dictionary support.

use ahash::AHashMap;
use std::sync::OnceLock;

/// Per-project compression dictionary: maps known identifiers to short references.
/// Build from FTS5 index at daemon startup.
#[derive(Debug, Clone, Default)]
pub struct CompressionDict {
    /// Known function/class names → replacement token
    pub known_symbols: AHashMap<String, String>,
    /// Common file paths → short reference
    pub known_paths: AHashMap<String, String>,
    /// Frequently co-occurring phrases → compressed form
    pub phrase_map: AHashMap<String, String>,
}

impl CompressionDict {
    /// Build from FTS5 phrase list
    pub fn from_phrases(phrases: &[String]) -> Self {
        let mut known_symbols = AHashMap::new();
        let mut phrase_map = AHashMap::new();
        for (i, phrase) in phrases.iter().enumerate() {
            if phrase.len() >= 3 && phrase.chars().all(|c| c.is_alphanumeric() || c == '_') {
                if phrase.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false)
                    || phrase.starts_with("fn_") || phrase.starts_with("def_")
                {
                    known_symbols.insert(phrase.clone(), format!("[sym:{}]", phrase.chars().take(3).collect::<String>()));
                }
            }
            if phrase.len() > 4 && phrase.len() < 20 {
                let key = phrase.chars().take(4).collect::<String>();
                phrase_map.insert(phrase.clone(), format!("[ph:{}]", key));
            }
        }
        Self { known_symbols, known_paths: AHashMap::new(), phrase_map }
    }

    /// Replace known symbols and phrases in text with compressed references
    pub fn apply(&self, text: &str) -> String {
        let mut t = text.to_string();
        for (symbol, replacement) in &self.known_symbols {
            if t.contains(symbol) {
                t = t.replace(symbol, replacement);
            }
        }
        for (phrase, replacement) in &self.phrase_map {
            if t.contains(phrase) {
                t = t.replace(phrase, replacement);
            }
        }
        t
    }
}

static FLUFF_PATTERNS: OnceLock<Vec<&'static str>> = OnceLock::new();
fn fluff_patterns() -> &'static [&'static str] {
    FLUFF_PATTERNS.get_or_init(|| vec![
        "I can see", "Looking at", "Based on this", "as you can see", "As mentioned", "First of all",
        "Now I", "So I", "Let me", "I will", "I need to", "I should", "I'm going to",
        "essentially", "basically", "simply", "actually", "obviously", "clearly", "currently",
        "Alright", "Okay", "Well,", "Now,",
    ])
}

/// Strip LLM reasoning fluff while preserving code context.
/// Ported from gate.js v2.2.0 inline compression. Grammar-free.
/// Returns compressed text if at least 40% smaller, else None.
/// If `dict` is provided, also compresses known symbols to short references.
pub fn compress_reasoning(text: &str, dict: Option<&CompressionDict>) -> Option<String> {
    let original_len = text.len();
    if original_len < 600 { return None; }

    if text.contains("```") || text.contains("//") || text.contains("/*")
        || text.contains("src/") || text.contains(".rs:") || text.contains(".py:")
        || text.contains("s/") || text.contains(".md")
    { return None; }

    let mut t = text.to_string();

    if let Some(dict) = dict {
        t = dict.apply(&t);
    }

    // Strip specific verbose patterns via regex (grammar-free) — gate.js v0.3.0 exact port
    let patterns = [
        r"(?i)\bLet me (analyze|look|check|review|see|think|consider)\b[^.]*\.",
        r"(?i)\bI (?:can|would|will) need to\b[^.]*\.",
        r"(?i)\bIn order to\b[^.]*\.",
        r"(?i)\bFirst,?\s*let me\b[^.]*\.",
        r"(?i)\bBased on (?:the|this|my|our)\b[^.]*\.?\s*",
        r"(?i)\bThis means that\b[^.]*\.",
        r"(?i)\bThe (?:next|final|first) step\b[^.]*\.",
        r"(?i)\bNow I (?:can|will|'ll|need to|should)\b[^.,;]*[,;.]?",
        r"(?i)\b(?:Alright|Okay|So,?|Well,?|Now,?)\s*",
        r"(?i)\b(?:essentially|basically|simply|actually|obviously|clearly|currently)\b",
    ];

    for pat in &patterns {
        if let Ok(re) = regex::Regex::new(pat) {
            t = re.replace_all(&t, "").to_string();
        }
    }

    // Collapse whitespace
    t = t.split_whitespace().collect::<Vec<_>>().join(" ");

    if (t.len() as f64) < original_len as f64 * 0.6 { Some(t) } else { None }
}

/// Gentle compression: strip basic fluff, preserve code context.
/// Returns None if savings < 5%.
pub fn gentle_compress(text: &str) -> Option<String> {
    let original_len = text.len();
    if original_len < 200 { return None; }
    if text.contains("```") || text.starts_with('{') || text.starts_with('[') { return None; }

    let mut t = text.to_string();
    // Strip fluff patterns
    for pattern in fluff_patterns() {
        t = t.replace(pattern, "");
    }
    // Strip common reasoning hedges
    let hedges = ["I think", "I believe", "It seems", "It appears", "We can", "we need to",
        "In order to", "Due to the fact", "The reason for", "As a result",
        "It is important", "It is worth noting", "It should be noted",
        "One thing to note", "Another thing", "In this case",
        "In this situation", "In the context", "With respect to"];
    for h in &hedges {
        t = t.replace(h, "");
    }
    // Collapse repeated whitespace
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");

    // Only return if we saved at least 5%
    if (t.len() as f64) < original_len as f64 * 0.95 {
        Some(t)
    } else {
        None
    }
}

/// Aggressive compression: strip fluff, extract actions/entities, return structured format.
/// For bash tool results where code context isn't needed.
pub fn aggressive_compress(text: &str) -> Option<String> {
    let original_len = text.len();
    if original_len < 200 { return None; }
    if text.contains("```") || text.starts_with('{') || text.starts_with('[') { return None; }

    let mut t = text.to_string();
    for pattern in fluff_patterns() {
        t = t.replace(pattern, "");
    }
    // Collapse repeated whitespace
    t = t.split_whitespace().collect::<Vec<_>>().join(" ");

    // Extract entities and actions
    let entities = extract_entities(text);
    let actions = extract_actions(text);
    if entities.is_empty() && actions.is_empty() { return None; }

    let compact = format_compact(&actions, &entities);
    if compact.len() >= original_len / 2 { return None; }
    Some(compact)
}

fn extract_entities(text: &str) -> Vec<String> {
    // File paths
    let fps: Vec<String> = text.split_whitespace()
        .filter(|w| w.contains('/') && w.contains('.'))
        .take(3)
        .map(|w| w.trim_end_matches(|c: char| c == '.' || c == ',' || c == ';').to_string())
        .collect();
    // PascalCase types
    let types: Vec<String> = text.split_whitespace()
        .filter(|w| w.len() >= 3 && w.starts_with(|c: char| c.is_ascii_uppercase()))
        .take(5)
        .map(|w| w.trim_end_matches(|c: char| !c.is_alphanumeric()).to_string())
        .collect();
    let mut all = fps;
    all.extend(types);
    all
}

fn extract_actions(text: &str) -> Vec<String> {
    let action_words = ["read", "write", "edit", "add", "modify", "change", "update", "remove",
        "delete", "create", "run", "check", "verify", "test", "build", "compile", "find", "search",
        "look", "examine", "implement", "refactor", "rename", "fix"];
    let mut actions = Vec::new();
    for word in text.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        if action_words.contains(&cleaned.as_str()) {
            actions.push(cleaned);
        }
    }
    actions
}

fn format_compact(actions: &[String], entities: &[String]) -> String {
    let mut parts = Vec::new();
    if !actions.is_empty() {
        parts.push(format!("[act] {}", actions.join(" ")));
    }
    if !entities.is_empty() {
        parts.push(format!("[ref] {}", entities.join(" ")));
    }
    parts.join(" | ")
}

/// Conversation window: keep N most recent turns, compress older ones
pub fn apply_conversation_window(turns: usize) -> (usize, usize) {
    // Returns (keep_count, compress_threshold)
    if turns <= 5 { (turns, 0) }
    else if turns <= 10 { (4, turns - 4) }
    else { (3, turns - 3) }
}

/// Merge sequential edits to the same file into a single edit
pub fn merge_same_file_edits(edits: &[EditCall]) -> Vec<EditCall> {
    let mut by_file: AHashMap<String, Vec<&EditCall>> = AHashMap::new();
    for e in edits {
        by_file.entry(e.file.clone()).or_default().push(e);
    }

    let mut merged = Vec::new();
    for (file, edits) in by_file {
        if edits.len() <= 1 {
            merged.push(edits[0].clone());
        } else {
            let combined_old: Vec<String> = edits.iter().map(|e| e.old_text.clone()).collect();
            let combined_new: Vec<String> = edits.iter().map(|e| e.new_text.clone()).collect();
            let total_old: usize = combined_old.iter().map(|s| s.len()).sum();
            let total_new: usize = combined_new.iter().map(|s| s.len()).sum();
            merged.push(EditCall {
                file,
                old_text: combined_old.join("\n"),
                new_text: combined_new.join("\n"),
                old_len: total_old,
                new_len: total_new,
                merged_count: edits.len(),
            });
        }
    }
    merged
}

#[derive(Debug, Clone)]
pub struct EditCall {
    pub file: String,
    pub old_text: String,
    pub new_text: String,
    pub old_len: usize,
    pub new_len: usize,
    pub merged_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_reasoning() {
        let text = "Let me look at this code carefully. I can see there's a bug in the validate_config function. I will fix it by changing line 5 of the parser. Now I need to also check the validator function for the same pattern. Alright this should work.";
        let r = compress_reasoning(text);
        assert!(r.is_some(), "compression should return Some for long text");
        let c = r.unwrap();
        assert!(c.len() < text.len(), "compressed text should be shorter");
        assert!(c.contains("[act]") || c.contains("[ref]"), "should contain action/ref markers");
    }

    #[test]
    fn test_short_text_skipped() {
        assert!(compress_reasoning("a b c").is_none());
    }

    #[test]
    fn test_edit_merge() {
        let edits = vec![
            EditCall { file: "a.rs".into(), old_text: "x".into(), new_text: "y".into(), old_len: 1, new_len: 1, merged_count: 1 },
            EditCall { file: "a.rs".into(), old_text: "z".into(), new_text: "w".into(), old_len: 1, new_len: 1, merged_count: 1 },
        ];
        let merged = merge_same_file_edits(&edits);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].merged_count, 2);
    }

    #[test]
    fn test_conversation_window() {
        let (keep, _) = apply_conversation_window(3);
        assert_eq!(keep, 3);
        let (keep, _) = apply_conversation_window(8);
        assert_eq!(keep, 4);
    }
}
