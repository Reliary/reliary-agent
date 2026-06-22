// Output formatting with strategy-specific compression.
// Ported from original sift's content-aware formatting pipeline.

use crate::classify::{self, Line, LineGroup, CompressionStrategy};

/// Extract multi-line compiler error blocks into single lines.
/// Groups consecutive non-blank lines starting from an error line.
pub fn extract_error_blocks(lines: &[Line]) -> Vec<Line> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].is_error && !lines[i].text.trim().is_empty() {
            let mut merged = String::new();
            let mut first = true;
            while i < lines.len() {
                let t = lines[i].text.trim();
                if t.is_empty() { break; }
                if first { merged.push_str(t); first = false; }
                else { merged.push_str("\n┃ "); merged.push_str(t); }
                i += 1;
            }
            result.push(Line {
                text: merged, repeat_dist: 0, skeleton_key: 0,
                is_error: false, is_separator: false, is_progress: false,
                is_key_value: false, is_summary: false, index: 0,
            });
        } else {
            result.push(lines[i].clone());
            i += 1;
        }
    }
    result
}

/// Collapse runs of singleton groups sharing the same timestamp prefix.
fn collapse_prefix_runs(groups: &[LineGroup]) -> Vec<LineGroup> {
    if groups.is_empty() { return groups.to_vec(); }
    let mut result: Vec<LineGroup> = Vec::new();
    let mut i = 0;
    while i < groups.len() {
        if groups[i].count == 1 && !groups[i].is_error {
            let prefix = extract_timestamp_prefix(&groups[i].sample);
            if !prefix.is_empty() {
                let mut j = i + 1;
                while j < groups.len() && groups[j].count == 1 && !groups[j].is_error {
                    if extract_timestamp_prefix(&groups[j].sample) == prefix { j += 1; }
                    else { break; }
                }
                let run_len = j - i;
                if run_len >= 5 {
                    result.push(LineGroup {
                        skeleton_key: 0,
                        sample: format!("[{} items matching prefix: \"{}\"]", run_len, prefix),
                        count: run_len, first_idx: groups[i].first_idx, is_error: false,
                        distinct_prefixes: vec![],
                    });
                    i = j; continue;
                }
            }
        }
        result.push(groups[i].clone());
        i += 1;
    }
    result
}

fn extract_timestamp_prefix(text: &str) -> String {
    let s = text.trim();
    let bytes = s.as_bytes();
    if s.len() >= 8 && bytes[0].is_ascii_digit() && bytes[1].is_ascii_digit() && bytes[2] == b':' &&
       bytes[3].is_ascii_digit() && bytes[4].is_ascii_digit() && bytes[5] == b':' &&
       bytes[6].is_ascii_digit() && bytes[7].is_ascii_digit() {
        let mut start = 8;
        if s.len() > start + 4 && bytes[start] == b'.' {
            let mut frac = start + 1;
            while frac < s.len() && bytes[frac].is_ascii_digit() { frac += 1; }
            start = frac;
        }
        let mut prefix: String = s[start..].trim().chars().take(30).collect();
        while prefix.ends_with(|c: char| c.is_ascii_digit() || c == ' ' || c == ':') { prefix.pop(); }
        prefix
    } else {
        String::new()
    }
}

/// Decide whether aggressive skeleton grouping should be used for these lines.
/// Aggressive is enabled when ≥80% of non-blank lines share the same template
/// AND those lines have similar lengths (within 30%).
///
/// This catches cargo "Compiling X vY" output (≥80% share, similar length)
/// while rejecting file reads where similar-looking function signatures are
/// only a small fraction of total lines.
pub fn should_use_aggressive(lines: &[Line]) -> bool {
    let non_blank: Vec<&Line> = lines.iter()
        .filter(|l| !l.text.trim().is_empty())
        .collect();
    if non_blank.len() < 5 {
        return false;
    }
    let mut counts: std::collections::HashMap<String, (usize, Vec<usize>)> = std::collections::HashMap::new();
    for l in &non_blank {
        let s = classify::aggressive_skeleton(&l.text);
        let entry = counts.entry(s).or_insert((0, Vec::new()));
        entry.0 += 1;
        entry.1.push(l.text.len());
    }
    let (count, lens) = counts.values().max_by_key(|(c, _)| *c).cloned().unwrap_or((0, vec![]));
    // Require ≥80% concentration of single template
    if count * 5 < non_blank.len() * 4 { return false; }
    let min_len = lens.iter().min().copied().unwrap_or(0);
    let max_len = lens.iter().max().copied().unwrap_or(0);
    min_len > 0 && min_len * 10 >= max_len * 7
}

/// Format output using strategy-specific compression.
pub fn format_output(lines: &[Line], strategy: CompressionStrategy) -> String {
    let mut lines = lines.to_vec();
    lines = extract_error_blocks(&lines);
    let use_aggressive = should_use_aggressive(&lines);
    match strategy {
        CompressionStrategy::Json => format_json(&lines),
        CompressionStrategy::Diff => format_diff(&lines),
        CompressionStrategy::Tabular => format_tabular_with(&lines, use_aggressive),
        CompressionStrategy::Prefixed => format_prefixed_with(&lines, use_aggressive),
        CompressionStrategy::Normal => format_normal_with(&lines, use_aggressive),
    }
}

fn format_json(lines: &[Line]) -> String {
    lines.iter()
        .filter(|l| !l.text.trim().is_empty() && !l.is_separator && !l.is_progress)
        .map(|l| l.text.as_str())
        .collect::<Vec<&str>>()
        .join("\n")
}

fn format_diff(lines: &[Line]) -> String {
    let mut out = String::new();
    let mut context_run: Vec<&Line> = Vec::new();
    for line in lines {
        let trimmed = line.text.trim();
        let is_hunk_header = trimmed.starts_with("@@ -") && trimmed.contains(" +") && trimmed.ends_with("@@");
        let is_file_header = trimmed.starts_with("--- ") || trimmed.starts_with("+++ ");
        if is_hunk_header || is_file_header {
            if context_run.len() >= 3 { out.push_str(&format!("[{} identical context lines]\n", context_run.len())); }
            else { for l in &context_run { out.push_str(&l.text); out.push('\n'); } }
            context_run.clear();
            out.push_str(&line.text); out.push('\n');
        } else if trimmed.is_empty() || line.is_separator {
            if context_run.len() >= 3 { out.push_str(&format!("[{} identical context lines]\n", context_run.len())); }
            else { for l in &context_run { out.push_str(&l.text); out.push('\n'); } }
            context_run.clear();
            if !trimmed.is_empty() { out.push_str(&line.text); out.push('\n'); }
        } else {
            if !context_run.is_empty() && context_run.last().unwrap().text == line.text {
                context_run.push(line);
            } else {
                if context_run.len() >= 3 { out.push_str(&format!("[{} identical context lines]\n", context_run.len())); }
                else { for l in &context_run { out.push_str(&l.text); out.push('\n'); } }
                context_run.clear();
                context_run.push(line);
            }
        }
    }
    if context_run.len() >= 3 { out.push_str(&format!("[{} identical context lines]\n", context_run.len())); }
    else { for l in &context_run { out.push_str(&l.text); out.push('\n'); } }
    out
}

fn format_tabular_with(lines: &[Line], use_aggressive: bool) -> String {
    let mut lines = lines.to_vec();
    classify::compress_tabular(&mut lines);
    let groups = if use_aggressive {
        collapse_prefix_runs(&aggressive_skeleton_groups(&lines))
    } else {
        collapse_prefix_runs(&classify::skeleton_groups(&lines))
    };
    let mut out = String::new();
    for group in &groups {
        if group.count == 1 { out.push_str(&group.sample); out.push('\n'); }
        else { out.push_str(&group.sample); out.push_str(&format!("  [{}+ more]\n", group.count - 1)); }
    }
    if out.ends_with('\n') { out.pop(); }
    out
}

fn format_prefixed_with(lines: &[Line], use_aggressive: bool) -> String {
    let groups = if use_aggressive {
        collapse_prefix_runs(&aggressive_skeleton_groups(lines))
    } else {
        collapse_prefix_runs(&classify::skeleton_groups_prefixed(lines))
    };
    let mut out = String::new();
    let mut ok_count: usize = 0;
    for group in &groups {
        let is_success = !group.sample.contains("FAIL") && !group.sample.contains("Error") && !group.sample.starts_with("error");
        let lower = group.sample.to_lowercase();
        if is_success && (lower.contains("... ok") || lower.starts_with("test result:")) {
            ok_count += group.count;
            continue;
        }
        flush_ok(&mut out, &mut ok_count);
        if group.count == 1 { out.push_str(&group.sample); out.push('\n'); }
        else if !group.distinct_prefixes.is_empty() {
            let show = group.distinct_prefixes.len().min(5);
            let rest = group.distinct_prefixes.len().saturating_sub(show);
            out.push_str(&group.sample);
            out.push_str(&format!("\n  [{}+ more in {} files: ", group.count - 1, group.distinct_prefixes.len()));
            for p in group.distinct_prefixes.iter().take(show) { out.push_str(p); out.push(' '); }
            if rest > 0 { out.push_str(&format!("+{} more", rest)); }
            out.push_str("]\n");
        } else {
            out.push_str(&group.sample);
            out.push_str(&format!("  [{}+ more structurally similar]\n", group.count - 1));
        }
    }
    flush_ok(&mut out, &mut ok_count);
    if out.ends_with('\n') { out.pop(); }
    out
}

fn format_normal_with(lines: &[Line], use_aggressive: bool) -> String {
    let mut lines = lines.to_vec();
    let is_repetitive = {
        let high = lines.len() / 2;
        let mut skels = std::collections::HashSet::new();
        for l in &lines {
            if l.text.trim().is_empty() || l.is_separator || l.is_progress { continue; }
            skels.insert(l.skeleton_key);
        }
        skels.len() <= high
    };
    if !is_repetitive { classify::compress_tabular(&mut lines); }

    let groups = if use_aggressive {
        collapse_prefix_runs(&aggressive_skeleton_groups(&lines))
    } else {
        collapse_prefix_runs(&classify::skeleton_groups(&lines))
    };
    let mut out = String::new();
    let mut ok_count: usize = 0;
    for group in &groups {
        let is_success = !group.sample.contains("FAIL") && !group.sample.contains("Error") && !group.sample.starts_with("error");
        let lower = group.sample.to_lowercase();
        if is_success && (lower.contains("... ok") || lower.starts_with("test result:")) {
            ok_count += group.count;
            continue;
        }
        flush_ok(&mut out, &mut ok_count);
        if group.count == 1 { out.push_str(&group.sample); out.push('\n'); }
        else if !group.distinct_prefixes.is_empty() && group.distinct_prefixes.len() >= group.count / 2 {
            let show = group.distinct_prefixes.len().min(5);
            let rest = group.distinct_prefixes.len().saturating_sub(show);
            out.push_str(&group.sample);
            out.push_str(&format!("\n  [{}+ more in {} files: ", group.count - 1, group.distinct_prefixes.len()));
            for p in group.distinct_prefixes.iter().take(show) { out.push_str(p); out.push(' '); }
            if rest > 0 { out.push_str(&format!("+{} more", rest)); }
            out.push_str("]\n");
        } else {
            out.push_str(&group.sample);
            out.push_str(&format!("  [{}+ more structurally similar]\n", group.count - 1));
        }
    }
    flush_ok(&mut out, &mut ok_count);
    if out.ends_with('\n') { out.pop(); }
    out
}

/// Group lines by aggressive skeleton, then collapse runs into LineGroups.
/// Mirrors classify::skeleton_groups but uses aggressive_skeleton for grouping.
/// Error lines (is_error) and progress lines (is_progress) are preserved verbatim.
fn aggressive_skeleton_groups(lines: &[Line]) -> Vec<classify::LineGroup> {
    use std::collections::HashMap;
    let mut groups_map: HashMap<String, classify::LineGroup> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        // Preserve error and progress lines verbatim — they are signal
        if line.is_error || line.is_progress {
            let key = format!("__single_error_{}", idx);
            groups_map.insert(key.clone(), classify::LineGroup {
                skeleton_key: 0,
                sample: line.text.clone(),
                count: 1,
                first_idx: idx,
                is_error: line.is_error,
                distinct_prefixes: vec![],
            });
            order.push(key);
            continue;
        }

        let agg = classify::aggressive_skeleton(&line.text);
        if agg.is_empty() {
            let key = format!("__blank_{}", idx);
            groups_map.insert(key.clone(), classify::LineGroup {
                skeleton_key: 0,
                sample: line.text.clone(),
                count: 1,
                first_idx: idx,
                is_error: false,
                distinct_prefixes: vec![],
            });
            order.push(key);
            continue;
        }

        if let Some(existing) = groups_map.get_mut(&agg) {
            existing.count += 1;
        } else {
            groups_map.insert(agg.clone(), classify::LineGroup {
                skeleton_key: 0,
                sample: line.text.clone(),
                count: 1,
                first_idx: idx,
                is_error: line.is_error,
                distinct_prefixes: vec![],
            });
            order.push(agg);
        }
    }

    order.into_iter().filter_map(|k| groups_map.remove(&k)).collect()
}

fn flush_ok(out: &mut String, count: &mut usize) {
    if *count > 0 { out.push_str(&format!("[{} ok]\n", count)); *count = 0; }
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
    fn extract_error_blocks_basic() {
        let mut err = e("Error: something");
        err.text = "Error: something".to_string();
        let lines = vec![l("normal"), err, l(""), l("after")];
        let result = extract_error_blocks(&lines);
        // Error line is merged into one block. Blank and normal lines pass through.
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn format_output_groups() {
        let out = format_output(&[ll("hello", 1), ll("world", 2)], CompressionStrategy::Normal);
        assert!(out.contains("hello") && out.contains("world"));
    }

    #[test]
    fn format_output_empty() {
        assert_eq!(format_output(&[], CompressionStrategy::Normal), "");
    }

    #[test]
    fn format_output_ok_collapsed() {
        let mut lines = vec![
            ll("test_case_a ... ok", 1),
            ll("test_case_b ... ok", 1),
            ll("test_case_c ... ok", 1),
            ll("Error: fail", 2),
        ];
        lines[3].is_error = true;
        let out = format_output(&lines, CompressionStrategy::Normal);
        assert!(out.contains("Error"));
        assert!(out.contains("[3 ok]") || out.contains("[3+"));
    }
}
