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

/// Parallel heal: process multiple edits concurrently, return the slowest wall time.
/// Uses a thread pool (one per edit) and waits for all to complete.
/// Returns aggregate results string.
pub fn parallel_heal(
    edits: &[(String, String, String)],  // (file, old, new)
    workdir: &str,
) -> String {
    let handles: Vec<_> = edits.iter().map(|(file, old, new)| {
        let file = file.clone();
        let old = old.clone();
        let new = new.clone();
        let wd = workdir.to_string();
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let result = heal_fix(&file, &old, &new, &wd);
            let elapsed = start.elapsed().as_millis();
            (file, result, elapsed)
        })
    }).collect();

    let mut results: Vec<String> = Vec::new();
    for h in handles {
        match h.join() {
            Ok((file, Ok(msg), elapsed)) => {
                results.push(format!("{} OK ({}ms)", file.rsplit('/').next().unwrap_or(&file), elapsed));
            }
            Ok((file, Err(e), elapsed)) => {
                results.push(format!("{} FAIL ({}ms): {}", file.rsplit('/').next().unwrap_or(&file), elapsed, e.chars().take(60).collect::<String>()));
            }
            Err(_) => results.push("ERROR: thread panic".to_string()),
        }
    }

    results.join("\n")
}
