/// IR reasoning compression, conversation window, edit merge, and compression dictionary.
/// Ported from gate.js (context-engine) with project-specific dictionary support.

use ahash::AHashMap;
use regex::Regex;
use std::sync::OnceLock;

/// Pre-compiled regex patterns (compiled once, reused across all calls)
fn compress_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let raw = [
            r"(?i)\blet me (?:analyze|look|check|review|see|think|consider)\b[^.]*\.",
            r"(?i)\bi (?:would|will|cannot|can't)\s+(?:need to|say|see|check|try)\b[^.]*\.",
            r"(?i)\bin order to\b[^.]*\.",
            r"(?i)\bfirst,?\s+let me\b[^.]*\.",
            r"(?i)\bbased on (?:the|this|my|our)\b[^.]*\.?\s*",
            r"(?i)\bthis means that\b[^.]*\.",
            r"(?i)\bthe (?:next|final|first) step\b[^.]*\.",
            r"(?i)\bnow i (?:can|will|'ll|need to|should)\b[^.,;]*[,;.]?",
            r"(?i)\b(?:alright|okay|so,?|well,?|now,?)\s*",
            r"(?i)\b(?:essentially|basically|simply|actually|obviously|clearly|currently)\b",
        ];
        raw.iter().filter_map(|p| Regex::new(p).ok()).collect()
    })
}

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
        for phrase in phrases.iter() {
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
            t = t.replace(symbol.as_str(), replacement);
        }
        for (phrase, replacement) in &self.phrase_map {
            t = t.replace(phrase.as_str(), replacement);
        }
        t
    }
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
        || text.starts_with('{') || text.starts_with('[')
    { return None; }

    let mut t = text.to_string();

    if let Some(dict) = dict {
        t = dict.apply(&t);
    }

    // Strip specific verbose patterns via pre-compiled regex
    for re in compress_patterns() {
        t = re.replace_all(&t, "").to_string();
    }

    // Collapse whitespace
    t = t.split_whitespace().collect::<Vec<_>>().join(" ");

    if (t.len() as f64) < original_len as f64 * 0.6 { Some(t) } else { None }
}

/// Aggressive compression for bash tool results.
pub fn aggressive_compress(text: &str) -> Option<String> {
    let original_len = text.len();
    if original_len < 200 { return None; }
    if text.contains("```") || text.starts_with('{') || text.starts_with('[') { return None; }

    let mut t = text.to_string();
    for pattern_str in &["I can see", "Looking at", "Based on this", "as you can see",
        "As mentioned", "First of all", "Now I", "So I", "Let me", "I will",
        "I need to", "I should", "I'm going to", "essentially", "basically",
        "simply", "actually", "obviously", "clearly", "currently",
        "Alright", "Okay", "Well,", "Now,"] {
        t = t.replace(pattern_str, "");
    }
    t = t.split_whitespace().collect::<Vec<_>>().join(" ");

    let entities = extract_entities(text);
    let actions = extract_actions(text);
    if entities.is_empty() && actions.is_empty() { return None; }

    let compact = format_compact(&actions, &entities);
    if compact.len() >= original_len / 2 { return None; }
    Some(compact)
}

fn extract_entities(text: &str) -> Vec<String> {
    let fps: Vec<String> = text.split_whitespace()
        .filter(|w| w.contains('/') && w.contains('.'))
        .take(3)
        .map(|w| w.trim_end_matches(|c: char| c == '.' || c == ',' || c == ';').to_string())
        .collect();
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
            merged.push(EditCall {
                file,
                old_text: combined_old.join("\n"),
                new_text: combined_new.join("\n"),
                old_len: combined_old.iter().map(|s| s.len()).sum(),
                new_len: combined_new.iter().map(|s| s.len()).sum(),
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
        assert!(c.contains("bug") || c.contains("fix"), "should preserve key content");
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
