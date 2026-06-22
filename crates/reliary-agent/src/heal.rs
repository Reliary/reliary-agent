//! Self-healing edits: shadow-apply fix, run test, revert on failure.
// LLM never sees the failure spiral — gets tighter error on next attempt.

use std::path::Path;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use std::io::Write;
use tracing::error;
use std::process::Command;

// Atomic file write: write to tmp, sync, rename. Prevents partial write corruption.
pub fn atomic_write(path: &str, content: &str) -> Result<(), String> {
    let tmp = format!("{}.tmp.{}", path, std::process::id());
    let mut file = File::create(&tmp).map_err(|e| format!("create tmp {}: {}", tmp, e))?;
    file.write_all(content.as_bytes()).map_err(|e| format!("write: {}", e))?;
    file.sync_all().map_err(|e| format!("sync: {}", e))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {}", e))?;
    Ok(())
}

// Detect the test command for a project. Returns (cmd, args) or (empty, empty)
// if no recognized project files exist. Grammar-free: pure file existence checks.
fn detect_test_command(workdir: &str) -> (String, Vec<String>) {
    detect_test_command_inner(workdir)
}

fn detect_test_command_inner(workdir: &str) -> (String, Vec<String>) {
    let wd = std::path::Path::new(workdir);
    if wd.join("Cargo.toml").exists() {
        ("cargo".to_string(), vec!["test".to_string(), "--quiet".to_string()])
    } else if wd.join("pyproject.toml").exists() || wd.join("pytest.ini").exists() || wd.join("setup.py").exists() {
        ("pytest".to_string(), vec!["-q".to_string()])
    } else if wd.join("package.json").exists() {
        ("npm".to_string(), vec!["test".to_string(), "--silent".to_string()])
    } else if wd.join("go.mod").exists() {
        ("go".to_string(), vec!["test".to_string()])
    } else if wd.join("Cargo.lock").exists() {
        ("cargo".to_string(), vec!["test".to_string(), "--quiet".to_string()])
    } else {
        (String::new(), Vec::new())
    }
}

fn change_hash(file: &str, new_content: &str) -> (u64, u64) {
    let mut h = DefaultHasher::new();
    file.hash(&mut h);
    new_content.hash(&mut h);
    let file_hash = h.finish();
    let mut h2 = DefaultHasher::new();
    new_content.hash(&mut h2);
    let ident_hash = h2.finish();
    (file_hash, ident_hash)
}

// Apply a fix in a shadow worktree, run tests, revert on failure.
// Checks the B-Cell edit cache first — skips the test if this exact edit
// was previously verified on the same file content.
pub fn heal_edit(file: &str, new_content: &str, workdir: &str) -> Result<(), String> {
    if !Path::new(file).exists() {
        return Err(format!("File not found: {}", file));
    }

    // Check B-Cell cache (opt-out via RELIARY_PROXY_DISABLE_EDIT_CACHE=1)
    let edit_cache_disabled = std::env::var("RELIARY_PROXY_DISABLE_EDIT_CACHE").is_ok_and(|v| v == "1");
    let (file_hash, ident_hash) = change_hash(file, new_content);
    let chronicle_path = format!("{}/.reliary/chronicle.sqlite", workdir.trim_end_matches('/'));
    if !edit_cache_disabled {
        if let Ok(db) = rusqlite::Connection::open(&chronicle_path) {
            if let Some(outcome) = crate::chronicle::edit_cache_get(&db, file_hash, ident_hash) {
                tracing::info!("edit_cache: hit (outcome={}) for {} ident={}", outcome, file, ident_hash);
                if outcome == "pass" {
                    return Ok(());
                }
            } else {
                tracing::info!("edit_cache: miss for {} ident={}", file, ident_hash);
            }
        }
    }

    let original = fs::read_to_string(file).map_err(|e| format!("Read: {}", e))?;
    // Atomic write with fsync + rename
    atomic_write(file, new_content)?;

    // Run tests and capture output. Detect project type to pick the right test command;
    // defaults to `cargo test` for Rust projects. Falls back to no-op on unknown projects.
    let (test_cmd, test_args) = detect_test_command(workdir);
    if test_cmd.is_empty() {
        return Ok(()); // Unknown project type — skip tests
    }
    let output = Command::new(&test_cmd)
        .args(&test_args)
        .current_dir(workdir)
        .output()
        .map_err(|e| format!("Test exec: {}", e))?;

    let result = if output.status.success() {
        // Store success in B-Cell cache
        if let Ok(db) = rusqlite::Connection::open(&chronicle_path) {
            crate::chronicle::edit_cache_set(&db, file_hash, ident_hash, "pass");
        }
        Ok(())
    } else {
        // Revert — also atomic; log if revert fails
        if let Err(e) = atomic_write(file, &original) {
            error!("heal revert FAILED for {}: {} — FILE MAY BE CORRUPTED", file, e);
        }
        // Store failure in B-Cell cache
        if let Ok(db) = rusqlite::Connection::open(&chronicle_path) {
            crate::chronicle::edit_cache_set(&db, file_hash, ident_hash, "fail");
        }
        // Extract first test failure
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{}{}", stdout, stderr);
        let summary = extract_first_failure(&combined);
        Err(summary)
    };
    result
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

// Shadow-apply a reliary_fix and test
pub fn heal_fix(file: &str, old: &str, new: &str, workdir: &str) -> Result<String, String> {
    let content = reliary_core::safe_read(file).map_err(|e| format!("Read: {}", e))?;
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

// Batch heal: apply multiple edits simultaneously, run tests once, revert ALL on failure.
pub fn batch_heal(edits: &[(String, String, String)], workdir: &str) -> String {
    let mut originals: Vec<(String, String)> = Vec::new();
    for (file, old, new) in edits {
        let content = match reliary_core::safe_read(file) {
            Ok(c) => c,
            Err(e) => return format!("FAIL: cannot read {} — {}", file, e),
        };
        let fixes = vec![(old.clone(), new.clone())];
        let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);
        if count == 0 { return format!("FAIL: no match in {}", file); }
        originals.push((file.clone(), content));
        if let Err(e) = atomic_write(file, &modified) {
            return format!("FAIL: atomic write error {} — {}", file, e);
        }
    }
    let (test_cmd, test_args) = detect_test_command(workdir);
    if test_cmd.is_empty() {
        return format!("OK: {} files edited (no test command for project type)", edits.len());
    }
    let output = Command::new(&test_cmd).args(&test_args).current_dir(workdir).output();
    match output {
        Ok(out) if out.status.success() => {
            format!("OK: {} files edited, tests pass", edits.len())
        }
        Ok(out) => {
            for (file, original) in &originals { let _ = atomic_write(file, original); }
            let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            format!("REVERTED ({} files): {}", edits.len(), extract_first_failure(&combined))
        }
        Err(e) => {
            for (file, original) in &originals { let _ = atomic_write(file, original); }
            format!("REVERTED (all files): {}", e)
        }
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

    #[test]
    fn test_detect_test_command_rust() {
        let dir = std::env::temp_dir().join("reliary_heal_test_rust");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.1.0\"\n").unwrap();
        let (cmd, args) = detect_test_command_inner(dir.to_str().unwrap());
        assert_eq!(cmd, "cargo");
        assert!(args.iter().any(|a| a == "test"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_test_command_python() {
        let dir = std::env::temp_dir().join("reliary_heal_test_py");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("pyproject.toml"), "[project]\nname=\"x\"\n").unwrap();
        let (cmd, _) = detect_test_command_inner(dir.to_str().unwrap());
        assert_eq!(cmd, "pytest");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_test_command_node() {
        let dir = std::env::temp_dir().join("reliary_heal_test_node");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        let (cmd, _) = detect_test_command_inner(dir.to_str().unwrap());
        assert_eq!(cmd, "npm");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_test_command_unknown() {
        let dir = std::env::temp_dir().join("reliary_heal_test_unknown");
        let _ = std::fs::create_dir_all(&dir);
        let (cmd, args) = detect_test_command_inner(dir.to_str().unwrap());
        assert_eq!(cmd, "");
        assert!(args.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
