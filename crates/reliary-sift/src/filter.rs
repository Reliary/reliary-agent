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

/// Format output using strategy-specific compression.
pub fn format_output(lines: &[Line], strategy: CompressionStrategy) -> String {
    let mut lines = lines.to_vec();
    lines = extract_error_blocks(&lines);
    match strategy {
        CompressionStrategy::Json => format_json(&lines),
        CompressionStrategy::Diff => format_diff(&lines),
        CompressionStrategy::Tabular => format_tabular(&lines),
        CompressionStrategy::Prefixed => format_prefixed(&lines),
        CompressionStrategy::Normal => format_normal(&lines),
    }
}

fn format_json(lines: &[Line]) -> String {
    let joined = lines.iter()
        .filter(|l| !l.text.trim().is_empty() && !l.is_separator && !l.is_progress)
        .map(|l| l.text.as_str())
        .collect::<Vec<&str>>()
        .join("\n");
    let compressed = crate::compress_json::compress_json(&joined);
    if compressed.len() < joined.len() { compressed } else { joined }
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

fn format_tabular(lines: &[Line]) -> String {
    let mut lines = lines.to_vec();
    classify::compress_tabular(&mut lines);
    let groups = collapse_prefix_runs(&classify::skeleton_groups(&lines));
    let mut out = String::new();
    for group in &groups {
        if group.count == 1 { out.push_str(&group.sample); out.push('\n'); }
        else { out.push_str(&group.sample); out.push_str(&format!("  [{}+ more]\n", group.count - 1)); }
    }
    if out.ends_with('\n') { out.pop(); }
    out
}

fn format_prefixed(lines: &[Line]) -> String {
    let groups = collapse_prefix_runs(&classify::skeleton_groups_prefixed(lines));
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

fn format_normal(lines: &[Line]) -> String {
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
    let mut groups = collapse_prefix_runs(&classify::skeleton_groups(&lines));
    // Merge non-consecutive groups sharing the same skeleton_key (shell/build
    // output often interleaves two skeleton variants; collapsing them after the
    // fact restores the "N items matching" compression).
    merge_split_skeleton_groups(&mut groups);
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

fn flush_ok(out: &mut String, count: &mut usize) {
    if *count > 0 { out.push_str(&format!("[{} ok]\n", count)); *count = 0; }
}

/// Merge groups that share a skeleton_key but were split by interleaved lines.
/// For shell/build output where two skeleton variants alternate (e.g. short
/// `Compiling crateN` and longer `Compiling crate-N-extra v0.1.0`), this
/// collapses them into one combined group.
fn merge_split_skeleton_groups(groups: &mut Vec<LineGroup>) {
    if groups.len() < 2 { return; }
    let mut merged: Vec<LineGroup> = Vec::new();
    let mut i = 0;
    while i < groups.len() {
        let key = groups[i].skeleton_key;
        let mut combined_count = 0;
        let mut sample: Option<String> = None;
        let mut first_idx = groups[i].first_idx;
        let mut is_error = false;
        let mut prefixes: Vec<String> = Vec::new();
        while i < groups.len() && groups[i].skeleton_key == key {
            combined_count += groups[i].count;
            if sample.is_none() { sample = Some(groups[i].sample.clone()); }
            if groups[i].first_idx < first_idx { first_idx = groups[i].first_idx; }
            if groups[i].is_error { is_error = true; }
            for p in &groups[i].distinct_prefixes {
                if !prefixes.contains(p) { prefixes.push(p.clone()); }
            }
            i += 1;
        }
        merged.push(LineGroup {
            skeleton_key: key,
            sample: sample.unwrap_or_default(),
            count: combined_count,
            first_idx,
            is_error,
            distinct_prefixes: prefixes,
        });
    }
    *groups = merged;
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
