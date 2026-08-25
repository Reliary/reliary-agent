//! Collapse repetitive command-output patterns.
use crate::classify::*;

/// Compress command output: classify + collapse runs + format.
pub fn compress_output(text: &str) -> String {
    if text.len() <= 200 {
        return text.to_string();
    }

    let lines = classify_output(text);
    let total = lines.len();
    // V14: lowered from 15 → 8 lines. Real cargo output has ~15 lines
    // (12 Compiling + Finished + Running + test result). The 15-line
    // cutoff excluded most real command output.
    if total <= 8 {
        return text.to_string();
    }

    // Phase 1: Extract error blocks (merge adjacent errors)
    let merged = merge_error_blocks(&lines);

    // Phase 1b (V14): Global prefix collapse — collect ALL "Compiling X" lines
    // across the whole output, not just consecutive runs. Cargo/pytest output
    // interleaves progress lines with summary lines.
    let merged = collapse_prefix_global(&merged);

    // Phase 1c (V14): Strip decorative separator lines like `=== title ===`
    // down to just the title. Pytest output uses these extensively.
    let merged = strip_decorative_separators(merged);

    // Phase 2: Collapse prefix runs (repeated Compiling/Checking lines)
    let collapsed = collapse_prefix_runs(&merged);

    // Phase 3: Collapse OK runs (consecutive passing test lines)
    let result = collapse_ok_lines(&collapsed);

    // Phase 4: Blank line collapse + join
    let final_result = collapse_blanks(&result);

    if final_result.len() < text.len() {
        final_result
    } else {
        text.to_string()
    }
}

/// Merge adjacent error lines into blocks.
/// Preserves the first error line (diagnostic), summarizes the rest.
fn merge_error_blocks(lines: &[OutputLine]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut in_block = false;
    let mut block_lines: Vec<String> = Vec::new();

    for line in lines {
        let is_error = line.line_type == OutputLineType::Error
            || line.line_type == OutputLineType::Warning
            || line.line_type == OutputLineType::Summary
                && line.text.contains("  --> ");
        if is_error {
            in_block = true;
            block_lines.push(line.text.clone());
        } else {
            if in_block && block_lines.len() > 1 {
                // Preserve first error line, summarize rest
                result.push(block_lines[0].clone());
                result.push(format!("  [error: {} additional lines]", block_lines.len() - 1));
            } else if in_block && block_lines.len() == 1 {
                result.push(block_lines[0].clone());
            }
            in_block = false;
            block_lines.clear();
            result.push(line.text.clone());
        }
    }
    if in_block && block_lines.len() > 1 {
        result.push(block_lines[0].clone());
        result.push(format!("  [error: {} additional lines]", block_lines.len() - 1));
    } else if in_block && block_lines.len() == 1 {
        result.push(block_lines[0].clone());
    }

    result
}

/// Collapse 3+ consecutive lines sharing the same compilation/checking prefix.
fn collapse_prefix_runs(lines: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();

        let is_progress = trimmed.starts_with("Compiling")
            || trimmed.starts_with("Checking")
            || trimmed.starts_with("Building")
            || trimmed.starts_with("Linking")
            || trimmed.starts_with("Running");

        if is_progress {
            let prefix = trimmed.split_whitespace().next().unwrap_or("").to_string();
            let mut count = 1;
            let mut j = i + 1;
            while j < lines.len() {
                let nt = lines[j].trim();
                if nt.split_whitespace().next().unwrap_or("") == prefix {
                    count += 1;
                    j += 1;
                } else {
                    break;
                }
            }
            if count >= 3 {
                result.push(format!("[{} {} ...] ({} lines)", count, prefix, count));
                i = j;
                continue;
            }
        }

        let is_ok = trimmed.contains("... ok") || trimmed == "ok";
        if is_ok {
            let mut count = 1;
            let mut j = i + 1;
            while j < lines.len() {
                let nt = lines[j].trim();
                if nt.contains("... ok") || nt == "ok" {
                    count += 1;
                    j += 1;
                } else {
                    break;
                }
            }
            if count >= 3 {
                result.push(format!("[{} ok]", count));
                i = j;
                continue;
            }
        }

        result.push(lines[i].clone());
        i += 1;
    }
    result
}

/// Collapse consecutive ok lines.
fn collapse_ok_lines(lines: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed == "ok" || trimmed.ends_with("... ok") {
            let mut count = 1;
            let mut j = i + 1;
            while j < lines.len() {
                let nt = lines[j].trim();
                if nt == "ok" || nt.ends_with("... ok") {
                    count += 1;
                    j += 1;
                } else {
                    break;
                }
            }
            if count >= 3 {
                result.push(format!("[{} ok]", count));
                i = j;
                continue;
            }
        }
        result.push(lines[i].clone());
        i += 1;
    }
    result
}

/// Collapse consecutive blank lines to single blank.
fn collapse_blanks(lines: &[String]) -> String {
    let mut result = String::new();
    let mut prev_blank = false;
    for line in lines {
        let is_blank = line.trim().is_empty();
        if is_blank && prev_blank { continue; }
        if !result.is_empty() { result.push('\n'); }
        result.push_str(line);
        prev_blank = is_blank;
    }
    result
}

/// V14: Collapse ALL lines sharing a prefix keyword (e.g., "Compiling",
/// "Checking"), not just consecutive ones. Real cargo/pytest output
/// interleaves "Compiling X" with "Finished" / "Running" / "test result:",
/// so the consecutive-only variant misses them. Grammar-free — prefix
/// detection is universal across build tools (cargo, npm, pip, go, etc.).
fn collapse_prefix_global(lines: &[String]) -> Vec<String> {
    // Group non-progress lines by leading word. Progress-prefixed lines
    // (Compiling/Checking/Building/Linking/Running) are pooled and replaced
    // with a single summary line at the end.
    let prefixes = [
        "Compiling", "Checking", "Building", "Linking", "Running",
        "modified:", "new file:", "deleted:", "renamed:",
        "added", "changed", "removed",
        "Downloading", "Installing", "Installed",
        "Successfully",
    ];
    let mut kept: Vec<String> = Vec::new();
    let mut progress_counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();

    for line in lines {
        let trimmed = line.trim();
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        if prefixes.contains(&first_word) && trimmed.len() > first_word.len() + 2 {
            *progress_counts.entry(first_word).or_insert(0) += 1;
        } else {
            kept.push(line.clone());
        }
    }
    // Emit progress summaries at the top so they're visible.
    let mut summaries: Vec<String> = Vec::new();
    for (prefix, count) in &progress_counts {
        if *count >= 2 {
            summaries.push(format!("[{} {} ...] ({} lines)", count, prefix, count));
        }
    }
    summaries.extend(kept);
    summaries
}

/// V14: Strip decorative separator lines like `=== test session starts ===`
/// down to just the inner content. These lines waste 60-80 chars each but
/// carry only their embedded text. Grammar-free: pure character classification.
fn strip_decorative_separators(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| {
        let trimmed = line.trim();
        if trimmed.len() < 10 {
            return line;
        }
        // Count separator chars (= - _ ~) at start and end.
        let bytes = trimmed.as_bytes();
        let mut start_sep = 0;
        while start_sep < bytes.len() && (bytes[start_sep] == b'=' || bytes[start_sep] == b'-' || bytes[start_sep] == b'_' || bytes[start_sep] == b'~') {
            start_sep += 1;
        }
        let mut end_sep = bytes.len();
        while end_sep > start_sep && (bytes[end_sep - 1] == b'=' || bytes[end_sep - 1] == b'-' || bytes[end_sep - 1] == b'_' || bytes[end_sep - 1] == b'~') {
            end_sep -= 1;
        }
        // Need ≥3 leading AND ≥3 trailing separator chars AND ≥10% of total length.
        let sep_count = start_sep + (bytes.len() - end_sep);
        if start_sep >= 3 && (bytes.len() - end_sep) >= 3 && (sep_count * 10 >= bytes.len()) {
            // Strip leading/trailing separator chars; trim whitespace from inner.
            let inner = trimmed[start_sep..end_sep].trim();
            if inner.is_empty() {
                String::new()
            } else {
                format!("[ {} ]", inner)
            }
        } else {
            line
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collapse_ok_lines_basic() {
        let input = vec![
            "test parse_empty ... ok".to_string(),
            "test parse_single ... ok".to_string(),
            "test parse_multiple ... ok".to_string(),
        ];
        let r = collapse_ok_lines(&input);
        assert_eq!(r.len(), 1);
        assert!(r[0].contains("3 ok"));
    }

    #[test]
    fn test_collapse_prefix_runs_compiling() {
        let input = vec![
            "   Compiling foo v0.1.0".to_string(),
            "   Compiling bar v0.2.0".to_string(),
            "   Compiling baz v0.3.0".to_string(),
        ];
        let r = collapse_prefix_runs(&input);
        assert_eq!(r.len(), 1);
        assert!(r[0].contains("3 Compiling"));
    }

    #[test]
    fn test_compress_output_cargo_output() {
        let mut text = String::new();
        for i in 0..10 {
            text.push_str(&format!("   Compiling crate{} v0.1.0\n", i));
        }
        text.push_str("    Finished dev\n");
        for i in 0..8 {
            text.push_str(&format!("test test_{} ... ok\n", i));
        }
        text.push_str("test result: ok. 8 passed\n");

        let compressed = compress_output(&text);
        assert!(compressed.len() < text.len(), "should compress: {} < {}", compressed.len(), text.len());
        assert!(compressed.contains("Compiling"), "should mention compiling");
    }

    #[test]
    fn test_compress_output_short_pass_through() {
        let short = "hello world";
        assert_eq!(compress_output(short), short);
    }

    #[test]
    fn test_compress_output_preserves_errors() {
        let mut text = String::new();
        for i in 0..30 {
            text.push_str(&format!("   Compiling crate{} v0.1.0\n", i));
        }
        text.push_str("test test_error ... FAILED\n");
        text.push_str("test result: FAILED. 30 passed, 1 failed\n");
        text.push_str("error[E0308]: mismatched types\n");
        text.push_str("  --> src/lib.rs:47\n");
        text.push_str("   = help: use to_string()\n");

        let compressed = compress_output(&text);
        assert!(compressed.contains("FAILED"), "FAILED should survive: {}", compressed);
        assert!(compressed.contains("E0308"), "E0308 should survive: {}", compressed);
    }
}

    #[test]
    fn bench_compression_ratios() {
        // Cargo build output (25 repeated Compiling lines)
        let mut cargo = String::new();
        for i in 0..25 {
            cargo.push_str(&format!("   Compiling crate{} v0.1.0 (build/{}-abc)\n", i, i));
        }
        cargo.push_str("    Finished dev [unoptimized + debuginfo] in 2.34s\n");

        // Test output (20 ok + 1 FAILED + error block)
        let mut test = String::new();
        for i in 0..20 {
            test.push_str(&format!("test test_{} ... ok\n", i));
        }
        test.push_str("test test_error ... FAILED\n");
        test.push_str("test result: FAILED. 20 passed, 1 failed\n");
        test.push_str("error[E0308]: mismatched types\n");
        test.push_str("  --> src/lib.rs:47\n");
        test.push_str("   = help: use .to_string()\n");

        // Small file content (not compressible)
        let file = "fn parse() {}\nfn tokenize() {}\nfn eval() {}\n";

        // Cargo build
        let compressed_cargo = compress_output(&cargo);
        let cargo_pct = (1.0 - compressed_cargo.len() as f64 / cargo.len() as f64) * 100.0;
        println!("Cargo build:   {} -> {} chars ({:.0}%)", cargo.len(), compressed_cargo.len(), cargo_pct);
        assert!(compressed_cargo.contains("Compiling"), "should still mention compiling");

        // Test output
        let compressed_test = compress_output(&test);
        let test_pct = (1.0 - compressed_test.len() as f64 / test.len() as f64) * 100.0;
        println!("Test output:   {} -> {} chars ({:.0}%)", test.len(), compressed_test.len(), test_pct);
        assert!(compressed_test.contains("FAILED"), "FAILED should survive");
        assert!(compressed_test.contains("E0308"), "E0308 should survive");

        // File content (too short to compress)
        let compressed_file = compress_output(file);
        let file_pct = (1.0 - compressed_file.len() as f64 / file.len() as f64) * 100.0;
        println!("File content:  {} -> {} chars ({:.0}%)", file.len(), compressed_file.len(), file_pct);
        assert_eq!(compressed_file.len(), file.len(), "short file should pass through");

        println!("\n---");
        println!("Cargo compressed: {}", &compressed_cargo[..compressed_cargo.len().min(200)]);
        println!();
        println!("Test compressed:  {}", &compressed_test[..compressed_test.len().min(200)]);
    }
