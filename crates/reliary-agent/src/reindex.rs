//! FTS5 re-index: updates the index for changed files.
// Grammar-free: works on any text file with a supported extension.
// V73: uses the shared `extract_file_phrases` pipeline so reindexed files are
// byte-identical to trust-time ingestion (same stemmer, same is_def/zone/count
// flags, same file_stats). Repopulates `file_phrases` so repeated reindexes
// don't duplicate entries.

fn reindex_file(db_path: &str, file: &str, content: &str) -> bool {
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

    // V73: canonicalize so the file_map key matches trust-time ingestion
    // (ingest stores canonicalize() results). Without this, a relative or
    // symlinked path creates a DUPLICATE file_map row while the canonical
    // row's stale entries survive — two rows for one file.
    let canonical = std::fs::canonicalize(file)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| file.to_string());

    // Step 1: Ensure file_map entry exists for this file.
    if let Err(e) = db.execute(
        "INSERT OR IGNORE INTO file_map (file_path) VALUES (?1)",
        params![canonical],
    ) {
        eprintln!("reindex file_map INSERT: {}", e);
        return false;
    }
    let file_id: i64 = match db.query_row(
        "SELECT id FROM file_map WHERE file_path = ?1",
        params![canonical],
        |row| row.get(0),
    ) {
        Ok(id) => id,
        Err(e) => { eprintln!("reindex file_map SELECT: {}", e); return false; }
    };

    // V73: extract via the SHARED pipeline (stem_identifier, keywords, is_def,
    // zones, blocks). The old code used `tokenize()` (porter_stem) and wrote a
    // flags=0 stub — both silently degraded search after any edit.
    let (phrase_locations, line_count) = reliary_search::ingest::extract_file_phrases(content);
    let token_len = phrase_locations.len() as i64;
    let content_len = content.len() as i64;

    // V73: all-or-nothing transaction with a rollback guard. Every early
    // return after BEGIN must not leave the transaction open — on the shared
    // MCP connection that poisons every later JIT build.
    if let Err(e) = db.execute_batch("BEGIN IMMEDIATE;") {
        eprintln!("reindex BEGIN: {}", e);
        return false;
    }
    struct TxGuard<'a> {
        conn: &'a rusqlite::Connection,
        committed: std::cell::Cell<bool>,
    }
    impl Drop for TxGuard<'_> {
        fn drop(&mut self) {
            if !self.committed.get() {
                let _ = self.conn.execute_batch("ROLLBACK;");
            }
        }
    }
    let tx_guard = TxGuard { conn: &db, committed: std::cell::Cell::new(false) };
    macro_rules! fail {
        ($($arg:tt)*) => {{
            eprintln!($($arg)*);
            return false; // tx_guard drops -> ROLLBACK
        }};
    }

    if let Err(e) = db.execute("DELETE FROM occurrence WHERE file_id = ?1", params![file_id]) {
        fail!("reindex DELETE occurrence: {}", e);
    }
    if let Err(e) = db.execute("DELETE FROM block WHERE file_id = ?1", params![file_id]) {
        fail!("reindex DELETE block: {}", e);
    }
    // V9 C1 removed the scope_binding and method_occurrence tables; guard on
    // table existence so legacy indexes still get cleaned.
    for legacy in ["scope_binding", "method_occurrence"] {
        let exists: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                params![legacy],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if exists > 0 {
            if let Err(e) = db.execute(&format!("DELETE FROM {} WHERE file_id = ?1", legacy), params![file_id]) {
                fail!("reindex DELETE {}: {}", legacy, e);
            }
        }
    }

    // C7: strip this file's entry from ALL phrase_occ blobs, using the
    // file_phrases side table. Errors PROPAGATE — a partial strip followed by
    // an append would duplicate entries in the committed blob.
    {
        let mut affected_phrases: Vec<i64> = Vec::new();
        {
            let mut stmt = match db.prepare("SELECT phrase_id FROM file_phrases WHERE file_id = ?1") {
                Ok(s) => s,
                Err(e) => fail!("reindex prepare file_phrases: {}", e),
            };
            let rows = match stmt.query_map(params![file_id], |r| r.get::<_, i64>(0)) {
                Ok(rows) => rows,
                Err(e) => fail!("reindex query file_phrases: {}", e),
            };
            for row in rows.flatten() { affected_phrases.push(row); }
        }
        for pid in &affected_phrases {
            // V73: a read error must NOT be treated as "no row" — that would
            // overwrite the blob with only this file's entry, deleting every
            // other file's occurrences for the phrase. CAST handles legacy
            // TEXT-coerced blobs from the pre-V72 bug.
            let blob: Option<Vec<u8>> = match db.query_row(
                "SELECT CAST(file_blob AS BLOB) FROM phrase_occ WHERE phrase_id = ?1",
                params![pid],
                |r| r.get(0),
            ) {
                Ok(b) => Some(b),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => fail!("reindex read blob pid={}: {}", pid, e),
            };
            let blob = match blob {
                Some(b) => b,
                None => continue,
            };
            // V74: a truncated/malformed blob would silently lose entries on
            // rewrite. Log it (the entries that DO parse are kept, and the
            // reindexed file's own entry is re-added below).
            if !reliary_search::schema::blob_is_well_formed(&blob) {
                eprintln!("reindex: malformed blob for phrase_id={} ({} bytes) — entries may be partial", pid, blob.len());
            }
            let entries: Vec<(i64, u8)> = reliary_search::schema::unpack_file_blob(&blob)
                .filter(|(fid, _)| *fid != file_id)
                .collect();
            if entries.is_empty() {
                if let Err(e) = db.execute("DELETE FROM phrase_occ WHERE phrase_id = ?1", params![pid]) {
                    fail!("reindex DELETE phrase_occ: {}", e);
                }
            } else {
                let new_blob = reliary_search::schema::pack_file_blob(&entries);
                if let Err(e) = db.execute(
                    "UPDATE phrase_occ SET file_blob = ?1 WHERE phrase_id = ?2",
                    params![&new_blob[..], pid],
                ) {
                    fail!("reindex UPDATE phrase_occ: {}", e);
                }
            }
        }
    }
    if let Err(e) = db.execute("DELETE FROM file_phrases WHERE file_id = ?1", params![file_id]) {
        fail!("reindex DELETE file_phrases: {}", e);
    }

    // Step 2: resolve phrase_ids for the file's tokens (insert new phrases).
    let mut affected_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    {
        let mut insert_stmt = match db.prepare("INSERT OR IGNORE INTO phrases (phrase) VALUES (?1)") {
            Ok(s) => s,
            Err(e) => fail!("reindex prepare phrase INSERT: {}", e),
        };
        let mut select_stmt = match db.prepare("SELECT id FROM phrases WHERE phrase = ?1") {
            Ok(s) => s,
            Err(e) => fail!("reindex prepare phrase SELECT: {}", e),
        };
        for phrase in phrase_locations.keys() {
            if let Err(e) = insert_stmt.execute(params![phrase]) {
                fail!("reindex INSERT phrase {:?}: {}", phrase, e);
            }
            match select_stmt.query_row(params![phrase], |row| row.get::<_, i64>(0)) {
                Ok(pid) => { affected_ids.insert(pid); }
                Err(e) => fail!("reindex SELECT phrase {:?}: {}", phrase, e),
            }
        }
    }

    // Step 3: UPSERT new (phrase, file) entries with REAL flags computed from
    // the same data trust uses (is_def_any, avg zone, occurrence count).
    for pid in &affected_ids {
        // Look up this pid's phrase text, then its locations.
        let phrase_text: Option<String> = db
            .query_row("SELECT phrase FROM phrases WHERE id = ?1", params![pid], |r| r.get(0))
            .ok(); // GUARDED: intentional — missing phrase means the token vanished; skip below
        let phrase_text = match phrase_text {
            Some(t) => t,
            None => continue,
        };
        let empty: Vec<(usize, u8, usize, bool, usize, u8)> = Vec::new();
        let locs = phrase_locations.get(&phrase_text).unwrap_or(&empty);
        let num_locs = locs.len().max(1) as u32;
        let avg_zone = locs.iter().map(|(_, z, _, _, _, _)| *z as u32).sum::<u32>() / num_locs;
        let is_def_any = locs.iter().any(|(_, _, _, d, _, _)| *d);
        let flags = reliary_search::schema::pack_flags(if is_def_any { 1 } else { 0 }, avg_zone as i32, num_locs);

        // Read-modify-write append (V72: SQLite `||`/`concat()` coerce BLOBs to
        // TEXT). CAST handles legacy TEXT rows; a read error propagates.
        let existing: Option<Vec<u8>> = match db.query_row(
            "SELECT CAST(file_blob AS BLOB) FROM phrase_occ WHERE phrase_id = ?1",
            params![pid],
            |r| r.get(0),
        ) {
            Ok(b) => Some(b),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => fail!("reindex read blob pid={}: {}", pid, e),
        };
        let mut merged = existing.unwrap_or_default();
        let mut entry: Vec<u8> = Vec::with_capacity(3);
        reliary_search::schema::encode_varint(file_id, &mut entry);
        entry.push(flags[0]);
        merged.extend_from_slice(&entry);
        if let Err(e) = db.execute(
            "INSERT INTO phrase_occ (phrase_id, file_blob) VALUES (?1, ?2)
             ON CONFLICT(phrase_id) DO UPDATE SET file_blob = excluded.file_blob",
            params![pid, &merged[..]],
        ) {
            fail!("reindex phrase_occ UPSERT pid={}: {}", pid, e);
        }
        // V73: repopulate file_phrases so the NEXT reindex can strip this entry.
        if let Err(e) = db.execute(
            "INSERT OR IGNORE INTO file_phrases (file_id, phrase_id) VALUES (?1, ?2)",
            params![file_id, pid],
        ) {
            fail!("reindex INSERT file_phrases pid={}: {}", pid, e);
        }
    }

    // file_stats (BM25 doc length). Trust writes this; reindex must too, or
    // reindexed files fall back to COALESCE(...,50) and rank wrong.
    if let Err(e) = db.execute(
        "INSERT INTO file_stats (file_id, token_len, content_len) VALUES (?1, ?2, ?3)
         ON CONFLICT(file_id) DO UPDATE SET token_len=excluded.token_len, content_len=excluded.content_len",
        params![file_id, token_len, content_len],
    ) {
        fail!("reindex file_stats: {}", e);
    }

    if let Err(e) = db.execute_batch("COMMIT;") {
        fail!("reindex COMMIT: {}", e);
    }
    tx_guard.committed.set(true);

    // C6: rebuild lazy tables AFTER COMMIT (outside transaction). If process
    // crashes here, the phrase_occ (file-level search) is consistent and the
    // lazy tables rebuild on next query.
    if let Err(e) = reliary_search::lazy_tables::ensure_all_for_file_with_content(&db, file_id, content) {
        eprintln!("reindex lazy_tables: {}", e);
    }

    // V60: the file's occurrence rows changed — invalidate the result cache.
    reliary_search::lazy_occurrence::invalidate_all_phrase_gens();
    // M4: bump the persisted index generation so the freshness stamp changes
    // for every MCP client on their next call.
    if let Err(e) = reliary_search::schema::bump_index_gen(&db) {
        eprintln!("reindex bump_index_gen: {}", e);
    }

    true
}

/// Re-index a single file. Public API for gate.js / hook layer to call after every edit.
/// Returns the number of phrases indexed (0 if file not indexed).
pub fn reindex_single_file(db_path: &str, file: &str, content: &str) -> usize {
    if !std::path::Path::new(db_path).exists() {
        return 0;
    }
    if reindex_file(db_path, file, content) {
        reliary_search::ingest::extract_file_phrases(content).0.len()
    } else {
        0
    }
}
