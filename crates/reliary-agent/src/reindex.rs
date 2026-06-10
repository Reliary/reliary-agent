/// Incremental FTS5 re-index: scavenger companion that updates the index for changed files.
/// Grammar-free: works on any text file with a supported extension.

use std::path::Path;
use std::time::SystemTime;

/// Re-index files that have been modified since the last index build.
/// Returns the number of files re-indexed.
pub fn incremental_reindex(workdir: &str) -> usize {
    let db_path_str = format!("{}/.reliary/index.sqlite", workdir.trim_end_matches('/'));
    let db_path = Path::new(&db_path_str);

    if !db_path.exists() {
        return 0; // no index to update
    }

    // Check if reindex marker exists and when it was set
    let marker_path = Path::new(workdir).join(".reliary").join("last_reindex");
    let last_reindex = marker_path_to_epoch(&marker_path);

    // Walk project files
    let mut count = 0;
    let supported_exts = ["rs", "py", "js", "ts", "go", "rb", "java", "md", "toml", "yaml", "json"];

    if let Ok(entries) = walkdir(workdir) {
        for file in entries {
            if let Some(ext) = file.extension().and_then(|e| e.to_str()) {
                if !supported_exts.contains(&ext) { continue; }
                let path_str = file.to_string_lossy().to_string();
                let mtime = file_modified(&file);
                if mtime > last_reindex {
                    // Re-index this file: delete old rows, insert fresh
                    if let Ok(content) = std::fs::read_to_string(&path_str) {
                        if reindex_file(&db_path_str, &path_str, &content) {
                            count += 1;
                        }
                    }
                }
            }
        }
    }

    // Update marker
    let _ = std::fs::write(&marker_path, b"now");
    count
}

fn marker_path_to_epoch(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
        .unwrap_or(0)
}

fn file_modified(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
        .unwrap_or(0)
}

fn walkdir(dir: &str) -> Result<Vec<std::path::PathBuf>, String> {
    let mut files = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(dir)];
    let skip_dirs = [".git", ".reliary", "node_modules", "target", "__pycache__", ".venv"];

    while let Some(path) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if !skip_dirs.contains(&name) { stack.push(p); }
                } else {
                    files.push(p);
                }
            }
        }
    }
    Ok(files)
}

fn reindex_file(db_path: &str, file: &str, content: &str) -> bool {
    use rusqlite::params;
    let db = match rusqlite::Connection::open(db_path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    // Delete existing rows for this file
    let _ = db.execute("DELETE FROM phrases WHERE file = ?1", params![file]);

    // Extract phrases and insert
    let phrases = reliary_search::tokenize(content);
    for phrase in &phrases {
        // Simple zone classification: count structural chars
        let zone = if content.contains("fn ") || content.contains("def ") | content.contains("class ") { 0 } else { 1 };
        let _ = db.execute(
            "INSERT INTO phrases (file, line_from, line_to, zone, prefix_offset) VALUES (?1, 0, 0, ?2, 0)",
            params![file, zone],
        );
        let id = db.last_insert_rowid();
        let _ = db.execute(
            "INSERT INTO phrases_fts (rowid, phrase) VALUES (?1, ?2)",
            params![id, phrase],
        );
    }
    true
}
