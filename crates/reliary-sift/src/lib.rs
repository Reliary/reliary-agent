/// Structural output compression + Maxwell information-theoretic gate.
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
        let mut freq = ahash::AHashMap::new();
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
        let unique: std::collections::HashSet<&str, ahash::RandomState> = words.iter().cloned().collect();
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

/// Grammar-free content line classification and transformation.
/// Works on any text file without knowing the language/tool.
#[derive(Debug, Clone, PartialEq)]
pub enum LineType {
    Blank,
    Separator,
    Comment,
    Import,
    Definition,
    Error,
    Summary,
    Code,
}

#[derive(Debug, Clone)]
pub struct ContentLine {
    pub text: String,
    pub line_type: LineType,
    pub index: usize,
}

/// Classify a single line using byte-DFA + indentation (grammar-free).
/// No keyword lists, no language detection.
pub fn classify_line(line: &str) -> LineType {
    let trimmed = line.trim();
    if trimmed.is_empty() { return LineType::Blank; }

    // Separator: 3+ chars from structural punctuation set
    let seps: &[char] = &['-', '=', '*', '.', '_', '~'];
    if trimmed.len() >= 3 && trimmed.chars().all(|c| seps.contains(&c)) {
        return LineType::Separator;
    }

    let lower = trimmed.to_lowercase();

    // Error: contains error/FAILED substring (case-insensitive)
    if lower.contains("error") || trimmed.starts_with("FAILED") {
        return LineType::Error;
    }

    // Summary: test results or build completion
    if (lower.contains("passed") && lower.contains("failed"))
        || lower.starts_with("test result:")
        || lower.starts_with("finished")
        || trimmed.contains("  --> ")
    {
        return LineType::Summary;
    }

    // Comment: first non-whitespace char is comment punctuation
    let first = trimmed.chars().next().unwrap_or(' ');
    if first == '#' || first == ';' || first == '%' || first == '\''
        || trimmed.starts_with("//") || trimmed.starts_with("--")
        || trimmed.starts_with("/*") || trimmed.starts_with("* ")
    {
        return LineType::Comment;
    }

    // Import: contains import-like keywords as substring (not AST, just string match)
    if lower.starts_with("import ") || lower.starts_with("from ")
        || lower.starts_with("use ") || lower.starts_with("include")
        || lower.starts_with("require(") || lower.starts_with("pub use ")
        || lower.starts_with("extern crate")
    {
        return LineType::Import;
    }

    // Definition vs Code: byte DFA — structural character ratio
    if is_definition_line(trimmed) {
        return LineType::Definition;
    }

    LineType::Code
}

/// Detect definition lines using byte DFA + indentation (grammar-free).
/// No keyword prefix matching — uses structural character density.
fn is_definition_line(line: &str) -> bool {
    let trimmed = line.trim();

    // Indentation heuristic: line ends with block-opening punctuation
    if trimmed.ends_with('{') || trimmed.ends_with("=>") || trimmed.ends_with("->")
        || trimmed.ends_with(':')
    {
        // Must have content before the punctuation (not just `{`)
        let before = trimmed.trim_end_matches(['{', '=', '>', '-', ':']).trim();
        if !before.is_empty() && before.chars().any(|c| c.is_ascii_alphanumeric()) {
            return true;
        }
    }

    // Byte DFA: count structural characters
    let structural: &[char] = &['{', '}', '(', ')', ';', '=', '<', '>'];
    let struct_count = trimmed.chars().filter(|c| structural.contains(c)).count();
    let alpha_count = trimmed.chars().filter(|c| c.is_ascii_alphabetic()).count();

    // High structural density + has parens = function-like definition
    if struct_count >= 2 && trimmed.contains('(') && trimmed.contains(')') {
        let struct_ratio = struct_count as f64 / trimmed.len().max(1) as f64;
        if struct_ratio > 0.05 && alpha_count >= 3 {
            return true;
        }
    }

    // Assignment: contains ` = ` (not `==`) with identifier before it
    if line.contains(" = ") && !line.contains("==") {
        let before = line.split('=').next().unwrap_or("").trim();
        if !before.is_empty() && before.chars().any(|c| c.is_ascii_alphanumeric()) {
            return true;
        }
    }

    false
}

/// Detect if text looks like source content vs command output (grammar-free).
/// Uses structural character ratio + blank line ratio.
pub fn looks_like_content(lines: &[ContentLine]) -> bool {
    if lines.len() < 20 { return false; }
    let non_blank: Vec<&ContentLine> = lines.iter().filter(|l| l.line_type != LineType::Blank).collect();
    if non_blank.len() < 10 { return false; }

    // Warning lines indicate command output, not source
    let warnings = lines.iter()
        .filter(|l| l.text.contains("warning:") || l.text.starts_with("  --> "))
        .count();
    if warnings >= 3 { return false; }

    // Structural character ratio across all non-blank lines
    let structural: &[char] = &['{', '}', '(', ')', ';', '=', '<', '>'];
    let total_chars: usize = non_blank.iter().map(|l| l.text.len()).sum();
    let struct_chars: usize = non_blank.iter()
        .map(|l| l.text.chars().filter(|c| structural.contains(c)).count())
        .sum();
    let struct_ratio = struct_chars as f64 / total_chars.max(1) as f64;

    // Source code has high structural density; command output has low
    if struct_ratio < 0.02 { return false; }

    // Definition ratio: source has many, command output has few
    let defs = non_blank.iter().filter(|l| l.line_type == LineType::Definition).count();
    let imports = non_blank.iter().filter(|l| l.line_type == LineType::Import).count();
    (defs + imports) > non_blank.len() / 10
}

/// Classify all lines in a content block.
pub fn classify_content(text: &str) -> Vec<ContentLine> {
    text.lines().enumerate().map(|(i, l)| ContentLine {
        text: l.to_string(),
        line_type: classify_line(l),
        index: i,
    }).collect()
}

/// Transform content for compact agent display.
/// Collapses blanks, strips comments, collapses imports, keeps definitions.
pub fn compress_content(lines: Vec<ContentLine>, aggressive: bool) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut blank_count = 0;
    let mut import_count = 0;
    let mut comment_run = false;
    let mut first_comment = true;

    for line in &lines {
        match line.line_type {
            LineType::Blank => {
                blank_count += 1;
                if result.is_empty() { continue; }
            }
            LineType::Separator => {
                if !result.last().is_some_and(|l| l.starts_with("---") || l.starts_with("===")) {
                    result.push("---".to_string());
                }
                blank_count = 0;
            }
            LineType::Comment => {
                if first_comment || !aggressive {
                    result.push(line.text.clone());
                    first_comment = false;
                }
                comment_run = true;
                blank_count = 0;
            }
            LineType::Import => {
                import_count += 1;
                blank_count = 0;
                first_comment = false;
            }
            LineType::Definition | LineType::Code | LineType::Summary | LineType::Error => {
                if import_count > 0 {
                    let label = if import_count == 1 { "import" } else { "imports" };
                    result.push(format!("[{} {}]", import_count, label));
                    import_count = 0;
                }
                if blank_count > 0 && !result.is_empty() {
                    result.push(String::new());
                    blank_count = 0;
                }
                if comment_run && aggressive { comment_run = false; }
                first_comment = false;
                result.push(line.text.clone());
            }
        }
    }

    if !result.is_empty() && result.last().is_none_or(|l| l.is_empty()) {
        result.pop();
    }
    result
}

/// Truncate long lines to max_len chars, appending "... (N chars)" when truncated.
pub fn truncate_lines(lines: Vec<String>, max_len: usize) -> Vec<String> {
    lines.into_iter().map(|l| {
        if l.len() > max_len {
            let trunc_len = max_len.saturating_sub(20);
            let safe_end = l.char_indices()
                .take_while(|(idx, _)| *idx < trunc_len)
                .last()
                .map(|(idx, c)| idx + c.len_utf8())
                .unwrap_or(0);
            format!("{}... ({} chars)", &l[..safe_end], l.len())
        } else { l }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_blank_empty() { assert_eq!(classify_line(""), LineType::Blank); }
    #[test]
    fn line_blank_whitespace() { assert_eq!(classify_line("   "), LineType::Blank); }
    #[test]
    fn line_separator() { assert_eq!(classify_line("---"), LineType::Separator); }
    #[test]
    fn line_separator_equals() { assert_eq!(classify_line("======"), LineType::Separator); }
    #[test]
    fn line_comment_hash() { assert_eq!(classify_line("# comment"), LineType::Comment); }
    #[test]
    fn line_comment_slash() { assert_eq!(classify_line("// comment"), LineType::Comment); }
    #[test]
    fn line_import() { assert_eq!(classify_line("import os"), LineType::Import); }
    #[test]
    fn line_definition_fn() { assert_eq!(classify_line("fn hello()"), LineType::Definition); }
    #[test]
    fn line_definition_braces() {
        assert_eq!(classify_line("if x > 0 {"), LineType::Definition);
        assert_eq!(classify_line("x = 5"), LineType::Definition);
    }
    #[test]
    fn line_error() { assert_eq!(classify_line("Error: not found"), LineType::Error); }
    #[test]
    fn line_summary() { assert_eq!(classify_line("test result: ok. 42 passed"), LineType::Summary); }
    #[test]
    fn compress_imports_collapsed() {
        let lines = classify_content("import a\nimport b\nimport c\nfn main() {}");
        let r = compress_content(lines, false);
        assert!(r.iter().any(|l| l.starts_with("[3 imports]")));
    }
    #[test]
    fn test_is_definition_line() {
        assert!(is_definition_line("fn hello()"));
        assert!(is_definition_line("x = y")); // assignment IS a definition
    }
    #[test]
    fn test_looks_like_content() {
        let text = "fn a() {}\nfn b() {}\nfn c() {}\n".repeat(8);
        let lines = classify_content(&text);
        assert!(looks_like_content(&lines));
    }
    #[test]
    fn test_compress_content_ratio() {
        let text = "use std::collections::HashMap;\nuse std::sync::Arc;\nuse std::time::Duration;\n\npub fn process_data(data: &str) -> Result<(), Error> {\n    let items = parse_items(data)?;\n    for item in items {\n        validate_item(&item);\n    }\n    Ok(())\n}\n\nfn validate_item(item: &Item) -> bool {\n    if item.is_valid() { item.process() }\n    true\n}";
        let lines = classify_content(text);
        let original_len: usize = lines.iter().map(|l| l.text.len() + 1).sum();
        let result = compress_content(lines, true);
        let compressed_len: usize = result.iter().map(|l| l.len() + 1).sum();
        assert!(compressed_len < original_len, "compress_content should reduce size ({} vs {})", compressed_len, original_len);
    }
    #[test]
    fn test_compress_content_non_aggressive_preserves_comments() {
        let text = "# this is a comment\n# another comment\nfn main() {}";
        let lines = classify_content(text);
        let r = compress_content(lines, false);
        assert!(r.iter().any(|l| l.contains("this is a comment")), "non-aggressive mode should preserve first comment");
    }
    #[test]
    fn test_truncate_long() {
        let r = truncate_lines(vec!["a".repeat(200)], 50);
        assert!(r[0].ends_with("chars)"));
    }
}
