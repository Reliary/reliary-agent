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
        let is_separator = is_decorative_separator(&clean) || (
            !clean.trim().is_empty()
            && clean.trim().chars().all(|c| c == '-' || c == '=' || c == '.' || c == '_' || c == '*')
        );
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
// P5: memchr-based fast-path. Most output is plain text — copy in bulk
// between ESC sequences instead of per-character push.
pub fn strip_all_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        // Bulk-copy the next chunk of non-ESC text.
        match memchr::memchr(b'\x1b', &bytes[i..]) {
            None => {
                out.push_str(&text[i..]);
                break;
            }
            Some(rel) => {
                let abs = i + rel;
                out.push_str(&text[i..abs]);
                i = abs + 1; // skip ESC
                // Skip the ANSI command sequence.
                if i < bytes.len() {
                    let cmd = bytes[i];
                    i += 1;
                    match cmd {
                        b'[' => {
                            while i < bytes.len() {
                                let b = bytes[i];
                                if b.is_ascii_alphabetic() || b == b'~' { i += 1; break; }
                                i += 1;
                            }
                        }
                        b']' | b'P' | b'X' | b'^' | b'_' => {
                            while i < bytes.len() {
                                match bytes[i] {
                                    0x07 => { i += 1; break; }
                                    0x1b => { i += 1; if i < bytes.len() { i += 1; } break; }
                                    _ => { i += 1; }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
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

        // 0b. Timestamp HH:MM:SS or HH:MM:SS.mmm
        // P13: detect at start of buffer or after whitespace. If found, emit {time}.
        if (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'[' || bytes[i - 1] == b'(') && i + 8 < len
            && bytes[i].is_ascii_digit() && bytes[i+1].is_ascii_digit()
            && bytes[i+2] == b':' && bytes[i+3].is_ascii_digit() && bytes[i+4].is_ascii_digit()
            && bytes[i+5] == b':' && bytes[i+6].is_ascii_digit() && bytes[i+7].is_ascii_digit()
        {
            // Confirm char after (end of seconds) is non-digit OR a . with more digits
            let after = if i + 8 < len {
                Some(bytes[i + 8])
            } else { None };
            let valid_end = match after {
                None => true,
                Some(b'.') => i + 11 < len && bytes[i+9].is_ascii_digit() && bytes[i+10].is_ascii_digit() && bytes[i+11].is_ascii_digit(),
                Some(b' ') | Some(b'\t') | Some(b']') | Some(b')') | Some(b',') | Some(b'Z') | Some(b'+') | Some(b'-') | Some(b'\n') => true,
                _ => false,
            };
            if valid_end {
                emit_str!("{time}");
                i += 8;
                if after == Some(b'.') { i += 4; }
                continue;
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
            while he < len && bytes[he].is_ascii_hexdigit() { he += 1; }
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

/// Aggressive skeleton: same structural normalization as `skeleton` but ALSO
/// replaces every word token with `{w}`. Used to detect template-filled output
/// (cargo "Compiling X", pytest "test_X ok", shell progress) where each line
/// differs only in token values but shares the same structural template.
///
/// Grammar-free: uses the same byte DFA as `skeleton`, no regex, no language
/// detection. Pure structural template extraction.
pub fn aggressive_skeleton(line: &str) -> String {
    let s = line.trim();
    if s.is_empty() { return String::new(); }
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut result = String::with_capacity(len.min(256));
    let mut last_space = false;

    macro_rules! emit_str {
        ($s:expr) => { last_space = false; result.push_str($s); };
    }

    while i < len {
        // 0. Progress bar (same as skeleton)
        if bytes[i] == b'[' && i + 3 < len {
            if let Some(cls) = s[i..].find(']') {
                let inner = &s[i+1..i+cls];
                if !inner.trim().is_empty() && inner.chars().all(|c| c == '#' || c == '.' || c == '=' || c == '>' || c == '-' || c == '_' || c.is_whitespace()) {
                    emit_str!("{progress}"); i += cls + 1; continue;
                }
            }
        }

        // 1. UUID (same as skeleton)
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

        // 2. word-NNN → {w}-{n}
        if bd(i) && is_alpha(i) {
            let mut we = i;
            while we < len && is_alpha(we) { we += 1; }
            if we > i + 2 && we < len && bytes[we] == b'-' && we + 1 < len && is_digit(we + 1) {
                let mut dne = we + 1;
                while dne < len && is_digit(dne) { dne += 1; }
                if dne > we + 1 { emit_str!("{w}-{n}"); i = dne; continue; }
            }
        }

        // 3. Hex hash → {hash}
        if bd(i) && bytes[i].is_ascii_hexdigit() {
            let mut he = i;
            while he < len && bytes[he].is_ascii_hexdigit() { he += 1; }
            if he - i >= 7 && he - i <= 40 && (he >= len || !is_alnum(he)) {
                emit_str!("{hash}"); i = he; continue;
            }
        }

        // 4. Version X.Y.Z → {ver}
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

        // 5. Number → {n}
        if bd(i) && is_digit(i) {
            let ne = num_end(i, bytes);
            if ne > i && (ne >= len || !is_alnum(ne)) {
                emit_str!("{n}"); i = ne; continue;
            }
        }

        // 6. Pure-alpha word → {w}
        // KEY DIFFERENCE from skeleton: this collapses every alpha word to {w},
        // so "Compiling serde" and "Compiling tokio" share the same template.
        // Compound tokens like "v1.0.200" get split: "v"→{w}, "1.0.200"→{ver}.
        // Tokens like "E0308" get split: "E"→{w}, "0308"→{n}. Both versions
        // share aggressive skeleton (both become {w}{n} after rules 6+5).
        if bd(i) && is_alpha(i) {
            let mut we = i;
            while we < len && is_alpha(we) { we += 1; }
            if we > i {
                // Single-letter words: keep verbatim (structurals like 'a', 'I')
                if we - i == 1 {
                    let ch = s[i..].chars().next().unwrap_or(' ');
                    result.push(ch);
                    last_space = false;
                } else {
                    emit_str!("{w}");
                }
                i = we;
                continue;
            }
        }

        // 7. Pure-alphanumeric run (e.g., identifiers like "abc123") → {w}
        // Catches tokens the alpha-only rule missed (start with digit).
        if bd(i) && is_alnum(i) {
            let mut we = i;
            while we < len && is_alnum(we) { we += 1; }
            if we > i {
                emit_str!("{w}");
                i = we;
                continue;
            }
        }

        // 8. Regular char
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

fn skeleton_hash(text: &str) -> u64 {
    let s = skeleton(text);
    if s.is_empty() { return 0; }
    let mut h: u64 = 5381;
    {
            let bytes = s.as_bytes();
            let mut i = 0;
            while i + 8 <= bytes.len() {
                let chunk = u64::from_le_bytes(bytes[i..i+8].try_into().unwrap());
                h = h.wrapping_mul(0x100000001b3).wrapping_add(chunk);
                i += 8;
            }
            while i < bytes.len() { h = h.wrapping_mul(33).wrapping_add(bytes[i] as u64); i += 1; }
        }
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

/// V14: Detect "decorative separator" lines like `=== title ===` that have
/// ≥3 separator chars at start AND end with content in the middle. These
/// are common in pytest/jest output and waste 60+ chars each.
pub fn is_decorative_separator(text: &str) -> bool {
    if text.len() < 10 { return false; }
    let bytes = text.as_bytes();
    let mut start_sep = 0;
    while start_sep < bytes.len() && matches!(bytes[start_sep], b'=' | b'-' | b'_' | b'~') {
        start_sep += 1;
    }
    if start_sep < 3 { return false; }
    let mut end_sep = bytes.len();
    while end_sep > start_sep && matches!(bytes[end_sep - 1], b'=' | b'-' | b'_' | b'~') {
        end_sep -= 1;
    }
    if bytes.len() - end_sep < 3 { return false; }
    let sep_count = start_sep + (bytes.len() - end_sep);
    sep_count * 10 >= bytes.len()
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
                {
            let bytes = p.as_bytes();
            let mut i = 0;
            while i + 8 <= bytes.len() {
                let chunk = u64::from_le_bytes(bytes[i..i+8].try_into().unwrap());
                h = h.wrapping_mul(0x100000001b3).wrapping_add(chunk);
                i += 8;
            }
            while i < bytes.len() { h = h.wrapping_mul(33).wrapping_add(bytes[i] as u64); i += 1; }
        }
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

/// A cluster of lines sharing an identical aggressive skeleton.
/// Used by `find_clusters` and `find_clusters_global` for template-based compression.
pub struct TemplateCluster {
    pub skeleton_key: u64,
    pub skeleton: String,
    pub sample: String,
    pub lines: Vec<String>,
    pub indices: Vec<usize>,
}

/// Hash of `aggressive_skeleton(line)`. Blank lines return 0.
fn aggressive_skeleton_hash(line: &str) -> u64 {
    let s = aggressive_skeleton(line);
    if s.is_empty() { return 0; }
    let mut h: u64 = 5381;
    {
            let bytes = s.as_bytes();
            let mut i = 0;
            while i + 8 <= bytes.len() {
                let chunk = u64::from_le_bytes(bytes[i..i+8].try_into().unwrap());
                h = h.wrapping_mul(0x100000001b3).wrapping_add(chunk);
                i += 8;
            }
            while i < bytes.len() { h = h.wrapping_mul(33).wrapping_add(bytes[i] as u64); i += 1; }
        }
    h
}

/// Find runs of `min_run`+ consecutive lines sharing an identical aggressive skeleton.
///
/// Uses `aggressive_skeleton` for clustering — collapses every word to `{w}`,
/// so 'Compiling serde' and 'Compiling tokio' cluster together (template `{w} {w}`).
/// The sample line is kept verbatim so rendered templates show real code.
///
/// **Convenience wrapper** `find_clusters_with_default(source)` uses `min_run = 3`,
/// matching the Python port's default. The required-arg version is `find_clusters(source, min_run)`.
pub fn find_clusters(source: &str, min_run: usize) -> Vec<TemplateCluster> {
    find_clusters_impl(source, min_run)
}

/// Default-parameter version of `find_clusters` matching the Python port's API.
/// `min_run = 3` — a cluster needs at least 3 consecutive same-skeleton lines.
pub fn find_clusters_with_default(source: &str) -> Vec<TemplateCluster> {
    find_clusters_impl(source, 3)
}

fn find_clusters_impl(source: &str, min_run: usize) -> Vec<TemplateCluster> {
    let lines: Vec<&str> = source.lines().collect();
    if lines.len() < min_run { return Vec::new(); }

    let skels: Vec<u64> = lines.iter().map(|l| aggressive_skeleton_hash(l)).collect();
    let mut clusters: Vec<TemplateCluster> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if skels[i] == 0 { i += 1; continue; }
        let mut j = i + 1;
        while j < lines.len() && skels[j] == skels[i] { j += 1; }
        let run = j - i;
        if run >= min_run {
            clusters.push(TemplateCluster {
                skeleton_key: skels[i],
                skeleton: aggressive_skeleton(lines[i]),
                sample: lines[i].to_string(),
                lines: lines[i..j].iter().map(|s| s.to_string()).collect(),
                indices: (i..j).collect(),
            });
        }
        i = j;
    }
    clusters
}

/// Find ALL lines sharing a skeleton, anywhere in the source (non-consecutive).
///
/// Catches the dominant form of codebase repetition: same structural pattern
/// appearing many times scattered across a file. Returns one cluster per
/// distinct skeleton with `min_count`+ occurrences. Lines shorter than
/// `min_line_len` chars are ignored.
///
/// **Convenience wrapper** `find_clusters_global_with_default(source)` uses
/// `min_count = 3, min_line_len = 12`, matching the Python port's defaults.
pub fn find_clusters_global(source: &str, min_count: usize, min_line_len: usize) -> Vec<TemplateCluster> {
    find_clusters_global_impl(source, min_count, min_line_len)
}

/// Default-parameter version of `find_clusters_global` matching the Python port's API.
/// `min_count = 3` — need at least 3 lines sharing the same skeleton.
/// `min_line_len = 12` — ignore short lines (boilerplate, brackets, etc.).
pub fn find_clusters_global_with_default(source: &str) -> Vec<TemplateCluster> {
    find_clusters_global_impl(source, 3, 12)
}

fn find_clusters_global_impl(source: &str, min_count: usize, min_line_len: usize) -> Vec<TemplateCluster> {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() { return Vec::new(); }

    // V14: BTreeMap for deterministic iteration order. Identical input
    // produces identical output bytes on every run — cache-safe.
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    let mut skeletons_cache: BTreeMap<u64, String> = BTreeMap::new();

    for (i, ln) in lines.iter().enumerate() {
        if ln.trim().len() < min_line_len { continue; }
        let h = aggressive_skeleton_hash(ln);
        if h == 0 { continue; }
        groups.entry(h).or_default().push(i);
        skeletons_cache.entry(h).or_insert_with(|| aggressive_skeleton(ln));
    }

    let mut clusters: Vec<TemplateCluster> = Vec::new();
    for (h, indices) in &groups {
        if indices.len() < min_count { continue; }
        let sample_idx = indices[0];
        clusters.push(TemplateCluster {
            skeleton_key: *h,
            skeleton: skeletons_cache[h].clone(),
            sample: lines[sample_idx].to_string(),
            lines: indices.iter().map(|&i| lines[i].to_string()).collect(),
            indices: indices.clone(),
        });
    }

    clusters.sort_by_key(|b| std::cmp::Reverse(b.indices.len()));
    clusters
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
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
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

    #[test]
    fn find_clusters_default_uses_min_run_3() {
        // 3 consecutive same-skeleton lines — default min_run=3 should form a cluster.
        // Use 2+ letter words since aggressive_skeleton keeps single-letter words verbatim.
        let src = "Compiling serde\nCompiling tokio\nCompiling hyper";
        let clusters = find_clusters_with_default(src);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].lines.len(), 3);
    }

    #[test]
    fn find_clusters_explicit_min_run_overrides_default() {
        // Same input, but explicit min_run=4 should NOT form a cluster (only 3 lines)
        let src = "Compiling a\nCompiling b\nCompiling c";
        let clusters = find_clusters(src, 4);
        assert_eq!(clusters.len(), 0);
    }

    #[test]
    fn find_clusters_global_default_uses_min_count_3_min_line_len_12() {
        // 3 occurrences of the same skeleton, all >= 12 chars — default should find them
        let src = "Compiling serde v1.0\nwarning unused\nCompiling tokio v2.0\nnote helpful\nCompiling hyper v3.0\nerror missing";
        let clusters = find_clusters_global_with_default(src);
        // All 3 "Compiling X vN" lines share aggressive skeleton "{w} {w} {ver}"
        assert!(!clusters.is_empty());
    }

    #[test]
    fn find_clusters_global_explicit_args_override() {
        // min_count=5 should NOT match 3 occurrences
        let src = "Compiling serde v1.0\nCompiling tokio v2.0\nCompiling hyper v3.0";
        let clusters = find_clusters_global(src, 5, 1);
        assert!(clusters.is_empty() || clusters.iter().all(|c| c.lines.len() >= 5));
    }
}
