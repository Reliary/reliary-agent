//! Shared project-index resolution for the CLI and the MCP server.
//!
//! Two strategies used to disagree:
//!   * the CLI opened `<path-arg>/.reliary/index.sqlite`
//!   * the MCP server opened `<process-cwd>/.reliary/index.sqlite`
//!
//! When an agent launches `reliary mcp` from anywhere other than the indexed
//! project (a global MCP config, or a client that does not set the spawn cwd),
//! every symbol tool reported "no index" regardless of the caller's intent.
//!
//! This module gives both the same answer: pick a start directory, then walk
//! **up** to the nearest ancestor that holds a usable index. A candidate is
//! only accepted if it is a real reliary index — a stray empty `index.sqlite`
//! (SQLite creates one on open) is skipped rather than shadowing the project.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use reliary_search::schema::SCHEMA_VERSION;

/// The index path for a project root.
pub fn db_path_for(root: &Path) -> PathBuf {
    root.join(".reliary").join("index.sqlite")
}

/// Whether `db_path` is a usable reliary index rather than a stub or a foreign
/// SQLite file. Opens read-only and checks `PRAGMA user_version` without ever
/// creating the file. A freshly created empty DB has `user_version = 0` and is
/// rejected, so a stray `~/.reliary/index.sqlite` cannot shadow a real project.
pub fn is_reliary_index(db_path: &Path) -> bool {
    if !db_path.is_file() {
        return false;
    }
    match Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(db) => db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0))
            .map(|v| v > 0 && v <= SCHEMA_VERSION)
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// Walk up from `start` (inclusive) to the nearest ancestor with a usable
/// index. `start` may be a file (its parent is used).
pub fn nearest_index_root(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };
    loop {
        if is_reliary_index(&db_path_for(&cur)) {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Outcome of index resolution.
pub enum Resolution {
    /// A usable index was found at `.reliary/index.sqlite` under this root.
    Found(PathBuf),
    /// Nothing usable was found; the contained directory is where the upward
    /// search began (name it in diagnostics).
    NotFound(PathBuf),
}



/// Resolve the project root for an index query.
///
/// Precedence for the start directory (first that exists as a directory wins):
///   1. `path_hint` — a tool's `path` argument, resolved against the CWD
///   2. `RELIARY_ROOT` env var — explicit user override
///   3. `client_root` — the workspace root advertised by the MCP client
///      (`roots/list`), the most authoritative source when present
///   4. the process CWD
///
/// Then walk up to the nearest usable index. The CWD/environment is read here
/// rather than cached so a `roots/list_changed` notification is honored.
pub fn resolve(path_hint: Option<&str>, client_root: Option<&Path>) -> Resolution {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // 1. explicit path hint (only if it names an existing directory)
    if let Some(h) = path_hint.filter(|h| !h.is_empty()) {
        let p = cwd.join(h);
        if p.is_dir() {
            return match nearest_index_root(&p) {
                Some(root) => Resolution::Found(root),
                None => Resolution::NotFound(p),
            };
        }
    }

    // 2. RELIARY_ROOT override
    if let Some(env_root) = std::env::var_os("RELIARY_ROOT") {
        let p = PathBuf::from(env_root);
        if p.is_dir() {
            if let Some(root) = nearest_index_root(&p) {
                return Resolution::Found(root);
            }
        }
    }

    // 3. client-advertised workspace root
    if let Some(cr) = client_root.filter(|p| p.is_dir()) {
        if let Some(root) = nearest_index_root(cr) {
            return Resolution::Found(root);
        }
    }

    // 4. process CWD
    match nearest_index_root(&cwd) {
        Some(root) => Resolution::Found(root),
        None => Resolution::NotFound(cwd),
    }
}

/// Human-readable "where did you look" line for empty results.
pub fn searched_note(start: &Path) -> String {
    format!(
        "no index found at or above {} — run `reliary trust {}` to build one",
        start.display(),
        start.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_index(root: &Path, version: i32) {
        let dir = root.join(".reliary");
        fs::create_dir_all(&dir).unwrap();
        let db = Connection::open(dir.join("index.sqlite")).unwrap();
        db.execute_batch(&format!("PRAGMA user_version = {}", version))
            .unwrap();
    }

    #[test]
    fn finds_index_in_same_dir() {
        let tmp = std::env::temp_dir().join(format!("relroot_same_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        make_index(&tmp, SCHEMA_VERSION);
        assert_eq!(nearest_index_root(&tmp), Some(tmp.clone()));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn walks_up_from_subdirectory() {
        let tmp = std::env::temp_dir().join(format!("relroot_up_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let deep = tmp.join("crates").join("agent").join("src");
        fs::create_dir_all(&deep).unwrap();
        make_index(&tmp, SCHEMA_VERSION);
        assert_eq!(nearest_index_root(&deep), Some(tmp.clone()));
        let _ = fs::remove_dir_all(&tmp);
    }

    /// The negative control for the stub-shadowing bug: a stray empty
    /// `index.sqlite` must NOT be accepted as a project index.
    #[test]
    fn skips_stub_index_and_finds_real_one_above() {
        let tmp = std::env::temp_dir().join(format!("relroot_stub_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let inner = tmp.join("project");
        fs::create_dir_all(inner.join(".reliary")).unwrap();
        // A stub: SQLite's default user_version is 0.
        Connection::open(inner.join(".reliary").join("index.sqlite")).unwrap();
        assert!(
            !is_reliary_index(&inner.join(".reliary").join("index.sqlite")),
            "a 0-version stub must not count as an index"
        );
        // Real index one level up.
        make_index(&tmp, SCHEMA_VERSION);
        assert_eq!(nearest_index_root(&inner), Some(tmp.clone()));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn path_hint_selects_project() {
        let tmp = std::env::temp_dir().join(format!("relroot_hint_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let proj = tmp.join("proj");
        fs::create_dir_all(&proj).unwrap();
        make_index(&proj, SCHEMA_VERSION);
        let hint = proj.to_string_lossy().to_string();
        match resolve(Some(&hint), None) {
            Resolution::Found(r) => assert_eq!(r, proj),
            Resolution::NotFound(_) => panic!("path hint should have resolved"),
        }
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn client_root_used_when_no_hint() {
        let tmp = std::env::temp_dir().join(format!("relroot_client_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let proj = tmp.join("proj");
        fs::create_dir_all(&proj).unwrap();
        make_index(&proj, SCHEMA_VERSION);
        match resolve(None, Some(&proj)) {
            Resolution::Found(r) => assert_eq!(r, proj),
            Resolution::NotFound(_) => panic!("client root should have resolved"),
        }
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn read_only_probe_does_not_create_file() {
        let tmp = std::env::temp_dir().join(format!("relroot_nocreate_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let missing = db_path_for(&tmp);
        assert!(!is_reliary_index(&missing));
        assert!(!missing.exists(), "is_reliary_index must never create the file");
        let _ = fs::remove_dir_all(&tmp);
    }
}
