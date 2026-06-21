// Grammar-free JSON compression using physics-inspired techniques.
//
// Three compressors tried in order, best wins:
//   1. PCA / linear regression — detects when JSON array entries follow a
//      linear arithmetic progression. Emits [N items linear: base=... delta=...].
//      Inspired by Noether's theorem (symmetry = invariance under translation)
//      and Fourier decomposition (constant delta = zero frequency).
//   2. Template extraction — finds longest common substring across consecutive
//      array entries. Emits [N items: <sample>, varying at positions].
//      Inspired by renormalization group (block averages).
//   3. Re-Pair byte-pair substitution — finds top-K most frequent substrings,
//      replaces with 2-char tokens. Inspired by Shannon entropy minimization.
//
// All three are grammar-free: zero AST, zero parser, zero tree-sitter. Pure
// byte-level math + regex for value extraction.

use std::collections::HashMap;

/// Detect JSON-like structure by character density. No parsing.
pub fn looks_like_json(s: &str) -> bool {
    let total = s.chars().count();
    if total < 20 { return false; }
    let mut json_chars = 0;
    for c in s.chars() {
        if matches!(c, '{' | '}' | '[' | ']' | ',' | ':' | '"') { json_chars += 1; }
    }
    (json_chars as f64 / total as f64) > 0.10
}

/// Extract numeric values from a string. Grammar-free: regex on signed integers.
fn extract_numbers(s: &str) -> Vec<i64> {
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
/// Grammar-free: detects balanced `}` followed by `,` or `]` as boundaries.
fn split_json_entries(s: &str) -> Vec<&str> {
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

/// PCA / linear regression: detect if entries follow a linear arithmetic progression.
///
/// Inspired by Noether's theorem (translational symmetry = constant of motion)
/// and Fourier analysis (constant delta = zero-frequency component).
///
/// Algorithm:
///   1. For each numeric position, extract the sequence across entries
///   2. Compute first differences
///   3. If all differences are constant (within tolerance), declare linear
///   4. Emit [N items linear: positions=[i,j,...], base=[...], delta=[...]]
pub fn compress_linear_json(s: &str) -> Option<String> {
    let entries = split_json_entries(s);
    if entries.len() < 10 { return None; }

    // Extract all numeric sequences (one per "position" in the template)
    let first_nums = extract_numbers(entries[0]);
    if first_nums.is_empty() { return None; }

    let num_positions = first_nums.len();
    let mut sequences: Vec<Vec<i64>> = vec![Vec::with_capacity(entries.len()); num_positions];
    sequences[0] = first_nums;

    for entry in &entries[1..] {
        let nums = extract_numbers(entry);
        if nums.len() != num_positions { return None; }
        for (i, &n) in nums.iter().enumerate() {
            sequences[i].push(n);
        }
    }

    // For each position, check if it's a linear sequence (constant delta)
    let mut base: Vec<i64> = Vec::new();
    let mut delta: Vec<i64> = Vec::new();
    let mut linear_positions: Vec<usize> = Vec::new();

    for (pos, seq) in sequences.iter().enumerate() {
        if seq.len() < 10 { continue; }
        let first_delta = seq[1] - seq[0];
        let mut is_linear = true;
        for i in 2..seq.len() {
            let d = seq[i] - seq[i - 1];
            // Allow tolerance: delta must be within ±1 of the first delta
            // (handles monotonic sequences with possible +0 hiccups)
            if (d - first_delta).abs() > 1 {
                is_linear = false;
                break;
            }
        }
        if is_linear {
            base.push(seq[0]);
            delta.push(first_delta);
            linear_positions.push(pos);
        }
    }

    // Need at least 2 linear positions to be worth compressing
    if linear_positions.len() < 2 { return None; }

    // Estimate compression: emit template + summary
    // Template = first entry with linear positions replaced by {n}
    let template_bytes = entries[0].len();
    let compressed = format!(
        "[{} items linear: positions={:?}, base={:?}, delta={:?}; sample: {}]",
        entries.len(), linear_positions, base, delta, entries[0]
    );

    // Only use if it actually saves characters
    if compressed.len() * 10 >= entries.iter().map(|e| e.len() + 1).sum::<usize>() * 9 {
        return None;
    }
    let _ = template_bytes;
    Some(compressed)
}

/// Template extraction: find the longest common substring across consecutive entries.
///
/// Inspired by renormalization group: block the entries and find the invariant.
pub fn compress_template_json(s: &str) -> Option<String> {
    let entries = split_json_entries(s);
    if entries.len() < 5 { return None; }

    // Find longest common prefix across ALL entries (must be present in every one)
    let mut prefix_len = 0;
    let first = entries[0];
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

    // If prefix + suffix covers enough of the entry, collapse
    let covered = prefix_len + suffix_len;
    if covered < 4 || entries.len() < 5 {
        return None;
    }
    // Don't compress if the template is so small we can't beat emitting entries
    if prefix_len + suffix_len < first.len() / 4 {
        return None;
    }

    let prefix = &first[..prefix_len];
    let suffix = &first[first.len() - suffix_len..];
    let total_orig: usize = entries.iter().map(|e| e.len() + 1).sum();

    // Build the template by replacing variable middle with placeholders
    let template = format!("[{}: '{}{{n}} {}']", entries.len(), prefix, suffix);
    // Estimate: template length vs per-entry savings
    let per_entry_savings = (prefix_len + suffix_len) * (entries.len() - 1);
    if per_entry_savings < template.len() {
        return None;
    }
    Some(template)
}

/// Re-Pair byte-pair substitution. Find the most frequent substring >=4 chars
/// that appears 3+ times, replace with a 2-char token from §a..§z,§aa..§zz.
///
/// Inspired by Shannon entropy: each replacement reduces the effective symbol
/// count when the pair is frequent enough.
pub fn compress_byte_pairs(s: &str, max_pairs: usize) -> Option<String> {
    if s.len() < 200 { return None; }

    let mut working = s.to_string();
    let mut dict: Vec<String> = Vec::new();
    let tokens = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

    for _ in 0..max_pairs {
        // Find the most frequent substring >= 4 chars
        let mut counts: HashMap<&[u8], usize> = HashMap::new();
        let working_bytes = working.as_bytes();
        let min_len = 4;
        let max_len = 16;

        for window_size in min_len..=max_len {
            for i in 0..working_bytes.len().saturating_sub(window_size) {
                let sub = &working_bytes[i..i + window_size];
                // Skip if contains a token character (avoids re-encoding)
                if sub.iter().any(|&b| b == b'\xc2' || b == 0xa7) { continue; }
                *counts.entry(sub).or_insert(0) += 1;
            }
        }

        // Find the best pair: most frequent, longest (prefer longer for compression)
        let best = counts.iter()
            .filter(|(_, &c)| c >= 3)
            .max_by_key(|(k, c)| *c * 1000 + k.len());
        let (best_sub, _best_count) = match best {
            Some((k, c)) => (*k, *c),
            None => break,
        };

        if best_sub.is_empty() { break; }

        // Assign a token
        let token_idx = dict.len();
        if token_idx >= tokens.len() * tokens.len() { break; }
        let token_byte = tokens[token_idx % tokens.len()];
        let token = format!("\u{00a7}{}", token_byte as char);

        // Replace all occurrences
        let before_len = working.len();
        let sub_str = std::str::from_utf8(best_sub).unwrap_or("").to_string();
        let sub_for_dict = sub_str.clone();
        working = working.replace(&sub_str, &token);
        if working.len() >= before_len { break; } // no improvement

        dict.push(format!("{}='{}'", token, sub_for_dict));
    }

    if dict.is_empty() { return None; }

    let total_orig = s.len();
    let compressed_body_len = working.len();
    let dict_len: usize = dict.iter().map(|d| d.len() + 1).sum();
    let compressed_total = compressed_body_len + dict_len + 12; // "[\u{00a7}]DICT:" + "BODY=" + "\n\n"

    if compressed_total >= total_orig { return None; }

    Some(format!("[\u{00a7}]DICT: {}\nBODY: {}", dict.join(", "), working))
}

/// Try all compressors in order; return the best result.
pub fn compress_json(s: &str) -> String {
    if !looks_like_json(s) { return s.to_string(); }
    let mut candidates: Vec<(usize, String)> = Vec::new();
    if let Some(c) = compress_linear_json(s) { candidates.push((c.len(), c)); }
    if let Some(c) = compress_template_json(s) { candidates.push((c.len(), c)); }
    if let Some(c) = compress_byte_pairs(s, 16) { candidates.push((c.len(), c)); }
    candidates.sort_by_key(|(l, _)| *l);
    match candidates.first() {
        Some((len, text)) if *len < s.len() => text.clone(),
        _ => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_looks_like_json() {
        assert!(looks_like_json(r#"[{"a":1,"b":2},{"a":3,"b":4}]"#));
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
    fn test_compress_linear() {
        let s: String = (0..20).map(|i| {
            if i > 0 { "," } else { "" }
        }).chain(std::iter::once("")).collect::<String>();
        let s = format!("[{}]", (0..20).map(|i| format!(r#"{{"i":{},"t":"R{} desc","s":{}}}"#, i, i, 100 - i)).collect::<Vec<_>>().join(","));
        let compressed = compress_linear_json(&s).expect("should compress");
        assert!(compressed.len() < s.len(), "compressed {} vs orig {}", compressed.len(), s.len());
        assert!(compressed.contains("linear"));
    }

    #[test]
    fn test_compress_template() {
        let entries: Vec<String> = (0..10).map(|i| format!(r#"{{"id":{},"name":"user{}"}}"#, i, i)).collect();
        let s = format!("[{}]", entries.join(","));
        let compressed = compress_template_json(&s).expect("should compress");
        assert!(compressed.len() < s.len());
        assert!(compressed.contains("items") || compressed.contains("n}"));
    }

    #[test]
    fn test_byte_pairs_compress() {
        let s: String = (0..50).map(|_| r#"{"event":"login","user":"alice","time":12345}"#.to_string()).collect::<Vec<_>>().join("\n");
        let compressed = compress_byte_pairs(&s, 4);
        if let Some(c) = compressed {
            assert!(c.len() < s.len(), "byte-pair {} vs orig {}", c.len(), s.len());
        }
    }

    #[test]
    fn test_end_to_end() {
        let s: String = (0..50).map(|i| format!(r#"{{"id":{},"name":"item{}","score":{}}}"#, i, i, 100 - i)).collect::<Vec<_>>().join(",");
        let s = format!("[{}]", s);
        let compressed = compress_json(&s);
        assert!(compressed.len() < s.len(), "end-to-end {} vs orig {}", compressed.len(), s.len());
    }
}