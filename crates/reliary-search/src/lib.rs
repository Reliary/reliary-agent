/// Grammar-free phrase search with BM25 scoring and Porter stemming.
/// Ported from stria (github.com/Reliary/stria).

pub mod schema;
pub mod search;
pub mod ingest;

/// BM25 IDF: ((N - df + 0.5) / (df + 0.5) + 1.0).ln()
pub fn bm25_idf(n_docs: f64, df: f64) -> f64 {
    ((n_docs - df + 0.5) / (df + 0.5) + 1.0).ln()
}

/// BM25 score with K1=1.2, b=0.75, log TF scaling
pub fn bm25_score(idf: f64, tf: f64, doc_len: f64, avgdl: f64) -> f64 {
    let k1 = 1.2;
    let b = 0.75;
    let log_tf = (1.0 + tf).ln();
    idf * (log_tf * (k1 + 1.0)) / (log_tf + k1 * (1.0 - b + b * doc_len / avgdl))
}

/// Grammar-free identifier scanning: extract [A-Za-z_][A-Za-z0-9_]{3,40}
pub fn scan_identifiers(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| {
            let len = t.len();
            (3..=40).contains(&len)
                && t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .map(|t| t.to_lowercase())
        .collect()
}

/// Porter-like stemming: reduce common suffixes
pub fn porter_stem(word: &str) -> String {
    let w = word.trim().to_lowercase();
    if w.len() <= 3 { return w; }
    // Remove common suffixes (simplified Porter)
    let suffixes = ["ing", "tion", "sion", "ment", "ness", "able", "ible", "ful", "less", "ly", "ed", "es", "er", "or", "al", "ic", "en", "ive", "ize", "ise"];
    for s in &suffixes {
        if w.ends_with(s) && w.len() > s.len() + 2 {
            return w[..w.len() - s.len()].to_string();
        }
    }
    if w.ends_with('s') && !w.ends_with("ss") && w.len() > 3 {
        return w[..w.len() - 1].to_string();
    }
    w
}

/// Trigram decomposition for OOV tokens
pub fn trigrams(token: &str) -> Vec<String> {
    let t = token.to_lowercase();
    if t.len() <= 3 { return vec![t]; }
    (0..t.len() - 2).map(|i| t[i..i + 3].to_string()).collect()
}

/// Tokenize a phrase into stemmed identifiers
pub fn tokenize(text: &str) -> Vec<String> {
    scan_identifiers(text).into_iter().map(|t| porter_stem(&t)).collect()
}

/// Grammar-free definition detection: is `phrase` at `match_start` in `line` a definition
/// or a call site? Uses byte-scanning for preceding keywords and following structural markers.
pub fn is_definition(phrase: &str, line: &str, match_start: usize) -> bool {
    let bytes = line.as_bytes();
    let end = match_start + phrase.len();
    if end >= bytes.len() { return false; }

    // Word boundary before the phrase
    if match_start > 0 {
        let prev = bytes[match_start - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'.' { return false; }

        // Check for non-definition keywords before the phrase (new, import)
        let mut word_start = match_start;
        while word_start > 0 {
            let w = bytes[word_start - 1];
            if w.is_ascii_alphanumeric() || w == b'_' { break; }
            word_start -= 1;
        }
        let mut word_begin = word_start;
        while word_begin > 0 {
            let w = bytes[word_begin - 1];
            if !w.is_ascii_alphanumeric() && w != b'_' { break; }
            word_begin -= 1;
        }
        let preceding_word = &bytes[word_begin..word_start];
        if matches!(preceding_word, b"new" | b"import") { return false; }
    }

    // Scan forward for structural definition marker: (, <, [, =, :, {, ->
    let mut pos = end;
    while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b'\t') { pos += 1; }
    while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') { pos += 1; }
    while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b'\t') { pos += 1; }
    if pos < bytes.len() {
        let c = bytes[pos];
        if matches!(c, b'(' | b'<' | b'[' | b'=' | b':' | b'{') { return true; }
        if c == b'-' && pos + 1 < bytes.len() && bytes[pos + 1] == b'>' { return true; }
    }
    false
}

/// Convenience wrapper: is `phrase` a definition in `line`?
pub fn is_definition_str(phrase: &str, line: &str) -> bool {
    line.find(phrase).is_some_and(|idx| is_definition(phrase, line, idx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bm25_idf() {
        let idf = bm25_idf(100.0, 100.0);
        assert!(idf > 0.0);
    }

    #[test]
    fn test_bm25_score() {
        let s = bm25_score(2.0, 1.0, 50.0, 100.0);
        assert!(s > 0.0 && s < 5.0);
    }

    #[test]
    fn test_scan_phrases() {
        let ids = scan_identifiers("validate_config returns True");
        assert!(ids.contains(&"validate_config".to_string()));
    }

    #[test]
    fn test_porter_stem() {
        assert_eq!(porter_stem("running"), "runn");
    }

    #[test]
    fn test_trigrams() {
        let t = trigrams("validate");
        assert_eq!(t.len(), 6);
        assert!(t.contains(&"val".to_string()));
    }
}
