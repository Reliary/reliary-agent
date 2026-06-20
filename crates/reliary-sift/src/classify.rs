// Structural classification with skeleton normalization and strategy detection.
// Grammar-free: byte DFA + indentation, no keyword lists, no language detection.



/// A single classified line.
#[derive(Debug, Clone)]
pub struct Line {
    pub text: String,
    pub repeat_dist: usize,
    pub skeleton_key: u64,
    pub is_error: bool,
    pub is_separator: bool,
    pub is_progress: bool,
    pub is_key_value: bool,
    pub is_summary: bool,
    pub index: usize,
}

/// Compression strategy determined by structural detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionStrategy {
    /// JSON/YAML — keep all structural tokens
    Json,
    /// Unified diff — preserve hunk headers, collapse context
    Diff,
    /// Tabular output — column pruning via visual gutters
    Tabular,
    /// Prefix-similar output (grep-like) — prefix-aware grouping
    Prefixed,
    /// Default — skeleton grouping + OK-collapse + error preservation
    Normal,
}

/// Skeleton group: lines with identical structural skeleton.
#[derive(Debug, Clone)]
pub struct LineGroup {
    pub skeleton_key: u64,
    pub sample: String,
    pub count: usize,
    pub first_idx: usize,
    pub is_error: bool,
    pub distinct_prefixes: Vec<String>,
}

/// Classify all lines using structural heuristics.
pub fn classify(text: &str) -> Vec<Line> {
    classify_with_lines(text.lines())
}

fn classify_with_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<Line> {
    let mut result: Vec<Line> = Vec::new();
    let mut prev_lines: Vec<String> = Vec::new();
    for (idx, line) in lines.enumerate() {
        let clean = strip_all_ansi(line);
        let repeat_dist = find_repeat_str(&prev_lines, &clean);
        let skey = skeleton_hash(&clean);
        let is_error = clean.contains("Error:")
            || clean.contains("error[")
            || clean.contains("FAILED")
            || clean.starts_with("  --> ")
            || clean.starts_with("error:");
        let is_separator = !clean.trim().is_empty()
            && clean.trim().chars().all(|c| c == '-' || c == '=' || c == '.' || c == '_' || c == '*');
        let is_progress = is_progress_line(&clean);
        let is_key_value = clean.contains(": ") || clean.contains('=');
        let is_summary = is_summary_line(&clean);

        prev_lines.push(clean.clone());
        if prev_lines.len() > 20 { prev_lines.remove(0); }

        result.push(Line {
            text: clean, repeat_dist, skeleton_key: skey,
            is_error, is_separator, is_progress, is_key_value, is_summary, index: idx,
        });
    }
    result
}

/// Strip ALL ANSI escape sequences (CSI, OSC, DCS, SOS, PM, APC).
pub fn strip_all_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                        for n in chars.by_ref() {
                            if n.is_ascii_alphabetic() || n == '~' || n == '"' { break; }
                        }
                    }
                Some(']') => {
                    loop { match chars.next() { Some('\x07') => break, Some('\x1b') => { chars.next(); break; } None => break, _ => continue, } }
                }
                Some('P') | Some('X') | Some('^') | Some('_') => {
                    loop { match chars.next() { Some('\x07') => break, Some('\x1b') => { chars.next(); break; } None => break, _ => continue, } }
                }
                _ => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Normalize line to structural skeleton: replace numbers, hex, versions with placeholders.
pub fn skeleton(line: &str) -> String {
    let s = line.trim();
    if s.is_empty() { return String::new(); }
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut result = String::with_capacity(len.min(512));
    let mut last_space = false;

    macro_rules! emit_str {
        ($s:expr) => { last_space = false; result.push_str($s); };
    }

    while i < len {
        // 0. Progress bar
        if bytes[i] == b'[' && i + 3 < len {
            if let Some(cls) = s[i..].find(']') {
                let inner = &s[i+1..i+cls];
                if !inner.trim().is_empty() && inner.chars().all(|c| c == '#' || c == '.' || c == '=' || c == '>' || c == '-' || c == '_' || c.is_whitespace()) {
                    emit_str!("{progress}"); i += cls + 1; continue;
                }
            }
        }

        // 1. UUID
        if i + 36 <= len {
            let mut is_uuid = true;
            for j in 0..36 {
                let b = bytes[i + j];
                let expect_dash = j == 8 || j == 13 || j == 18 || j == 23;
                if expect_dash { if b != b'-' { is_uuid = false; break; } }
                else if !b.is_ascii_hexdigit() { is_uuid = false; break; }
            }
            if is_uuid { emit_str!("{uuid}"); i += 36; continue; }
        }

        let is_alpha = |pos: usize| -> bool { bytes[pos].is_ascii_alphabetic() };
        let is_digit = |pos: usize| -> bool { bytes[pos].is_ascii_digit() };
        let is_alnum = |pos: usize| -> bool { bytes[pos].is_ascii_alphanumeric() };
        let bd = |pos: usize| -> bool { pos == 0 || !is_alnum(pos - 1) };

        // 2. word-NNN
        if bd(i) && is_alpha(i) {
            let mut we = i;
            while we < len && is_alpha(we) { we += 1; }
            if we > i + 2 && we < len && bytes[we] == b'-' && we + 1 < len && is_digit(we + 1) {
                let mut dne = we + 1;
                while dne < len && is_digit(dne) { dne += 1; }
                if dne > we + 1 { emit_str!("{w}-{n}"); i = dne; continue; }
            }
        }

        // 3. Hex hash (7-40 chars)
        if bd(i) && bytes[i].is_ascii_hexdigit() {
            let mut he = i;
            while he < len && (bytes[he] as char).is_ascii_hexdigit() { he += 1; }
            if he - i >= 7 && he - i <= 40 && (he >= len || !is_alnum(he)) {
                emit_str!("{hash}"); i = he; continue;
            }
        }

        // 4. Version X.Y.Z
        if is_digit(i) {
            let ve = num_end(i, bytes);
            if ve > i && ve < len && bytes[ve] == b'.' {
                let v2e = num_end(ve + 1, bytes);
                if v2e > ve + 1 && v2e < len && bytes[v2e] == b'.' {
                    let v3e = num_end(v2e + 1, bytes);
                    if v3e > v2e + 1 { emit_str!("{ver}"); i = v3e; continue; }
                }
            }
        }

        // 5. Number
        if bd(i) && is_digit(i) {
            let ne = num_end(i, bytes);
            if ne > i && (ne >= len || !is_alnum(ne)) {
                emit_str!("{n}"); i = ne; continue;
            }
        }

        // 6. Regular char
        let ch = s[i..].chars().next().unwrap_or(' ');
        if ch.is_whitespace() {
            if !last_space { result.push(' '); last_space = true; }
            i += ch.len_utf8();
        } else {
            result.push(ch);
            last_space = false;
            i += ch.len_utf8();
        }
    }

    result.trim().to_lowercase()
}

fn num_end(start: usize, bytes: &[u8]) -> usize {
    let mut e = start;
    while e < bytes.len() && (bytes[e] as char).is_ascii_digit() { e += 1; }
    e
}

fn skeleton_hash(text: &str) -> u64 {
    let s = skeleton(text);
    if s.is_empty() { return 0; }
    let mut h: u64 = 5381;
    for b in s.bytes() { h = h.wrapping_mul(33).wrapping_add(b as u64); }
    h
}

fn find_repeat_str(prev: &[String], line: &str) -> usize {
    let trimmed = line.trim();
    if trimmed.is_empty() { return 1; }
    for (i, pl) in prev.iter().enumerate().rev() {
        if pl.trim() == trimmed { return prev.len() - i; }
    }
    0
}

/// Returns true if this line should be dropped from output entirely.
pub fn should_drop(line: &Line) -> bool {
    if line.is_error { return false; }
    if line.text.trim().is_empty() { return true; }
    if line.is_separator { return true; }
    if line.is_progress { return true; }
    if line.text.trim().len() <= 2 { return true; }
    false
}

/// Group structurally identical lines (same skeleton) into runs.
pub fn skeleton_groups(lines: &[Line]) -> Vec<LineGroup> {
    let mut groups: Vec<LineGroup> = Vec::new();
    let mut current: Option<LineGroup> = None;
    for line in lines {
        if should_drop(line) { continue; }
        let prefix = extract_leading_token(&line.text);
        match &mut current {
            None => {
                current = Some(LineGroup {
                    skeleton_key: line.skeleton_key, sample: line.text.clone(),
                    count: 1, first_idx: line.index, is_error: line.is_error,
                    distinct_prefixes: prefix.clone().map_or(vec![], |p| vec![p]),
                });
            }
            Some(ref mut g) if g.skeleton_key == line.skeleton_key => {
                g.count += 1;
                if let Some(ref p) = prefix {
                    if !g.distinct_prefixes.contains(p) {
                        g.distinct_prefixes.push(p.clone());
                    }
                }
            }
            Some(g) => {
                groups.push(std::mem::replace(g, LineGroup {
                    skeleton_key: line.skeleton_key, sample: line.text.clone(),
                    count: 1, first_idx: line.index, is_error: line.is_error,
                    distinct_prefixes: prefix.clone().map_or(vec![], |p| vec![p]),
                }));
            }
        }
    }
    if let Some(g) = current { groups.push(g); }
    groups
}

/// Extract the leading token (before `:`) from a line, for grep-like file:line prefix tracking.
fn extract_leading_token(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let colon = trimmed.find(':')?;
    let token = trimmed[..colon].trim();
    if token.len() >= 2 && token.len() < 120 { Some(token.to_string()) } else { None }
}

/// Skeleton grouping with prefix awareness for grep-like output.
pub fn skeleton_groups_prefixed(lines: &[Line]) -> Vec<LineGroup> {
    let mut groups: Vec<LineGroup> = Vec::new();
    let mut current: Option<LineGroup> = None;
    for line in lines {
        if should_drop(line) { continue; }
        let prefix = extract_leading_token(&line.text);
        let combined_key = match &prefix {
            Some(p) => {
                let mut h: u64 = 5381;
                for b in p.bytes() { h = h.wrapping_mul(33).wrapping_add(b as u64); }
                h ^ line.skeleton_key
            }
            None => line.skeleton_key,
        };
        match &mut current {
            None => {
                current = Some(LineGroup {
                    skeleton_key: combined_key, sample: line.text.clone(),
                    count: 1, first_idx: line.index, is_error: line.is_error,
                    distinct_prefixes: prefix.clone().map_or(vec![], |p| vec![p]),
                });
            }
            Some(ref mut g) if g.skeleton_key == combined_key => {
                g.count += 1;
                if let Some(ref p) = prefix {
                    if !g.distinct_prefixes.contains(p) {
                        g.distinct_prefixes.push(p.clone());
                    }
                }
            }
            Some(g) => {
                groups.push(std::mem::replace(g, LineGroup {
                    skeleton_key: combined_key, sample: line.text.clone(),
                    count: 1, first_idx: line.index, is_error: line.is_error,
                    distinct_prefixes: prefix.clone().map_or(vec![], |p| vec![p]),
                }));
            }
        }
    }
    if let Some(g) = current { groups.push(g); }
    groups
}

/// Find vertical gutters (columns of spaces) in a block of text.
fn find_visual_gutters(data: &[&str]) -> Vec<usize> {
    if data.is_empty() { return Vec::new(); }
    let max_len = data.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    if max_len == 0 { return Vec::new(); }
    let mut space_counts = vec![0; max_len];
    for line in data {
        let chars: Vec<char> = line.chars().collect();
        for i in 0..chars.len() {
            if chars[i] == ' ' { space_counts[i] += 1; }
        }
    }
    let threshold = (data.len() as f64 * 0.90) as usize;
    let mut gutters = Vec::new();
    let mut in_gutter = false;
    for (i, &count) in space_counts.iter().enumerate() {
        if count >= threshold {
            if !in_gutter { gutters.push(i); in_gutter = true; }
        } else { in_gutter = false; }
    }
    gutters
}

/// Detect and compress tabular output via visual gutter column pruning.
pub fn compress_tabular(lines: &mut [Line]) -> bool {
    let data: Vec<&str> = lines.iter()
        .filter(|l| !l.text.trim().is_empty() && !l.is_separator)
        .map(|l| l.text.as_str())
        .collect();
    if data.len() < 4 { return false; }
    let gutters = find_visual_gutters(&data);
    if gutters.len() < 2 { return false; }

    let ncol = gutters.len() + 1;
    let matrix: Vec<Vec<&str>> = data.iter()
        .filter(|l| {
            let chars: Vec<char> = l.chars().collect();
            gutters.iter().all(|&g| g >= chars.len() || chars[g] == ' ')
        })
        .map(|l| {
            let mut fields = Vec::new();
            let mut start = 0;
            for &g in &gutters {
                let field = &l[byte_idx(l, start)..byte_idx(l, g)];
                fields.push(field.trim());
                start = g;
            }
            fields.push(l[byte_idx(l, start)..].trim());
            fields
        })
        .filter(|f| f.len() == ncol)
        .collect();
    if (matrix.len() as f64 / data.len() as f64) < 0.8 { return false; }

    let mut drop_col = vec![false; ncol];
    for c in 0..ncol {
        if c == ncol - 1 { continue; }
        let mut noise_count = 0;
        let mut seen = std::collections::HashSet::new();
        let mut has_colon = 0;
        for row in &matrix {
            let val = row[c];
            seen.insert(val);
            if val.contains(':') { has_colon += 1; }
            let sk = skeleton(val);
            if sk == "{n}" || sk == "{hash}" || sk == "{uuid}" || sk == "{ver}" || sk == "{n}.{n}" || sk == "{n}:{n}" || sk == "?" || sk == "-" {
                noise_count += 1;
            }
        }
        if has_colon as f64 / matrix.len() as f64 > 0.5 { continue; }
        let noise_ratio = noise_count as f64 / matrix.len() as f64;
        let unique_ratio = seen.len() as f64 / matrix.len() as f64;
        if (noise_ratio > 0.90 || unique_ratio > 0.95) && ncol >= 3 {
            drop_col[c] = true;
        }
    }
    let drop_count = drop_col.iter().filter(|&&d| d).count();
    if drop_count == 0 || drop_count == ncol { return false; }

    for line in lines.iter_mut() {
        if line.is_separator || line.text.trim().is_empty() { continue; }
        let chars: Vec<char> = line.text.chars().collect();
        if !gutters.iter().all(|&g| g >= chars.len() || chars[g] == ' ') { continue; }
        let mut fields = Vec::new();
        let mut start = 0;
        for &g in &gutters {
            let field = &line.text[byte_idx(&line.text, start)..byte_idx(&line.text, g)];
            fields.push(field.trim().to_string());
            start = g;
        }
        fields.push(line.text[byte_idx(&line.text, start)..].trim().to_string());
        if fields.len() != ncol { continue; }
        let mut compressed = Vec::new();
        for (i, field) in fields.iter().enumerate() {
            if !drop_col[i] { compressed.push(field.as_str()); }
        }
        line.text = compressed.join(" ");
    }
    true
}

fn byte_idx(s: &str, char_pos: usize) -> usize {
    s.char_indices().nth(char_pos).map(|(i, _)| i).unwrap_or(s.len())
}

fn is_progress_line(text: &str) -> bool {
    let has_pct = text.contains('%');
    let has_bar = text.contains("###")
        || text.contains("===")
        || text.contains("---")
        || text.contains("...")
        || text.contains("██")
        || text.contains("▒▒")
        || text.contains("░░")
        || text.contains("|||");
    has_pct && has_bar
}

fn is_summary_line(text: &str) -> bool {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let has_ok = words.contains(&"ok")
        || words.contains(&"ok.")
        || words.contains(&"ok,")
        || words.contains(&"[ok]");
    let has_not_ok = lower.contains("not ok");
    (has_ok && !has_not_ok)
        || text.starts_with("test result:")
        || text.starts_with("Finished")
        || text.starts_with("Compiling")
        || text.starts_with("Downloaded")
}

/// Detect strategy from buffer of (raw_line, classified_line).
pub fn detect_strategy(buf: &[(String, Line)]) -> CompressionStrategy {
    if detect_json(buf) { return CompressionStrategy::Json; }
    if detect_diff(buf) { return CompressionStrategy::Diff; }
    if detect_tabular(buf) { return CompressionStrategy::Tabular; }
    if detect_prefixed(buf).is_some() { return CompressionStrategy::Prefixed; }
    CompressionStrategy::Normal
}

fn detect_json(buf: &[(String, Line)]) -> bool {
    buf.iter()
        .find(|(_, l)| !l.text.trim().is_empty())
        .map(|(_, l)| {
            let t = l.text.trim();
            t.starts_with('{') || t.starts_with('[')
        })
        .unwrap_or(false)
}

fn detect_diff(buf: &[(String, Line)]) -> bool {
    buf.iter().any(|(_, l)| {
        let t = l.text.trim();
        t.starts_with("@@ -") && t.contains(" +") && t.ends_with("@@")
    })
}

fn detect_tabular(buf: &[(String, Line)]) -> bool {
    let data: Vec<&str> = buf.iter()
        .filter(|(_, l)| !l.text.trim().is_empty() && !l.is_separator)
        .map(|(_, l)| l.text.as_str())
        .collect();
    if data.len() < 4 { return false; }
    let gutters = find_visual_gutters(&data);
    gutters.len() >= 2
}

fn detect_prefixed(buf: &[(String, Line)]) -> Option<String> {
    let prefixes: Vec<Option<String>> = buf.iter()
        .map(|(_, l)| extract_leading_token(&l.text))
        .collect();
    let total = prefixes.iter().filter(|p| p.is_some()).count();
    if total < 5 { return None; }
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for p in prefixes.iter().flatten() {
        *counts.entry(p.clone()).or_insert(0) += 1;
    }
    let (top, count) = counts.into_iter().max_by_key(|(_, c)| *c)?;
    if count as f64 / total as f64 > 0.5 { Some(top) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(text: &str) -> Line {
        Line { text: text.into(), repeat_dist: 0, skeleton_key: 0, is_error: false, is_separator: false, is_progress: false, is_key_value: false, is_summary: false, index: 0 }
    }
    fn ll(text: &str, key: u64) -> Line {
        Line { text: text.into(), repeat_dist: 0, skeleton_key: key, is_error: false, is_separator: false, is_progress: false, is_key_value: false, is_summary: false, index: 0 }
    }
    fn e(text: &str) -> Line {
        Line { text: text.into(), repeat_dist: 0, skeleton_key: 0, is_error: true, is_separator: false, is_progress: false, is_key_value: false, is_summary: false, index: 0 }
    }

    #[test]
    fn skeleton_uuid() { assert_eq!(skeleton("abc 550e8400-e29b-41d4-a716-446655440000 xyz"), "abc {uuid} xyz"); }
    #[test]
    fn skeleton_hex_hash() { assert_eq!(skeleton("commit abcdef1234567890abcdef12"), "commit {hash}"); }
    #[test]
    fn skeleton_version() { assert_eq!(skeleton("rustc 1.72.0"), "rustc {ver}"); }
    #[test]
    fn skeleton_word_num() { assert_eq!(skeleton("build-12345"), "{w}-{n}"); }
    #[test]
    fn skeleton_number() { assert_eq!(skeleton("line 42"), "line {n}"); }
    #[test]
    fn skeleton_progress_bar() { assert_eq!(skeleton("[##########..........]"), "{progress}"); }
    #[test]
    fn skeleton_collapses_whitespace() { assert_eq!(skeleton("hello    world"), "hello world"); }
    #[test]
    fn skeleton_lowercases() { assert_eq!(skeleton("HELLO WORLD"), "hello world"); }
    #[test]
    fn classify_error_detection() {
        for err in &["Error: not found", "error[E0432]", "FAILED: test", "  --> test.rs:42", "error: aborting"] {
            assert!(classify(err)[0].is_error, "should detect error: {}", err);
        }
    }
    #[test]
    fn classify_summary() { assert!(classify("test result: ok. 42 passed; 0 failed")[0].is_summary); }
    #[test]
    fn classify_separator() { assert!(classify("----")[0].is_separator); }
    #[test]
    fn classify_progress() { assert!(classify(" 12% [##########..........]")[0].is_progress); }
    #[test]
    fn classify_repeat_distance() {
        let lines = classify("hello\nworld\nhello\nhello\n");
        assert_eq!(lines[2].repeat_dist, 2);
        assert_eq!(lines[3].repeat_dist, 1);
    }
    #[test]
    fn should_not_drop_error() { assert!(!should_drop(&e("Error: timeout"))); }
    #[test]
    fn should_drop_blank() {
        assert!(should_drop(&l("")));
        assert!(should_drop(&l("   ")));
    }
    #[test]
    fn should_drop_separator() {
        let mut sep = l("----");
        sep.is_separator = true;
        assert!(should_drop(&sep));
    }
    #[test]
    fn should_drop_short() { assert!(should_drop(&l("hi"))); }
    #[test]
    fn groups_empty() { assert_eq!(skeleton_groups(&[]).len(), 0); }
    #[test]
    fn groups_merge_same() {
        let g = skeleton_groups(&[ll("hello", 1), ll("hello", 1)]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].count, 2);
    }
    #[test]
    fn groups_split_different() {
        let g = skeleton_groups(&[ll("hello", 1), ll("world", 2)]);
        assert_eq!(g.len(), 2);
    }
    #[test]
    fn compress_tabular_drops_low_info() {
        let mut lines = vec![
            l("a 1 10"), l("a 2 20"), l("a 3 30"),
            l("a 4 40"), l("a 5 50"),
        ];
        assert!(compress_tabular(&mut lines));
    }
    #[test]
    fn compress_tabular_keeps_last_column() {
        let mut lines = vec![
            l("a 10 ok"), l("a 20 ok"), l("a 30 ok"),
            l("a 40 ok"), l("a 50 ok"),
        ];
        assert!(compress_tabular(&mut lines));
        for line in &lines {
            let fields: Vec<&str> = line.text.split_whitespace().collect();
            assert_eq!(fields.last(), Some(&"ok"));
        }
    }
    #[test]
    fn detect_json_strategy() {
        let buf = vec![("{\"a\": 1}".to_string(), l("{\"a\": 1}"))];
        assert_eq!(detect_strategy(&buf), CompressionStrategy::Json);
    }
    #[test]
    fn detect_diff_strategy() {
        let buf = vec![("@@ -1,3 +1,4 @@".to_string(), l("@@ -1,3 +1,4 @@"))];
        assert_eq!(detect_strategy(&buf), CompressionStrategy::Diff);
    }
    #[test]
    fn detect_normal_strategy() {
        let buf = vec![("hello world".to_string(), l("hello world"))];
        assert_eq!(detect_strategy(&buf), CompressionStrategy::Normal);
    }
}
