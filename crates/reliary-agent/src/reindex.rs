//! FTS5 re-index: updates the index for changed files.
// Grammar-free: works on any text file with a supported extension.

// tracing removed

fn reindex_file(db_path: &str, file: &str, content: &str, phrases: &[String]) -> bool {
    use rusqlite::params;
    let db = match rusqlite::Connection::open(db_path) {
        Ok(d) => {
            // C2: watcher writer must use WAL+NORMAL to coexist with MCP readers.
            let _ = d.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;");
            // D2: verify schema version before writing
            if reliary_search::schema::open_existing_db_safe(&d).is_err() {
                eprintln!("reindex: schema mismatch in {}", db_path);
                return false;
            }
            d
        }
        Err(e) => {
            eprintln!("reindex open {}: {}", db_path, e);
            return false;
        }
    };

    // Schema: phrases(id, phrase), file_map(id, file_path), phrase_occ(phrase_id, file_id, ...)
    // Step 1: Ensure file_map entry exists for this file
    if let Err(e) = db.execute(
        "INSERT OR IGNORE INTO file_map (file_path) VALUES (?1)",
        params![file],
    ) {
        eprintln!("reindex file_map INSERT: {}", e);
        return false;
    }
    let file_id: i64 = match db.query_row(
        "SELECT id FROM file_map WHERE file_path = ?1",
        params![file],
        |row| row.get(0),
    ) {
        Ok(id) => id,
        Err(e) => { eprintln!("reindex file_map SELECT: {}", e); return false; }
    };

    // Step 2: Delete existing entries for this file from ALL relevant tables.
    // Arc 31 Phase B: previously only phrase_occ was deleted, which made the
    // watcher's incremental re-index leave stale occurrence/block/bindings.
    //
    // Arc 37 schema v4: phrase_occ.file_blob is packed. We can't DELETE
    // by file_id directly; we rebuild each affected blob without our entry.
    if let Err(e) = db.execute_batch("BEGIN;") {
        eprintln!("reindex BEGIN: {}", e);
        return false;
    }

    if let Err(e) = db.execute("DELETE FROM occurrence WHERE file_id = ?1", params![file_id]) {
        eprintln!("reindex DELETE occurrence: {}", e);
        return false;
    }
    if let Err(e) = db.execute("DELETE FROM block WHERE file_id = ?1", params![file_id]) {
        eprintln!("reindex DELETE block: {}", e);
        return false;
    }
    if let Err(e) = db.execute("DELETE FROM scope_binding WHERE file_id = ?1", params![file_id]) {
        eprintln!("reindex DELETE scope_binding: {}", e);
        return false;
    }
    if let Err(e) = db.execute("DELETE FROM method_occurrence WHERE file_id = ?1", params![file_id]) {
        eprintln!("reindex DELETE method_occurrence: {}", e);
        return false;
    }
    if let Err(e) = db.execute("DELETE FROM file_stats WHERE file_id = ?1", params![file_id]) {
        eprintln!("reindex DELETE file_stats: {}", e);
        return false;
    }

    // C7: strip this file's entry from ALL phrase_occ blobs.
    // P1-5: use file_phrases side table instead of full-table scan.
    {
        let mut affected_phrases: Vec<i64> = Vec::new();
        if let Ok(mut stmt) = db.prepare("SELECT phrase_id FROM file_phrases WHERE file_id = ?1") {
            if let Ok(rows) = stmt.query_map(params![file_id], |r| r.get::<_, i64>(0)) {
                for row in rows.flatten() {
                    affected_phrases.push(row);
                }
            }
        }
        let mut del_stmt = match db.prepare("DELETE FROM phrase_occ WHERE phrase_id = ?1") {
            Ok(s) => Some(s),
            Err(e) => { eprintln!("reindex prepare del_stmt: {}", e); None }
        };
        let mut upd_stmt = match db.prepare("UPDATE phrase_occ SET file_blob = ?1 WHERE phrase_id = ?2") {
            Ok(s) => Some(s),
            Err(e) => { eprintln!("reindex prepare upd_stmt: {}", e); None }
        };
        for pid in &affected_phrases {
            // Fetch current blob for this phrase.
            let blob: Option<Vec<u8>> = db.query_row(
                "SELECT file_blob FROM phrase_occ WHERE phrase_id = ?1",
                params![pid],
                |r| r.get(0),
            ).ok();
            let blob = match blob {
                Some(b) => b,
                None => continue,
            };
            let entries: Vec<(i64, u8)> = reliary_search::schema::unpack_file_blob(&blob)
                .filter(|(fid, _)| *fid != file_id)
                .collect();
        if entries.is_empty() {
            if let Some(stmt) = del_stmt.as_mut() {
                if let Err(e) = stmt.execute(params![pid]) {
                    eprintln!("reindex DELETE phrase_occ: {}", e);
                }
            }
        } else {
            let new_blob = reliary_search::schema::pack_file_blob(&entries);
            if let Some(stmt) = upd_stmt.as_mut() {
                if let Err(e) = stmt.execute(params![&new_blob[..], pid]) {
                    eprintln!("reindex UPDATE phrase_occ: {}", e);
                }
            }
        }
    }
    // Also delete file_phrases entries for this file.
    if let Err(e) = db.execute("DELETE FROM file_phrases WHERE file_id = ?1", params![file_id]) {
        eprintln!("reindex DELETE file_phrases: {}", e);
    }

    // Arc 37 schema v4: collect phrase_ids for new content's phrases to UPSERT.
    let mut affected_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    {
        let mut insert_stmt = db.prepare("INSERT OR IGNORE INTO phrases (phrase) VALUES (?1)");
        let mut select_stmt = db.prepare("SELECT id FROM phrases WHERE phrase = ?1");
        for phrase in phrases {
            if let Ok(ref mut stmt) = insert_stmt {
                if let Err(e) = stmt.execute(params![phrase]) {
                    eprintln!("reindex INSERT phrase: {}", e);
                }
            }
            if let Ok(ref mut stmt) = select_stmt {
                if let Ok(pid) = stmt.query_row(
                    params![phrase],
                    |row| row.get::<_, i64>(0),
                ) {
                    affected_ids.insert(pid);
                }
            }
        }
    }
    // (C7 scan above already stripped file_id from all phrase_occ blobs;
    // the per-affected-id strip loop was removed.)

    // Step 3: UPSERT new (phrase, file) entries into phrase_occ with packed blob.
    // The C7 scan above stripped file_id from ALL blobs; we now append the new entry.
    for pid in &affected_ids {
        let entry: Vec<u8> = {
            let mut b = Vec::with_capacity(3);
            reliary_search::schema::encode_varint(file_id, &mut b);
            b.push(0u8);  // flags = 0 (reindex stub)
            b
        };
        // D3: UPSERT error must propagate, not silently succeed
        if let Err(e) = db.execute(
            "INSERT INTO phrase_occ (phrase_id, file_blob) VALUES (?1, ?2)
             ON CONFLICT(phrase_id) DO UPDATE SET file_blob = file_blob || excluded.file_blob",
            params![pid, &entry[..]],
        ) {
            eprintln!("reindex phrase_occ UPSERT: {}", e);
            let _ = db.execute_batch("ROLLBACK;");
            return false;
        }
    }

    // C5: COMMIT failure must not return success
    if let Err(e) = db.execute_batch("COMMIT;") {
        eprintln!("reindex COMMIT: {}", e);
        let _ = db.execute_batch("ROLLBACK;");
        return false;
    }

    // C6: rebuild lazy tables AFTER COMMIT (outside transaction). If process
    // crashes here, the DELETEs+INSERTs above are persisted but the lazy
    // tables remain empty until next build_all. Downstream tools will see
    // stale data but the phrase_occ (file-level search) is consistent.
    // (Properly nesting inside the tx would require restructuring ensure_*
    // to use SAVEPOINT, which is deferred.)
    if let Err(e) = reliary_search::lazy_tables::ensure_all_for_file_with_content(&db, file_id, content) {
        eprintln!("reindex lazy_tables: {}", e);
    }

    true
}
}

/// Re-index a single file. Public API for gate.js / hook layer to call after every edit.
/// Returns the number of phrases indexed (0 if file not indexed).
pub fn reindex_single_file(db_path: &str, file: &str, content: &str) -> usize {
    if !std::path::Path::new(db_path).exists() {
        return 0;
    }
    let phrases = reliary_search::tokenize(content);
    let count = phrases.len();
    if reindex_file(db_path, file, content, &phrases) {
        count
    } else {
        0
    }
}
