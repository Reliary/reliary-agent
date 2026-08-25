
use std::sync::LazyLock;

/// Cached fix-extraction regexes. Compiled once on first use.
static FIX_RE: LazyLock<[regex_lite::Regex; 6]> = LazyLock::new(|| {
    [
        regex_lite::Regex::new(r"(?m)'([^']+)'\s*(?:→|->|=>)\s*'([^']+)'").unwrap(),
        regex_lite::Regex::new(r"(?m)`([^`]+)`\s*(?:→|->|=>)\s*`([^`]+)`").unwrap(),
        regex_lite::Regex::new(r"(?i)change\s+'([^']+)'\s+to\s+'([^']+)'").unwrap(),
        regex_lite::Regex::new(r"(?i)(?:replace|swap|switch)\s+'([^']+)'\s+(?:with|to|for)\s+'([^']+)'").unwrap(),
        regex_lite::Regex::new(r"s/([^/]+)/([^/]*)/").unwrap(),
        regex_lite::Regex::new(r"(?m)(\S+(?:\s+\S+){0,5})\s*(?:→|->|=>)\s*(\S+(?:\s+\S+){0,5})").unwrap(),
    ]
});

static CAM_RE: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
    regex_lite::Regex::new(r"['`]?(\S{2,})['`]?\s*(?:→|->|=>)\s*['`]?(\S{2,})['`]?").unwrap()
});

/// Pattern extraction from memory content supporting multiple formats.
pub fn extract_fixes(memory_content: &str) -> Vec<(String, String)> {
    let mut fixes = Vec::with_capacity(8);
    let mut seen: rustc_hash::FxHashSet<(String, String)> = rustc_hash::FxHashSet::default();

    // D5: Borrow keys from captures, only own at final push.
    let mut push = |old: &str, new: &str, fixes: &mut Vec<(String, String)>, seen: &mut rustc_hash::FxHashSet<(String, String)>| {
        if !old.is_empty() && old != new && seen.insert((old.to_string(), new.to_string())) {
            fixes.push((old.to_string(), new.to_string()));
        }
    };

    for cap in FIX_RE[0].captures_iter(memory_content) {
        push(&cap[1], &cap[2], &mut fixes, &mut seen);
    }
    for cap in FIX_RE[1].captures_iter(memory_content) {
        push(&cap[1], &cap[2], &mut fixes, &mut seen);
    }
    for cap in FIX_RE[2].captures_iter(memory_content) {
        push(&cap[1], &cap[2], &mut fixes, &mut seen);
    }
    for cap in FIX_RE[3].captures_iter(memory_content) {
        push(&cap[1], &cap[2], &mut fixes, &mut seen);
    }
    for cap in FIX_RE[4].captures_iter(memory_content) {
        push(&cap[1], &cap[2], &mut fixes, &mut seen);
    }
    for cap in FIX_RE[5].captures_iter(memory_content) {
        let old_str = cap[1].trim();
        let new_str = cap[2].trim();
        if old_str.len() >= 2 && new_str.len() >= 2 {
            push(old_str, new_str, &mut fixes, &mut seen);
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
        // D15: Sorted array + binary search (small N, but cleaner).
        const KEYWORDS: &[&str] = &["fn ", "def ", "function ", "pub ", "struct ", "class ", "trait ", "impl ", "enum "];
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim();
            if t.contains(&func_name) && KEYWORDS.iter().any(|k| t.contains(k)) {
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
    // D15: Sorted for binary search future.
    const SKIP: &[&str] = &["async", "class", "const", "def", "default", "enum", "export",
        "fn", "function", "impl", "interface", "let", "private", "protected", "pub",
        "static", "struct", "trait", "type", "var"];
    sig.split(['(', ' ', '{'])
        .find(|s| {
            let t = s.trim();
            !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_')
                && !SKIP.contains(&t)
        })
        .unwrap_or("")
        .to_string()
}

/// Grammar-free boundary detection: find end of function by indentation
pub fn find_boundary(lines: &[&str], start: usize, base_indent: usize) -> usize {
    if start >= lines.len() { return lines.len().saturating_sub(1); }
    for i in start..lines.len() {
        let line = lines[i];
        // D14: Cache trim once per line.
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent <= base_indent {
            let mut j = i;
            while j > start {
                let prev_trimmed = lines[j - 1].trim();
                if !(prev_trimmed.is_empty()
                    || prev_trimmed.starts_with('#')
                    || prev_trimmed.starts_with("//"))
                { break; }
                j -= 1;
            }
            if j > start { return j - 1; }
            return i.saturating_sub(1);
        }
    }
    lines.len().saturating_sub(1)
}

/// Apply fixes to file content (in-memory).
/// D9: AhoCorasick single-pass replace instead of per-fix O(content) replace.
/// D10: Single count-and-replace pass.
/// D13: Empty fixes guard.
pub fn apply_fixes(content: &str, fixes: &[(String, String)]) -> (String, usize) {
    if fixes.is_empty() {
        return (content.to_string(), 0);
    }
    // Collect all unique old→new pairs for AhoCorasick.
    let mut unique_old: Vec<&str> = Vec::with_capacity(fixes.len());
    for (old, _) in fixes {
        if !old.is_empty() && !unique_old.contains(&old.as_str()) {
            unique_old.push(old.as_str());
        }
    }
    let Ok(ac) = aho_corasick::AhoCorasick::builder()
        .match_kind(aho_corasick::MatchKind::LeftmostLongest)
        .build(&unique_old) else {
        return (content.to_string(), 0);
    };
    let mut result = String::with_capacity(content.len());
    let mut last_end = 0;
    let mut total = 0usize;
    for mat in ac.find_iter(content) {
        result.push_str(&content[last_end..mat.start()]);
        let old = &unique_old[mat.pattern().as_usize()];
        if let Some((_, new)) = fixes.iter().find(|(o, _)| o.as_str() == *old) {
            result.push_str(new);
            total += 1;
        } else {
            result.push_str(old);
        }
        last_end = mat.end();
    }
    result.push_str(&content[last_end..]);
    (result, total)
}

/// Content-aware matching: find old/new pairs where old exists in content
pub fn content_aware_match(memory_content: &str, file_content: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    for cap in CAM_RE.captures_iter(memory_content) {
        let old_str = cap[1].trim_matches(|c| c == '\'' || c == '"' || c == '`').to_string();
        let new_str = cap[2].trim_matches(|c| c == '\'' || c == '"' || c == '`').to_string();
        if file_content.contains(&old_str) && old_str.len() >= 2 {
            results.push((old_str, new_str));
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
