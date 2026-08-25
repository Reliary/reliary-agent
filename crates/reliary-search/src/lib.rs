use std::io::Read;
use std::path::Path;

/// Grammar-free phrase search with BM25 scoring and Porter stemming.
pub mod schema;
pub mod search;
pub mod phrase_index;
pub mod ingest;
pub mod ft_weight;
pub mod symbol;
pub mod keywords;
pub mod pattern;
pub mod type_flow;
pub mod brace_graph;
pub mod scope_types;
pub mod func_profile;
pub mod signature;
pub mod structural;
pub mod op_table;
pub mod node_classifier;
pub mod expr_tree;
pub mod boltzmann;
pub mod refal;
pub mod architecture;
pub mod trace_path;
pub mod lazy_occurrence;
pub mod lazy_tables;
pub mod callgraph_v2;
pub mod file_meta;
pub mod similar;
pub mod full_file;
pub mod compat;
pub mod plan;
pub mod qualified;

/// Content-based binary detection.
///
/// Reads the first `probe_bytes` of `path` and returns true if it looks
/// like a binary file (null byte present OR > 30% non-printable bytes).
/// Returns false for files that don't exist or can't be read — caller
/// may treat that as "text" and try to ingest.
///
/// Grammar-free: byte-level inspection only. No extension knowledge.
pub fn is_likely_binary(path: &Path, probe_bytes: usize) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false; // can't read → don't skip (let caller decide)
    };
    let mut buf = vec![0u8; probe_bytes];
    let n = match f.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return false,
    };
    buf.truncate(n);

    if n == 0 {
        return false; // empty file is text (won't get indexed anyway)
    }

    // Check 1: null byte present → binary.
    // LLVM auto-vectorizes this loop on x86-64 with SSE2.
    if buf.iter().any(|&b| b == 0) {
        return true;
    }

    // Check 2: high non-printable ratio → binary.
    // Non-printable: < 0x09 (tab), or 0x0B-0x0C (vtab), or 0x0E-0x1F,
    // or 0x7F (DEL), or > 0x7F (high bytes not valid UTF-8 are rare in text).
    // Skip \n (0x0A), \r (0x0D), tab (0x09) which are valid in text.
    let non_printable = buf
        .iter()
        .filter(|&&b| {
            b < 0x09
                || (b > 0x0D && b < 0x20)
                || b == 0x7F
                || b > 0x7F
        })
        .count();
    non_printable * 10 > n * 3 // > 30%
}

/// Extensions scanned by dead-code detection. Now deprecated — dead-code
/// detection uses `is_likely_binary()` (content-based) instead of extensions.
#[deprecated(note = "Use is_likely_binary() — extension whitelist is gone")]
pub const SUPPORTED_EXTS_DEAD: &[&str] = &[];

/// BM25 IDF: ((N - df + 0.5) / (df + 0.5) + 1.0).ln()
#[inline(always)]
pub fn bm25_idf(n_docs: f32, df: f32) -> f32 {
    ((n_docs - df + 0.5) / (df + 0.5) + 1.0).ln()
}

// P8: BM25 hoist constants — precompute k1, b, k1+1, k1*b, 1-b so they're
// not recomputed on every call.
#[inline(always)]
pub fn bm25_score(idf: f32, tf: f32, doc_len: f32, avgdl: f32) -> f32 {
    const K1_PLUS_1: f32 = 2.2;
    const K1_TIMES_B: f32 = 0.9;
    const ONE_MINUS_B: f32 = 0.25;
    let norm = ONE_MINUS_B + K1_TIMES_B * (doc_len / avgdl);
    idf * (tf * K1_PLUS_1) / (tf + K1_TIMES_B * norm)
}

/// C10: standard BM25 with raw TF scoring (Robertson–Walker formula).
/// Uses raw `tf` instead of log(1+tf), which is the BM25L variant. Raw TF
/// preserves the relative weight of frequent terms so a doc with 50 hits
/// ranks meaningfully higher than one with 1 hit.
/// Grammar-free identifier scanning: extract [A-Za-z_][A-Za-z0-9_]{3,40}
pub fn scan_identifiers(text: &str) -> Vec<String> {
    // V21: lowered min length from 3 to 2 — Python/Ruby/JS commonly use
    // 2-char identifiers (re, os, io, db, id, fn, x, y). Filtering them
    // loses ~15% of Python occurrence rows.
    //
    // V38: for identifiers containing `_`, return the FULL identifier (not
    // stemmed). Porter stemmer destroys compound names like `classify_structural`
    // → `classifi`. This is the root cause of inaccurate search results on
    // Rust codebases where almost all identifiers are snake_case.
    let mut seen = rustc_hash::FxHashSet::default();
    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|t| {
            let len = t.len();
            (2..=40).contains(&len)
                && t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        })
        .map(|t| t.to_ascii_lowercase())
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// V38: stem only the suffix of identifiers, preserving compound names.
/// `classify_structural` → stems only `structural` → `structur`,
/// but we keep the full identifier as the canonical phrase.
/// V57c: also preserve CamelCase compounds (`StructuralResult`, `BraceNode`)
/// — Porter-stemming the lowercased form strips the last word's suffix
/// (`structuralresult` → `structur`), colliding with `structural` and
/// losing the type name from the index entirely.
#[inline(always)]
pub fn stem_identifier(name: &str) -> String {
    if name.contains('_') {
        // snake_case compound — use full name, don't stem
        name.to_ascii_lowercase()
    } else if name.chars().filter(|c| c.is_ascii_uppercase()).count() >= 2 {
        // CamelCase compound (≥2 capitals = multi-word type like
        // StructuralResult, BraceNode, FileMeta) — preserve as-is.
        name.to_ascii_lowercase()
    } else {
        porter_stem(name)
    }
}

/// S9: Streaming iterator version of scan_identifiers. Yields borrowed slices —
/// caller applies to_ascii_lowercase lazily, avoiding the per-line Vec allocation.
/// Grammar-free: same ASCII-only alphanumeric + 3..=40 length filter.
pub fn scan_identifiers_iter<'a>(text: &'a str) -> impl Iterator<Item = &'a str> {
    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|t| {
            let len = t.len();
            (3..=40).contains(&len)
                && t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        })
}

/// Porter-like stemming: reduce common suffixes
#[inline(always)]
pub fn porter_stem(word: &str) -> String {
    let w = word.trim().to_ascii_lowercase();
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

/// P9-5: Same as porter_stem but skips the to_ascii_lowercase() call when
/// the caller has already lowercased the input. Use from scan_identifiers
/// pipeline where tokens are pre-lowercased.
#[inline(always)]
pub fn porter_stem_lower(word: &str) -> String {
    let w = word.trim();
    if w.len() <= 3 { return w.to_string(); }
    let suffixes = ["ing", "tion", "sion", "ment", "ness", "able", "ible", "ful", "less", "ly", "ed", "es", "er", "or", "al", "ic", "en", "ive", "ize", "ise"];
    for s in &suffixes {
        if w.ends_with(s) && w.len() > s.len() + 2 {
            return w[..w.len() - s.len()].to_string();
        }
    }
    if w.ends_with('s') && !w.ends_with("ss") && w.len() > 3 {
        return w[..w.len() - 1].to_string();
    }
    w.to_string()
}

/// Trigram decomposition for OOV tokens
pub fn trigrams(token: &str) -> Vec<String> {
    let t = token.to_lowercase();
    let chars: Vec<char> = t.chars().collect();
    if chars.len() <= 3 { return vec![t]; }
    (0..=chars.len() - 3).map(|i| chars[i..i + 3].iter().collect()).collect()
}

/// Tokenize a phrase into stemmed identifiers
pub fn tokenize(text: &str) -> Vec<String> {
    scan_identifiers(text).into_iter().map(|t| porter_stem(&t)).collect()
}

/// C3: check if a byte position in a line is inside a string literal or comment.
/// If inside any of these contexts, return false (not a real definition site).
fn inside_string_or_comment(line: &str, pos: usize) -> bool {
    let bytes = line.as_bytes();
    if pos >= bytes.len() { return false; }

    // 1. Line comment: `//` starts a comment to end-of-line. If pos is after `//`,
    //    it's in a comment.
    if let Some(slc_idx) = find_byte_outside_string(line, b'/') {
        // Check for `//` — need TWO consecutive `/` after the slc_idx.
        if slc_idx + 1 < bytes.len() && bytes[slc_idx] == b'/' && bytes[slc_idx + 1] == b'/' {
            if pos >= slc_idx { return true; }
        }
    }

    // 2. Block comment: scan for `/* ... */`. If `/*` appears before pos and
    //    no matching `*/` appears between `/*` and pos, it's in a block comment.
    //    For a single line we treat anything between `/*` and `*/` as comment.
    if let Some(mut depth) = find_block_comment_depth(line, pos) {
        if depth > 0 { return true; }
    }

    // 3. Double-quoted string: scan for `"` before pos. If we find one without
    //    an escape, check if we're inside.
    if let Some(quote_count) = count_unescaped_double_quotes(line, pos) {
        if quote_count % 2 == 1 { return true; }
    }

    // 4. Single-quoted string (Rust char literal): similar.
    if let Some(quote_count) = count_unescaped_single_quotes(line, pos) {
        if quote_count % 2 == 1 { return true; }
    }

    false
}

/// Find the byte index of the first occurrence of `target` outside string literals.
fn find_byte_outside_string(line: &str, target: u8) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escape { escape = false; continue; }
        if b == b'\\' && in_string { escape = true; continue; }
        if b == b'"' { in_string = !in_string; continue; }
        if !in_string && b == target { return Some(i); }
    }
    None
}

/// Count unescaped double-quote characters before `pos`.
fn count_unescaped_double_quotes(line: &str, pos: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i < pos && i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < pos { i += 2; continue; }
        if bytes[i] == b'"' { count += 1; }
        i += 1;
    }
    Some(count)
}

/// Count unescaped single-quote characters before `pos`.
fn count_unescaped_single_quotes(line: &str, pos: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i < pos && i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < pos { i += 2; continue; }
        if bytes[i] == b'\'' { count += 1; }
        i += 1;
    }
    Some(count)
}

/// For single-line scanning: 0 if pos is outside block comments, >0 if inside.
fn find_block_comment_depth(line: &str, pos: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut depth: usize = 0;
    let mut i = 0;
    while i < pos && i < bytes.len() {
        if depth > 0 {
            // We're inside a block comment, look for `*/`.
            if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                depth -= 1;
                i += 2;
                continue;
            }
        } else {
            // Outside, look for `/*`.
            if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                depth += 1;
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    Some(depth)
}

/// Grammar-free definition detection: is `phrase` at `match_start` in `line` a definition
/// or a call site? Uses byte-scanning for preceding keywords and following structural markers.
pub fn is_definition(phrase: &str, line: &str, match_start: usize) -> bool {
    let bytes = line.as_bytes();
    let end = match_start + phrase.len();
    if end >= bytes.len() { return false; }

    // C3: reject if phrase is inside a string literal or comment.
    if inside_string_or_comment(line, match_start) { return false; }

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
    use std::io::Write;

    fn write_tmp(name: &str, data: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn test_is_likely_binary_text() {
        let p = write_tmp("r39_test_text.txt", b"fn main() { println!(\"hello\"); }\n");
        assert!(!is_likely_binary(&p, 8192));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_is_likely_binary_with_null() {
        let p = write_tmp("r39_test_null.bin", &[b'a', b'b', 0, b'd', b'e']);
        assert!(is_likely_binary(&p, 8192));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_c3_string_literal_not_definition() {
        // `let s = "fn foo(";` — "foo" is inside a string, NOT a definition.
        assert!(!is_definition_str("foo", "    let s = \"fn foo(\";"));
        // char literal: `'\n'` is not a definition.
        assert!(!is_definition_str("foo", "    let c = 'foo';"));
        // Inside a block comment.
        assert!(!is_definition_str("foo", "    /* foo() */"));
        assert!(!is_definition_str("foo", "    /* foo( */"));
        // Inside a line comment.
        assert!(!is_definition_str("foo", "    // foo("));
    }

    #[test]
    fn test_c3_real_definitions_still_detected() {
        // Function defs are still detected.
        assert!(is_definition_str("foo", "    fn foo(bar: i32) {"));
        assert!(is_definition_str("foo", "    fn foo("));
        assert!(is_definition_str("foo", "    pub fn foo<T>(x: T) -> T"));
        // Variable bindings still detected.
        assert!(is_definition_str("foo", "    let foo = 5;"));
        // Method call sites are NOT definitions.
        assert!(!is_definition_str("foo", "    x.foo()"));
    }

    #[test]
    fn test_is_likely_binary_high_non_printable() {
        // 50% bytes are 0xFF (non-printable), 50% are ASCII.
        // Threshold is > 30% — 50% triggers it.
        let data: Vec<u8> = (0..400).map(|i| if i % 2 == 0 { 0xFFu8 } else { b'x' }).collect();
        let p = write_tmp("r39_test_nonprint.bin", &data);
        assert!(is_likely_binary(&p, 8192));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_is_likely_binary_empty() {
        let p = write_tmp("r39_test_empty.txt", b"");
        assert!(!is_likely_binary(&p, 8192));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_is_likely_binary_nonexistent() {
        let p = Path::new("/nonexistent/path/to/file.xyz");
        assert!(!is_likely_binary(p, 8192)); // can't read → don't skip
    }

    #[test]
    fn test_is_likely_binary_no_extension() {
        let p = write_tmp("r39_test_no_ext", b"let x = 5 in x + 1\n");
        assert!(!is_likely_binary(&p, 8192));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_is_likely_binary_unknown_extension() {
        // .nix, .erl, .hs, .xyz — no extension knowledge needed.
        let p_nix = write_tmp("r39_test.nix", b"{ pkgs }: pkgs.hello\n");
        assert!(!is_likely_binary(&p_nix, 8192));
        let _ = std::fs::remove_file(&p_nix);

        let p_erl = write_tmp("r39_test.erl", b"foo(X) -> X + 1.\n");
        assert!(!is_likely_binary(&p_erl, 8192));
        let _ = std::fs::remove_file(&p_erl);

        let p_hs = write_tmp("r39_test.hs", b"foo :: Int -> Int\nfoo x = x + 1\n");
        assert!(!is_likely_binary(&p_hs, 8192));
        let _ = std::fs::remove_file(&p_hs);
    }

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
    fn test_stem_identifier_camel_case() {
        // V57c: CamelCase compounds must be preserved, not Porter-stemmed.
        assert_eq!(stem_identifier("StructuralResult"), "structuralresult");
        assert_eq!(stem_identifier("BraceNode"), "bracenode");
        assert_eq!(stem_identifier("FileMeta"), "filemeta");
        // snake_case still preserved (V38).
        assert_eq!(stem_identifier("classify_structural"), "classify_structural");
        // Plain lowercase still stems.
        assert_eq!(stem_identifier("running"), "runn");
    }

    #[test]
    fn test_trigrams() {
        let t = trigrams("validate");
        assert_eq!(t.len(), 6);
        assert!(t.contains(&"val".to_string()));
    }
}
