//! File walking, tokenization, and index insertion.
//! Uses Rayon for parallel parsing and FxHashMap for speed.

use rusqlite::{params, Connection};
use walkdir::WalkDir;
use rustc_hash::FxHashMap;
use rayon::prelude::*;

use crate::schema::{classify_line, pack_flags, pack_line_nos};
use crate::{scan_identifiers, porter_stem};

const SUPPORTED_EXTS: [&str; 12] = ["rs", "py", "js", "ts", "tsx", "jsx", "go", "c", "cpp", "h", "hpp", "rb"];

struct FileResult {
    file: String,
    content_len: usize,
    phrase_locations: FxHashMap<String, Vec<(usize, u8)>>,
    lines_is_def: Vec<bool>, // pre-calculated is_def for each line to avoid keeping full file lines
}

/// Index all supported files in a directory. Returns file count.
pub fn index_directory(db: &Connection, dir: &str) -> Result<usize, String> {
    let mut paths = Vec::new();
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {  // GUARDED: intentional
        let path = entry.path();
        if !path.is_file() { continue; }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !SUPPORTED_EXTS.contains(&ext) { continue; }
        if path.components().any(|c| c.as_os_str().to_str().map(|s| s.starts_with('.') && s != ".").unwrap_or(false)) { continue; }
        paths.push(path.to_path_buf());
    }

    let results: Vec<FileResult> = paths.par_iter().filter_map(|path| {
        let file = path.to_string_lossy().to_string();
        let content = std::fs::read_to_string(path).ok()?;
        
        let lines: Vec<&str> = content.lines().collect();
        let mut phrase_locations: FxHashMap<String, Vec<(usize, u8)>> = FxHashMap::default();
        let mut lines_is_def = Vec::with_capacity(lines.len());

        for (li, line) in lines.iter().enumerate() {
            let zone = classify_line(line);
            for token in scan_identifiers(line) {
                let stemmed = porter_stem(&token);
                phrase_locations.entry(stemmed).or_default().push((li, zone));
            }
            let t = line.trim();
            lines_is_def.push(
                t.starts_with("fn ") || t.starts_with("def ") || t.starts_with("class ")
                || t.starts_with("struct ") || t.starts_with("enum ") || t.starts_with("trait ")
                || t.starts_with("pub ")
            );
        }

        if phrase_locations.is_empty() { return None; }

        Some(FileResult {
            file,
            content_len: content.len(),
            phrase_locations,
            lines_is_def,
        })
    }).collect();

    let mut count = 0;
    
    // Batch inserts
    let _ = db.execute_batch("BEGIN;");
    
    for res in results {
        db.execute("INSERT OR IGNORE INTO file_map (file_path) VALUES (?1)", params![res.file])
            .map_err(|e| format!("insert file: {}", e))?;
        let file_id: i64 = db.query_row("SELECT id FROM file_map WHERE file_path = ?1", params![res.file], |r| r.get(0))
            .map_err(|e| format!("get file id: {}", e))?;

        for (phrase, locations) in &res.phrase_locations {
            db.execute("INSERT OR IGNORE INTO phrases (phrase) VALUES (?1)", params![phrase])
                .map_err(|e| format!("insert phrase: {}", e))?;
            let phrase_id: i64 = db.query_row("SELECT id FROM phrases WHERE phrase = ?1", params![phrase], |r| r.get(0))
                .unwrap_or(0);

            if phrase_id == 0 { continue; }

            let num_locs = locations.len() as u32;
            let avg_zone = locations.iter().map(|(_, z)| *z as u32).sum::<u32>() / num_locs.max(1);
            
            let is_def = if locations.iter().any(|(li, _)| res.lines_is_def.get(*li).copied().unwrap_or(false)) { 1 } else { 0 };

            let flags = pack_flags(is_def, avg_zone as i32, num_locs);
            let first_line = locations.first().map(|(l, _)| *l as u32).unwrap_or(0);
            let line_nos = pack_line_nos(first_line);

            if let Err(e) = db.execute(
                "INSERT OR REPLACE INTO phrase_occ (phrase_id, file_id, flags, line_nos) VALUES (?1, ?2, ?3, ?4)",
                params![phrase_id, file_id, &flags[..], &line_nos[..]],
            ) {
                eprintln!("[ingest] phrase_occ: {}", e);
            }

            if let Err(e) = db.execute("INSERT OR IGNORE INTO phrases_fts(rowid, phrase) VALUES (?1, ?2)", params![phrase_id, phrase]) {
                eprintln!("[ingest] phrases_fts: {}", e);
            }
        }

        let token_len = res.phrase_locations.len() as i64;
        let content_len = res.content_len as i64;
        db.execute(
            "INSERT OR REPLACE INTO file_stats (file_id, token_len, content_len) VALUES (?1, ?2, ?3)",
            params![file_id, token_len, content_len],
        ).ok();  // GUARDED: intentional

        count += 1;
    }
    
    let _ = db.execute_batch("COMMIT;");

    Ok(count)
}
