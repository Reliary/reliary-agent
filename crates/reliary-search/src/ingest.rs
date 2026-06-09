/// File walking, tokenization, and index insertion.
/// Sequential but fast — stria's speed comes from FTS5, not parallelism.

use rusqlite::{params, Connection};
use walkdir::WalkDir;
use std::collections::HashMap;

use crate::schema::{classify_line, pack_flags, pack_line_nos};
use crate::{scan_identifiers, porter_stem};

const SUPPORTED_EXTS: [&str; 12] = ["rs", "py", "js", "ts", "tsx", "jsx", "go", "c", "cpp", "h", "hpp", "rb"];

/// Index all supported files in a directory. Returns file count.
pub fn index_directory(db: &Connection, dir: &str) -> Result<usize, String> {
    let mut count = 0usize;

    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() { continue; }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !SUPPORTED_EXTS.contains(&ext) { continue; }
        if path.components().any(|c| c.as_os_str().to_str().map(|s| s.starts_with('.')).unwrap_or(false)) { continue; }

        let file = path.to_string_lossy().to_string();
        let content = match std::fs::read_to_string(&file) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Tokenize and classify lines
        let lines: Vec<&str> = content.lines().collect();
        let mut phrase_locations: HashMap<String, Vec<(usize, u8)>> = HashMap::new();

        for (li, line) in lines.iter().enumerate() {
            let zone = classify_line(line);
            for token in scan_identifiers(line) {
                let stemmed = porter_stem(&token);
                phrase_locations.entry(stemmed).or_default().push((li, zone));
            }
        }

        if phrase_locations.is_empty() { continue; }

        // Insert file
        db.execute("INSERT OR IGNORE INTO file_map (file_path) VALUES (?1)", params![file])
            .map_err(|e| format!("insert file: {}", e))?;
        let file_id: i64 = db.query_row("SELECT id FROM file_map WHERE file_path = ?1", params![file], |r| r.get(0))
            .map_err(|e| format!("get file id: {}", e))?;

        // Insert phrases and occurrences
        for (phrase, locations) in &phrase_locations {
            db.execute("INSERT OR IGNORE INTO phrases (phrase) VALUES (?1)", params![phrase])
                .map_err(|e| format!("insert phrase: {}", e))?;
            let phrase_id: i64 = db.query_row("SELECT id FROM phrases WHERE phrase = ?1", params![phrase], |r| r.get(0))
                .unwrap_or(0);

            if phrase_id == 0 { continue; }

            // Aggregate: count per file, is_def heuristic, zone
            let num_locs = locations.len() as u32;
            let avg_zone = locations.iter().map(|(_, z)| *z as u32).sum::<u32>() / num_locs.max(1);
            let is_def = if locations.iter().any(|(li, _)| {
                let line = lines.get(*li).unwrap_or(&"");
                let t = line.trim();
                t.starts_with("fn ") || t.starts_with("def ") || t.starts_with("class ")
                    || t.starts_with("struct ") || t.starts_with("enum ") || t.starts_with("trait ")
                    || t.starts_with("pub ")
            }) { 1 } else { 0 };

            let flags = pack_flags(is_def, avg_zone as i32, num_locs);
            let first_line = locations.first().map(|(l, _)| *l as u32).unwrap_or(0);
            let line_nos = pack_line_nos(first_line);

            db.execute(
                "INSERT OR REPLACE INTO phrase_occ (phrase_id, file_id, flags, line_nos) VALUES (?1, ?2, ?3, ?4)",
                params![phrase_id, file_id, &flags[..], &line_nos[..]],
            ).ok();

            // Populate FTS5
            db.execute("INSERT OR IGNORE INTO phrases_fts(rowid, phrase) VALUES (?1, ?2)", params![phrase_id, phrase]).ok();
        }

        // File stats
        let token_len = phrase_locations.len() as i64;
        let content_len = content.len() as i64;
        db.execute(
            "INSERT OR REPLACE INTO file_stats (file_id, token_len, content_len) VALUES (?1, ?2, ?3)",
            params![file_id, token_len, content_len],
        ).ok();

        count += 1;
    }

    Ok(count)
}
