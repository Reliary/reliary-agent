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
    }
    reliary_core::open(path)
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
}