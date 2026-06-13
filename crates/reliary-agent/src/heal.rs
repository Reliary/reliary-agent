/// Self-healing edits: shadow-apply fix, run test, revert on failure.
/// LLM never sees the failure spiral — gets tighter error on next attempt.

use std::path::Path;
use std::fs;
use std::process::Command;

/// Apply a fix in a shadow worktree, run tests, revert on failure.
/// Returns Ok(()) if fix passes tests, Err(error_summary) with first test failure.
pub fn heal_edit(file: &str, new_content: &str, workdir: &str) -> Result<(), String> {
    if !Path::new(file).exists() {
        return Err(format!("File not found: {}", file));
    }

    let original = fs::read_to_string(file).map_err(|e| format!("Read: {}", e))?;
    fs::write(file, new_content).map_err(|e| format!("Write: {}", e))?;

    // Run tests and capture output
    let output = Command::new("cargo")
        .args(["test", "--quiet"])
        .current_dir(workdir)
        .output()
        .map_err(|e| format!("Test exec: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        // Revert
        fs::write(file, &original).ok();
        // Extract first test failure
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{}{}", stdout, stderr);
        let summary = extract_first_failure(&combined);
        Err(summary)
    }
}

fn extract_first_failure(output: &str) -> String {
    for line in output.lines() {
        let t = line.trim();
        if t.contains("FAILED") && !t.contains("test result") {
            return t.chars().take(120).collect();
        }
        if t.contains("panicked at") {
            return t.chars().take(120).collect();
        }
        if t.contains("expected `true`, got `false`") || t.contains("assertion") {
            return t.chars().take(120).collect();
        }
    }
    // Last 3 lines of output
    let lines: Vec<&str> = output.lines().collect();
    let count = lines.len();
    if count >= 3 {
        lines[count-3..].join(" | ").chars().take(150).collect()
    } else {
        "Tests failed — reverted".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_failed_line() {
        let output = "running 1 test\ntest line_zone_code ... FAILED\n    assertion failed\n";
        let r = extract_first_failure(output);
        assert!(r.contains("FAILED"), "Got: {}", r);
    }

    #[test]
    fn test_extract_panicked_at() {
        let output = "thread 'main' panicked at src/main.rs:42:\nindex out of bounds\n";
        let r = extract_first_failure(output);
        assert!(r.contains("panicked"), "Got: {}", r);
    }

    #[test]
    fn test_extract_assertion() {
        let output = "expected `true`, got `false`\n";
        let r = extract_first_failure(output);
        assert!(r.contains("expected"), "Got: {}", r);
    }

    #[test]
    fn test_extract_fallback() {
        let output = "line1\nline2\nline3\nline4\n";
        let r = extract_first_failure(output);
        assert!(r.contains("line2"), "Got: {}", r);
    }
}

/// Shadow-apply a reliary_fix and test
pub fn heal_fix(file: &str, old: &str, new: &str, workdir: &str) -> Result<String, String> {
    let content = fs::read_to_string(file).map_err(|e| format!("Read: {}", e))?;
    let fixes = vec![(old.to_string(), new.to_string())];
    let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);

    if count == 0 {
        return Err("No matches found".to_string());
    }

    match heal_edit(file, &modified, workdir) {
        Ok(()) => Ok(format!("OK: {} replacements, tests pass", count)),
        Err(e) => Err(format!("{} (reverted)", e)),
    }
}

/// Batch heal: apply multiple edits simultaneously, run tests once, revert ALL on failure.
pub fn batch_heal(edits: &[(String, String, String)], workdir: &str) -> String {
    let mut originals: Vec<(String, String)> = Vec::new();
    for (file, old, new) in edits {
        let content = match fs::read_to_string(file) {
            Ok(c) => c,
            Err(e) => return format!("FAIL: cannot read {} — {}", file, e),
        };
        let fixes = vec![(old.clone(), new.clone())];
        let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);
        if count == 0 { return format!("FAIL: no match in {}", file); }
        originals.push((file.clone(), content));
        fs::write(file, &modified).ok();
    }
    let output = Command::new("cargo").args(["test", "--quiet"]).current_dir(workdir).output();
    match output {
        Ok(out) if out.status.success() => {
            format!("OK: {} files edited, tests pass", edits.len())
        }
        Ok(out) => {
            for (file, original) in &originals { fs::write(file, original).ok(); }
            let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            format!("REVERTED ({} files): {}", edits.len(), extract_first_failure(&combined))
        }
        Err(e) => {
            for (file, original) in &originals { fs::write(file, original).ok(); }
            format!("REVERTED (all files): {}", e)
        }
    }
}
