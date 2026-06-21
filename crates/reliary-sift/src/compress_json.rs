// Grammar-free JSON compression.
//
// Narrow, conservative version. Only fires on near-uniform structured
// JSON arrays. The goal is not maximum compression on synthetic fixtures
// (that's bench-fitting); it's saving tokens on real tool output where
// the LLM needs to understand structure without item-by-item detail.
//
// Three compressors tried in order, best wins:
//   1. PCA / linear regression — fires only when ALL fields are linear
//      and there are 50+ items. Emits [N items: schema=...; first=...,
//      delta=...]. Inspired by Noether's theorem (translational symmetry
//      = constant of motion).
//   2. Template extraction — fires only when 80%+ of entry bytes are
//      covered by a common prefix+suffix. Emits [N: 'prefix{n}suffix'].
//      Inspired by renormalization group (block invariants).
//
// Dropped: byte-pair / Re-Pair compressor. Its output is unreadable
// (`§a` token substitution requires the LLM to mentally decode the
// dictionary) and only fires on real-world JSON ~5% of the time. The
// tokenization cost (unusual characters tokenize as 2-3 tokens each)
// eats the savings.
//
// All compression is grammar-free: zero AST, zero parser, zero
// tree-sitter. Pure byte-level math + regex for value extraction.

// use std::collections::HashMap;

/// Detect JSON-like structure by character density. No parsing.
pub fn looks_like_json(s: &str) -> bool {
    let total = s.chars().count();
    if total < 100 { return false; }
    let mut json_chars = 0;
    for c in s.chars() {
        if matches!(c, '{' | '}' | '[' | ']' | ',' | ':' | '"') { json_chars += 1; }
    }
    (json_chars as f64 / total as f64) > 0.10
}

/// Extract numeric values from a string. Grammar-free: byte-level scan.
pub fn extract_numbers(s: &str) -> Vec<i64> {
    let mut nums = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() || (bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()) {
            let neg = bytes[i] == b'-';
            let start = if neg { i + 1 } else { i };
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() { end += 1; }
            if let Ok(n) = s[start..end].parse::<i64>() {
                nums.push(if neg { -n } else { n });
            }
            i = end;
        } else {
            i += 1;
        }
    }
    nums
}

/// Split a JSON array string into individual object entries.
/// Grammar-free: balanced-brace tracking.
pub fn split_json_entries(s: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    let bytes = s.as_bytes();
    let mut depth = 0;
    let mut start = None;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escape { escape = false; continue; }
        if in_string {
            if b == b'\\' { escape = true; }
            else if b == b'"' { in_string = false; }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 { start = Some(i); }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(st) = start {
                        entries.push(&s[st..=i]);
                        start = None;
                    }
                }
            }
            _ => {}
        }
    }
    entries
}

/// PCA / linear regression: detect if entries follow a linear arithmetic
/// progression. Conservative: requires 50+ items AND ALL fields linear.
///
/// Inspired by Noether's theorem (translational symmetry = constant of
/// motion) and Fourier analysis (constant delta = zero-frequency component).
pub fn compress_linear_json(s: &str) -> Option<String> {
    let entries = split_json_entries(s);
    // Conservative threshold: real-world JSON arrays of 50+ linear items are rare
    if entries.len() < 50 { return None; }

    let first_nums = extract_numbers(entries[0]);
    if first_nums.is_empty() { return None; }

    let num_positions = first_nums.len();
    let mut sequences: Vec<Vec<i64>> = vec![Vec::with_capacity(entries.len()); num_positions];
    for (i, &n) in first_nums.iter().enumerate() {
        sequences[i].push(n);
    }

    for entry in &entries[1..] {
        let nums = extract_numbers(entry);
        if nums.len() != num_positions { return None; }
        for (i, &n) in nums.iter().enumerate() {
            sequences[i].push(n);
        }
    }

    // ALL numeric positions must be linear (not just 2+)
    let mut base: Vec<i64> = Vec::new();
    let mut delta: Vec<i64> = Vec::new();
    for seq in &sequences {
        if seq.len() < 50 { return None; }
        let first_delta = seq[1] - seq[0];
        for i in 2..seq.len() {
            let d = seq[i] - seq[i - 1];
            // Strict tolerance: delta must equal first_delta exactly
            // (real-world data has noise; we want perfect linearity)
            if d != first_delta {
                return None;
            }
        }
        base.push(seq[0]);
        delta.push(first_delta);
    }

    // Estimate compression: template + summary vs per-entry
    let first = entries[0];
    let total_orig: usize = entries.iter().map(|e| e.len() + 1).sum();
    let compressed = format!(
        "[{} items: all fields linear, first={}, delta={:?}]",
        entries.len(), first, delta
    );
    if compressed.len() >= total_orig { return None; }
    Some(compressed)
}

/// Template extraction: find the longest common prefix/suffix across entries.
/// Conservative: requires 80%+ of entry bytes to be covered by prefix+suffix.
///
/// Inspired by renormalization group: block the entries, find the invariant.
pub fn compress_template_json(s: &str) -> Option<String> {
    let entries = split_json_entries(s);
    if entries.len() < 10 { return None; }

    let first = entries[0];

    // Find longest common prefix across ALL entries
    let mut prefix_len = 0;
    'outer: for i in 0..first.len() {
        let c = first.as_bytes()[i];
        for entry in &entries[1..] {
            if i >= entry.len() || entry.as_bytes()[i] != c {
                break 'outer;
            }
        }
        prefix_len = i + 1;
    }

    // Find longest common suffix
    let mut suffix_len = 0;
    'outer2: for i in 0..first.len() {
        let c = first.as_bytes()[first.len() - 1 - i];
        for entry in &entries[1..] {
            if i >= entry.len() || entry.as_bytes()[entry.len() - 1 - i] != c {
                break 'outer2;
            }
        }
        suffix_len = i + 1;
    }

    // Strict threshold: 80%+ of entry must be covered
    let covered = prefix_len + suffix_len;
    if covered * 5 < first.len() * 4 {
        return None;
    }

    let prefix = &first[..prefix_len];
    let suffix = &first[first.len() - suffix_len..];
    let total_orig: usize = entries.iter().map(|e| e.len() + 1).sum();

    // Emit the template once plus a list of just the varying middle.
    // This is genuinely compression: the LLM sees the schema + N varying values.
    let varying: Vec<String> = entries.iter()
        .map(|e| e[prefix_len..e.len() - suffix_len].to_string())
        .collect();
    let compressed = format!(
        "[{}: '{}{{n}}{}'] varying={:?}",
        entries.len(), prefix, suffix, varying
    );
    if compressed.len() >= total_orig {
        return None;
    }
    Some(compressed)
}

/// Try all compressors in order; return the best result, or input if no
/// improvement.
pub fn compress_json(s: &str) -> String {
    if !looks_like_json(s) { return s.to_string(); }
    let mut candidates: Vec<(usize, String)> = Vec::new();
    if let Some(c) = compress_linear_json(s) { candidates.push((c.len(), c)); }
    if let Some(c) = compress_template_json(s) { candidates.push((c.len(), c)); }
    candidates.sort_by_key(|(l, _)| *l);
    match candidates.first() {
        Some((len, text)) if *len < s.len() * 4 / 5 => text.clone(),  // Only if 20%+ savings
        _ => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_looks_like_json() {
        // 100+ chars, dense JSON syntax
        let s = r#"[{"a":1,"b":2,"c":3},{"a":4,"b":5,"c":6},{"a":7,"b":8,"c":9},{"a":10,"b":11,"c":12},{"a":13,"b":14,"c":15},{"a":16,"b":17,"c":18}]"#;
        assert!(looks_like_json(s), "{} should look like JSON", s);
        assert!(!looks_like_json("def foo(): pass\ndef bar(): pass"));
    }

    #[test]
    fn test_extract_numbers() {
        let nums = extract_numbers(r#"{"i":42,"t":"R5 desc","s":-3}"#);
        assert_eq!(nums, vec![42, 5, -3]);
    }

    #[test]
    fn test_split_json_entries() {
        let s = r#"[{"a":1},{"a":2},{"a":3}]"#;
        let entries = split_json_entries(s);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], r#"{"a":1}"#);
    }

    #[test]
    fn test_compress_linear_50_threshold() {
        // 20 items should NOT compress (below 50 threshold)
        let s: String = (0..20).map(|i| format!(r#"{{"i":{},"s":{}}}"#, i, 100 - i)).collect::<Vec<_>>().join(",");
        let s = format!("[{}]", s);
        assert!(compress_linear_json(&s).is_none(), "should not compress 20 items");
    }

    #[test]
    fn test_compress_linear_with_outlier_fails() {
        // 60 items but one is broken — strict tolerance should reject
        let mut items: Vec<String> = (0..59).map(|i| format!(r#"{{"i":{},"s":{}}}"#, i, 100 - i)).collect();
        items.push(r#"{"i":99,"s":-1}"#.to_string()); // outlier
        let s = format!("[{}]", items.join(","));
        assert!(compress_linear_json(&s).is_none(), "outlier should reject strict linear");
    }

    #[test]
    fn test_compress_template() {
        // 20 items, all fields identical except one varying — 90%+ prefix coverage
        let entries: Vec<String> = (0..20).map(|i| format!(r#"{{"id":{},"name":"a_very_long_static_username_string_xxxxxxxxxxxx","score":100}}"#, i)).collect();
        let s = format!("[{}]", entries.join(","));
        let compressed = compress_template_json(&s).expect("should compress");
        assert!(compressed.len() < s.len());
    }

    #[test]
    fn test_end_to_end_conservative() {
        // Realistic JSON: 20 items, varied values — should NOT compress
        let s: String = (0..20).map(|i| format!(r#"{{"id":{},"name":"item_{}","score":{}}}"#, i, i, i * 7 + 3)).collect::<Vec<_>>().join(",");
        let s = format!("[{}]", s);
        let compressed = compress_json(&s);
        // Below thresholds: should pass through
        assert_eq!(compressed, s, "20 items should not compress, got {} vs {}", compressed.len(), s.len());
    }

    #[test]
    fn test_end_to_end_highly_regular() {
        // 50+ items, all linear — SHOULD compress
        let s: String = (0..60).map(|i| format!(r#"{{"i":{},"s":{}}}"#, i, 100 - i)).collect::<Vec<_>>().join(",");
        let s = format!("[{}]", s);
        let compressed = compress_json(&s);
        assert!(compressed.len() < s.len(), "60 linear items should compress, got {} vs {}", compressed.len(), s.len());
    }
}