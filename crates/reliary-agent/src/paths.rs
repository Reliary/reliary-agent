//! Project root resolution — find .reliary/ directory by walking up from
//! any file path. Used by CLI and MCP server to locate the index.

use std::path::Path;

/// Find (root, index_path, cache_path) by walking up from `start` looking for `.reliary/`.
/// Returns None if no `.reliary/` directory found.
#[allow(dead_code)]
pub fn find_reliary_root(start: &str) -> Option<(String, String, String)> {
    let p = Path::new(start);
    let mut current: Option<&Path> = if p.is_file() || p.is_symlink() {
        p.parent()
    } else {
        Some(p)
    };
    while let Some(dir) = current {
        let reliary = dir.join(".reliary");
        if reliary.is_dir() {
            let root = dir.to_string_lossy().to_string();
            let idx = reliary.join("index.sqlite").to_string_lossy().to_string();
            let cache = reliary.join("cache.sqlite").to_string_lossy().to_string();
            return Some((root, idx, cache));
        }
        current = dir.parent();
    }
    None
}

/// Get the workdir from start, defaulting to "." if no .reliary found.
#[allow(dead_code)]
pub fn find_workdir(start: &str) -> String {
    find_reliary_root(start).map(|(r, _, _)| r).unwrap_or_else(|| ".".to_string())
}

/// Strip the root prefix from a path to get the index-stored relative path.
#[allow(dead_code)]
pub fn relativize(root: &str, file: &str) -> String {
    file.strip_prefix(&format!("{}/", root))
        .or_else(|| file.strip_prefix(root))
        .unwrap_or(file)
        .to_string()
}

/// Open or create the cache database at the given path.
pub fn open_or_create(path: &std::path::Path) -> Result<rusqlite::Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok(); // GUARDED: intentional — open() reports failure
        ensure_reliary_ignored(parent);
    }
    reliary_core::open(path)
}

/// Make `.reliary/` invisible to git by writing a self-ignoring `.gitignore`
/// (`*`) into it.
///
/// Why a file inside the directory rather than an entry in the project's
/// tracked `.gitignore`:
///   - no churn in a file the user owns (we never edit tracked files);
///   - it works at ANY nesting depth (a nested `bench/.reliary/` is covered by
///     its own copy without enumerating paths);
///   - git reads a `.gitignore` even when the file itself is matched by the
///     ignore rules, so the directory reports clean.
///
/// This is a safety measure, not cosmetics: `.reliary/` holds `index.sqlite`
/// (source-derived) and `cache.sqlite`, which stores the ORIGINAL text of every
/// command passed through `reliary wrap` — routinely API keys, env dumps and
/// connection strings. A fresh user who runs `trust` then `git add .` would
/// otherwise stage both.
///
/// Idempotent and non-destructive: existing user content (e.g. a custom
/// `.gitignore` that negates `!config.json`) is never overwritten. Only acts
/// when `dir` is named `.reliary`.
pub fn ensure_reliary_ignored(dir: &Path) {
    if dir.file_name().and_then(|n| n.to_str()) != Some(".reliary") {
        return;
    }
    let gi = dir.join(".gitignore");
    if gi.exists() {
        return;
    }
    let _ = std::fs::write(&gi, "*\n"); // GUARDED: intentional — best-effort, never fatal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_workdir_defaults_to_dot() {
        assert_eq!(find_workdir("/nonexistent/path"), ".");
    }

    #[test]
    fn relativize_strips_prefix() {
        assert_eq!(relativize("/home/user/project", "/home/user/project/src/main.rs"), "src/main.rs");
    }

    #[test]
    fn relativize_returns_full_when_no_match() {
        assert_eq!(relativize("/home/a", "/home/b/file.rs"), "/home/b/file.rs");
    }

    #[test]
    fn ensure_reliary_ignored_writes_star() {
        let dir = std::env::temp_dir().join(format!("relign_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rr = dir.join(".reliary");
        std::fs::create_dir_all(&rr).unwrap();
        ensure_reliary_ignored(&rr);
        let gi = rr.join(".gitignore");
        assert!(gi.exists(), "self-ignore must be written");
        assert_eq!(std::fs::read_to_string(&gi).unwrap().trim(), "*");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_reliary_ignored_does_not_clobber_user_file() {
        let dir = std::env::temp_dir().join(format!("relign2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rr = dir.join(".reliary");
        std::fs::create_dir_all(&rr).unwrap();
        // A user who chose to share config keeps their negated file.
        std::fs::write(rr.join(".gitignore"), "*\n!config.json\n").unwrap();
        ensure_reliary_ignored(&rr);
        let got = std::fs::read_to_string(rr.join(".gitignore")).unwrap();
        assert!(got.contains("!config.json"), "user .gitignore must be preserved, got: {got:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_reliary_ignored_ignores_other_dirs() {
        // Must never litter a non-.reliary directory.
        let dir = std::env::temp_dir().join(format!("relign3_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        ensure_reliary_ignored(&dir);
        assert!(!dir.join(".gitignore").exists(), "must not write into a non-.reliary dir");
        let _ = std::fs::remove_dir_all(&dir);
    }
}