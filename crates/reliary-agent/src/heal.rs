/// Self-healing edits: shadow-apply fix, run test, revert on failure.
/// LLM never sees the failure spiral — gets tighter error on next attempt.

use std::path::Path;
use std::fs;
use std::process::Command;

/// Apply a fix in a shadow worktree, run tests, revert on failure.
/// Returns Ok(()) if fix passes tests, Err(reason) if tests fail.
pub fn heal_edit(file: &str, new_content: &str, workdir: &str) -> Result<(), String> {
    // Verify file exists
    if !Path::new(file).exists() {
        return Err(format!("File not found: {}", file));
    }

    // Read original content
    let original = fs::read_to_string(file).map_err(|e| format!("Read: {}", e))?;

    // Write new content
    fs::write(file, new_content).map_err(|e| format!("Write: {}", e))?;

    // Run tests
    let status = Command::new("cargo")
        .args(["test", "--quiet"])
        .current_dir(workdir)
        .status()
        .map_err(|e| format!("Test exec: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        // Revert
        fs::write(file, &original).ok();
        Err("Tests failed after edit — reverted".to_string())
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
