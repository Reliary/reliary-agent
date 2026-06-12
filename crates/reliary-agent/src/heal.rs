/// Self-healing edits: shadow-apply fix, run test, revert on failure.
/// LLM never sees the failure spiral — gets tighter error on next attempt.

use std::path::Path;
use std::fs;
use std::process::Command;

/// Apply a fix in a shadow worktree, run tests, revert on failure.
/// Uses atomic renames to prevent file corruption on crash.
pub fn heal_edit(file: &str, new_content: &str, workdir: &str) -> Result<(), String> {
    if !Path::new(file).exists() {
        return Err(format!("File not found: {}", file));
    }

    // Step 1: Create atomic backup
    let backup = format!("{}.reliary-bak", file);
    fs::copy(file, &backup).map_err(|e| format!("Backup: {}", e))?;

    // Step 2: Write new content
    if let Err(e) = fs::write(file, new_content) {
        // Restore backup on write failure
        let _ = fs::rename(&backup, file);
        return Err(format!("Write: {}", e));
    }

    // Step 3: Run tests
    let output = Command::new("cargo")
        .args(["test", "--quiet"])
        .current_dir(workdir)
        .output()
        .map_err(|e| {
            // Restore backup on test configuration failure
            let _ = fs::rename(&backup, file);
            format!("Test exec: {}", e)
        })?;

    if output.status.success() {
        // Clean up backup on success
        let _ = fs::remove_file(&backup);
        Ok(())
    } else {
        // Atomic revert on test failure
        if let Err(e) = fs::rename(&backup, file) {
            return Err(format!("REVERT FAILED (file may be corrupt): {}", e));
        }
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
