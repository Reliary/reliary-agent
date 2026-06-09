/// Grammar-free pattern extraction, content-aware matching, fixing files.
/// Ported from cortex-rs fix.rs and relay edit.rs.

use std::collections::HashMap;

/// A resolved fix: old text → new text, applied to a specific file
#[derive(Debug, Clone)]
pub struct Fix {
    pub old: String,
    pub new: String,
    pub file: String,
    pub line: usize,
}

/// Pattern extraction from memory content supporting multiple formats.
pub fn extract_fixes(memory_content: &str) -> Vec<(String, String)> {
    let mut fixes = Vec::new();

    if let Ok(re) = regex_lite::Regex::new(r"(?m)'([^']+)'\s*(?:→|->|=>)\s*'([^']+)'") {
        for cap in re.captures_iter(memory_content) {
            fixes.push((cap[1].to_string(), cap[2].to_string()));
        }
    }

    if let Ok(re) = regex_lite::Regex::new(r"(?m)`([^`]+)`\s*(?:→|->|=>)\s*`([^`]+)`") {
        for cap in re.captures_iter(memory_content) {
            fixes.push((cap[1].to_string(), cap[2].to_string()));
        }
    }

    if let Ok(re) = regex_lite::Regex::new(r"(?i)change\s+'([^']+)'\s+to\s+'([^']+)'") {
        for cap in re.captures_iter(memory_content) {
            fixes.push((cap[1].to_string(), cap[2].to_string()));
        }
    }

    if let Ok(re) = regex_lite::Regex::new(r"(?i)(?:replace|swap|switch)\s+'([^']+)'\s+(?:with|to|for)\s+'([^']+)'") {
        for cap in re.captures_iter(memory_content) {
            fixes.push((cap[1].to_string(), cap[2].to_string()));
        }
    }

    if let Ok(re) = regex_lite::Regex::new(r"s/([^/]+)/([^/]*)/") {
        for cap in re.captures_iter(memory_content) {
            let old_str = cap[1].to_string();
            let new_str = cap[2].to_string();
            if old_str.len() >= 1 && !new_str.is_empty() && old_str != new_str {
                fixes.push((old_str, new_str));
            }
        }
    }

    if let Ok(re) = regex_lite::Regex::new(r"(?m)(\S+(?:\s+\S+){0,5})\s*(?:→|->|=>)\s*(\S+(?:\s+\S+){0,5})") {
        for cap in re.captures_iter(memory_content) {
            let old_str = cap[1].trim().to_string();
            let new_str = cap[2].trim().to_string();
            if old_str.len() >= 2 && new_str.len() >= 2 && old_str != new_str {
                fixes.push((old_str, new_str));
            }
        }
    }

    fixes
}

/// Forgiving signature matching: find a function by fuzzy signature.
pub fn find_function<'a>(lines: &[&'a str], signature: &str) -> Option<(usize, &'a str)> {
    let trimmed = signature.trim();

    for (i, l) in lines.iter().enumerate() {
        if l.trim() == trimmed { return Some((i, l)); }
    }

    for (i, l) in lines.iter().enumerate() {
        if l.trim().starts_with(trimmed) { return Some((i, l)); }
    }

    let func_name = extract_func_name(trimmed);
    if func_name.len() >= 3 {
        let keywords = ["fn ", "def ", "function ", "pub ", "struct ", "class ", "trait ", "impl ", "enum "];
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim();
            if t.contains(&func_name) && keywords.iter().any(|k| t.contains(k)) {
                return Some((i, l));
            }
        }
        for (i, l) in lines.iter().enumerate() {
            if l.contains(&func_name) { return Some((i, l)); }
        }
    }

    None
}

fn extract_func_name(sig: &str) -> String {
    let skip = ["def", "fn", "function", "pub", "private", "protected", "static", "async",
        "export", "default", "const", "let", "var", "class", "struct", "enum", "trait",
        "impl", "interface", "type"];
    sig.split(['(', ' ', '{'])
        .find(|s| {
            let t = s.trim();
            !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_')
                && !skip.contains(&t)
        })
        .unwrap_or("")
        .to_string()
}

/// Grammar-free boundary detection: find end of function by indentation
pub fn find_boundary(lines: &[&str], start: usize, base_indent: usize) -> usize {
    if start >= lines.len() { return lines.len().saturating_sub(1); }
    for i in start..lines.len() {
        let line = lines[i];
        if line.trim().is_empty() || line.trim().starts_with('#') || line.trim().starts_with("//") {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent <= base_indent {
            let mut j = i;
            while j > start && (lines[j - 1].trim().is_empty()
                || lines[j - 1].trim().starts_with('#')
                || lines[j - 1].trim().starts_with("//"))
            { j -= 1; }
            if j > start { return j - 1; }
            return i.saturating_sub(1);
        }
    }
    lines.len().saturating_sub(1)
}

/// Apply fixes to file content (in-memory)
pub fn apply_fixes(content: &str, fixes: &[(String, String)]) -> (String, usize) {
    let mut modified = content.to_string();
    let mut total = 0;
    for (old, new) in fixes {
        if old == new { continue; }
        let count = modified.matches(old).count();
        if count > 0 {
            modified = modified.replace(old, new);
            total += count;
        } else {
            let unquoted = old.trim_matches(|c| c == '\'' || c == '"' || c == '`');
            if unquoted.len() < old.len() {
                let count2 = modified.matches(unquoted).count();
                if count2 > 0 {
                    modified = modified.replace(unquoted, new);
                    total += count2;
                }
            }
        }
    }
    (modified, total)
}

/// Content-aware matching: find old/new pairs where old exists in content
pub fn content_aware_match(memory_content: &str, file_content: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    if let Ok(re) = regex_lite::Regex::new(r"['`]?(\S{2,})['`]?\s*(?:→|->|=>)\s*['`]?(\S{2,})['`]?") {
        for cap in re.captures_iter(memory_content) {
            let old_str = cap[1].trim_matches(|c| c == '\'' || c == '"' || c == '`').to_string();
            let new_str = cap[2].trim_matches(|c| c == '\'' || c == '"' || c == '`').to_string();
            if file_content.contains(&old_str) && old_str.len() >= 2 {
                results.push((old_str, new_str));
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_quoted_fix() {
        let fixes = extract_fixes("'if x < 2' → 'if x == 2'");
        assert!(!fixes.is_empty());
        assert_eq!(fixes[0].0, "if x < 2");
    }

    #[test]
    fn test_extract_change_pattern() {
        let fixes = extract_fixes("change 'old_val' to 'new_val'");
        assert!(!fixes.is_empty());
        assert_eq!(fixes[0].0, "old_val");
    }

    #[test]
    fn test_find_function_exact() {
        let lines = vec!["pub fn foo() {}", "fn bar(x: i32) {}", "fn baz() {}"];
        let (idx, _) = find_function(&lines, "fn bar(x: i32)").unwrap();
        assert_eq!(idx, 1);
    }

    #[test]
    fn test_find_function_fuzzy() {
        let lines = vec!["pub fn process_data(config: Config) -> Result", "fn helper() {}"];
        let (idx, _) = find_function(&lines, "process_data").unwrap();
        assert_eq!(idx, 0);
    }

    #[test]
    fn test_find_boundary() {
        let lines = vec!["fn foo() {", "    let x = 1;", "}", "fn bar() {}"];
        // start=1, base_indent=0
        // i=1: indent 4 > 0, continue
        // i=2: indent 0 <= 0, j=2, lines[1] not blank/comment, j>start → return j-1=1
        let end = find_boundary(&lines, 1, 0);
        assert_eq!(end, 1); // last content line before next function
    }

    #[test]
    fn test_apply_replace() {
        let fixes = vec![("old_thing".to_string(), "new_thing".to_string())];
        let (result, count) = apply_fixes("use old_thing;", &fixes);
        assert_eq!(count, 1);
        assert!(!result.contains("old_thing"));
        assert!(result.contains("new_thing"));
    }

    #[test]
    fn test_content_aware_match() {
        let mem = "'func_a' → 'func_b'";
        let content = "use func_a;";
        let matches = content_aware_match(mem, content);
        assert!(!matches.is_empty(), "should find match for func_a");
        assert_eq!(matches[0].0, "func_a");
    }
}
