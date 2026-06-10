/// Phase 3: Recursive Decomposition via the Harness.
/// The LLM never sees a failure — the harness decomposes and retries.

use std::process::Command;
use std::path::Path;

/// Re-try an edit that uses heal. If it fails, decompose the fix.
/// Heuristic: apply the fix in the main file, then check caller files.
pub fn heal_or_decompose(file: &str, old: &str, new: &str, workdir: &str) -> String {
    // First attempt: direct heal
    let first = super::heal::heal_fix(file, old, new, workdir);
    if let Ok(msg) = &first {
        if msg.starts_with("OK") {
            return format!("OK: direct fix — {}", msg);
        }
    }

    // Direct heal failed. Try decomposing.
    // Strategy 1: apply only to non-test files (skip test files)
    let dir = Path::new(file).parent().unwrap_or(Path::new("."));
    let basename = Path::new(file).file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let test_file = format!("test_{}", basename);
    let alt_test_file = format!("{}_test", basename);

    // Find all non-test files that might need this fix
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
            if (ext == "rs" || ext == "py" || ext == "js") 
                && name != test_file && name != alt_test_file {
                if let Some(s) = p.to_str() {
                    candidates.push(s.to_string());
                }
            }
        }
    }

    // Apply fix to all non-test candidates
    let mut applied = 0;
    for candidate in &candidates {
        if let Ok(content) = std::fs::read_to_string(candidate) {
            if content.contains(old) {
                let (modified, count) = reliary_fix::apply_fixes(&content, &[(old.to_string(), new.to_string())]);
                if count > 0 {
                    let _ = std::fs::write(candidate, &modified);
                    applied += count;
                }
            }
        }
    }

    if applied == 0 {
        return format!("FAIL: direct heal failed, no other files to apply to — {}", 
            first.unwrap_or_else(|e| e));
    }

    // Run tests
    let output = Command::new("cargo")
        .args(["test", "--quiet"])
        .current_dir(workdir)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            format!("OK: decomposed fix — applied to {} files, tests pass", applied)
        }
        Ok(out) => {
            // Revert by restoring from git
            let _ = Command::new("git")
                .args(["checkout", "--", file])
                .current_dir(workdir)
                .output();
            for candidate in &candidates {
                let _ = Command::new("git")
                    .args(["checkout", "--", candidate])
                    .current_dir(workdir)
                    .output();
            }
            let stderr = String::from_utf8_lossy(&out.stderr);
            let first_fail = stderr.lines()
                .find(|l| l.contains("FAIL") || l.contains("failed"))
                .unwrap_or("no failure details");
            format!("FAIL: decomposed fix reverted — {}", first_fail.chars().take(120).collect::<String>())
        }
        Err(_) => {
            format!("FAIL: decomposed fix, but test runner failed")
        }
    }
}
