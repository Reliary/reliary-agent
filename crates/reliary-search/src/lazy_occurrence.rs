//! Arc 34 Step 5: lazy occurrence build.
//!
//! At trust time, we skip inserting rows into the `occurrence` table (saves
//! 41-50% of total ingest time on Linux kernel). At first find_references
//! query for a phrase, we JIT-build the occurrence rows for that phrase from
//! source files on disk.
//!
//! Cache strategy: a single SQL check `SELECT 1 FROM occurrence WHERE
//! phrase_id = ? LIMIT 1` is fast with the (phrase_id, is_def, file_id)
//! composite index. If 0 rows, we read phrase_occ to get file_ids, read each
//! source file, scan for the stemmed phrase, INSERT into occurrence.
//!
//! Grammar-free: the scan uses the same `porter_stem` + `scan_identifiers`
//! pipeline as the indexer. Same detection logic = same hits.

use rusqlite::{params, Connection};
use rustc_hash::FxHashMap;
use std::fs;

/// V58b: PER-PHRASE generation counters. The global counter (V58) was too
/// coarse — any JIT build anywhere invalidated every cached result, so
/// cross-query repeats never hit. A repeat is now served from cache iff THAT
/// phrase's occurrence rows are unchanged.
static PHRASE_GENS: std::sync::Mutex<Option<FxHashMap<i64, u64>>> = std::sync::Mutex::new(None);

fn phrase_gens() -> FxHashMap<i64, u64> {
    let mut g = PHRASE_GENS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(FxHashMap::default).clone()
}

fn phrase_gens_store<F: FnOnce(&mut FxHashMap<i64, u64>)>(f: F) {
    let mut g = PHRASE_GENS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(FxHashMap::default));
}

/// Current generation for one phrase (0 if never built).
pub fn phrase_generation(phrase_id: i64) -> u64 {
    phrase_gens().get(&phrase_id).copied().unwrap_or(0)
}

#[inline]
pub(crate) fn bump_gen_if_inserted(n: usize) {
    let _ = n; // retained for callers; per-phrase bumps happen in ensure_* paths
}


/// Full invalidation (file-level rebuild touches many phrases).
pub fn invalidate_all_phrase_gens() {
    phrase_gens().clear();
}

#[inline]
fn bump_phrase_gen(_db: &Connection, phrase_id: i64) {
    phrase_gens_store(|m| { *m.entry(phrase_id).or_insert(0) += 1; });
}

/// Grammar-free content classifier: determines if a file is source-like
/// by examining its line structure, not its file extension.
///
/// Source files have lines that look like definitions (identifiers followed
/// by `(`, lines ending with `{`, `=>`, `->`), while JSON/markdown/config
/// files have lines that look like data (`key: value`, `key=value`, bare
/// strings, or prose).
///
/// Returns true if >30% of non-blank sampled lines look code-bearing.
pub fn is_source_like(content: &str) -> bool {
    let all_lines: Vec<&str> = content.lines().collect();
    if all_lines.is_empty() { return false; }

    // Sample 100 lines evenly distributed across the file (not just the
    // first 100 — the header may be all doc comments).
    let n = all_lines.len();
    let sample_count = n.min(100);
    let sample: Vec<&str> = if n <= sample_count {
        all_lines.clone()
    } else {
        (0..sample_count).map(|i| all_lines[i * n / sample_count]).collect()
    };

    let mut code_lines = 0usize;
    let mut non_blank = 0usize;

    for line in &sample {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        non_blank += 1;

        // Comment-like: starts with //, #, *, -, >, <!--
        if trimmed.starts_with("//") || trimmed.starts_with('#')
            || trimmed.starts_with('*') || trimmed.starts_with("<!--")
            || (trimmed.starts_with("- ") && !trimmed.starts_with("-- "))
        {
            continue; // comment, not code
        }

        // Data-like: looks like JSON key-value ("key": value) or bare string
        // JSON objects/arrays start with { or [
        if (trimmed.starts_with('"') && trimmed.contains("\":"))
            || trimmed.starts_with("{\"") || trimmed.starts_with("[{")
            || trimmed.starts_with('[') && trimmed.ends_with(']')
            || trimmed.starts_with('{') && trimmed.ends_with('}')
        {
            continue; // data, not code
        }

        // Code-bearing signals:
        // 1. Line ends with { (function/struct/enum/block start)
        // 2. Line has identifier( pattern (function call or definition)
        // 3. Line ends with => or -> (arrow function or return type)
        // 4. Line has let/const/var/fn/def/pub/mod/impl/struct/enum/use/import
        // 5. Line ends with ; and contains = (assignment)
        let ends_with_brace = trimmed.ends_with('{');
        // L3: early-exit on lines that don't contain `(` (cheaper than walking).
        let has_call_pattern = trimmed.contains('(')
            && !trimmed.starts_with('(')
            && !trimmed.starts_with('"')
            && trimmed.as_bytes().iter().any(|&c| c.is_ascii_alphabetic() || c == b'_');
        let has_arrow = trimmed.ends_with("=>") || trimmed.ends_with("->");
        let has_keyword = {
            let lower = trimmed.to_lowercase();
            lower.starts_with("let ") || lower.starts_with("const ")
                || lower.starts_with("var ") || lower.starts_with("fn ")
                || lower.starts_with("def ") || lower.starts_with("pub ")
                || lower.starts_with("mod ") || lower.starts_with("impl ")
                || lower.starts_with("struct ") || lower.starts_with("enum ")
                || lower.starts_with("use ") || lower.starts_with("import ")
                || lower.starts_with("async ") || lower.starts_with("export ")
                || lower.starts_with("class ") || lower.starts_with("static ")
                || lower.starts_with("func ") || lower.starts_with("package ")
                || lower.starts_with("type ") || lower.starts_with("trait ")
        };
        let has_assignment = trimmed.contains('=') && trimmed.ends_with(';')
            && !trimmed.starts_with('=');

        if ends_with_brace || has_arrow || has_keyword
            || (has_call_pattern && !trimmed.ends_with(','))
            || has_assignment
        {
            code_lines += 1;
        }
    }

    if non_blank == 0 {
        eprintln!("[guard:is_source_like] skipped (empty content)");
        return false;
    }
    let ratio = code_lines as f64 / non_blank as f64;
    if ratio <= 0.10 {
        eprintln!("[guard:is_source_like] skipped (ratio={:.3} <= 0.10, code={}, non_blank={})", ratio, code_lines, non_blank);
        return false;
    }
    true
}

/// Check if occurrence rows already exist for a phrase.
/// Used as a fast guard before JIT-building.
pub fn has_occurrence(db: &Connection, phrase_id: i64) -> rusqlite::Result<bool> {
    let mut stmt = db.prepare_cached(
        "SELECT 1 FROM occurrence WHERE phrase_id = ?1 LIMIT 1"
    )?;
    let mut rows = stmt.query(params![phrase_id])?;
    Ok(rows.next()?.is_some())
}

/// V13: per-file occurrence check. Returns true if this file already has
/// occurrence rows for this phrase. Used by ensure_occurrence_for_phrase
/// to avoid redundant work on files already populated.
pub fn occurrence_has_file(db: &Connection, file_id: i64, phrase_id: i64) -> rusqlite::Result<bool> {
    let mut stmt = db.prepare_cached(
        "SELECT 1 FROM occurrence WHERE file_id = ?1 AND phrase_id = ?2 LIMIT 1"
    )?;
    let mut rows = stmt.query(params![file_id, phrase_id])?;
    Ok(rows.next()?.is_some())
}

/// Get all file_ids that contain a phrase, in (file_id ASC) order.
/// Used to drive JIT source reads.
///
/// Arc 37 schema v4: phrase_occ.file_blob is a packed list of (varint file_id,
/// flags[1]) entries. Unpack the blob, then look up paths in file_map via IN.
pub fn file_ids_for_phrase(db: &Connection, phrase_id: i64) -> rusqlite::Result<Vec<(i64, String)>> {
    let file_blob: Option<Vec<u8>> = db.query_row(
        "SELECT file_blob FROM phrase_occ WHERE phrase_id = ?1",
        params![phrase_id],
        |r| r.get(0),
    )?;
    let file_blob = match file_blob { Some(b) => b, None => return Ok(vec![]) };

    let file_ids: Vec<i64> = crate::schema::unpack_file_blob(&file_blob)
        .map(|(fid, _)| fid)
        .collect();
    if file_ids.is_empty() { return Ok(vec![]); }

    // Look up file paths via IN clause.
    let placeholders = std::iter::repeat("?").take(file_ids.len()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, file_path FROM file_map WHERE id IN ({}) ORDER BY id ASC",
        placeholders
    );
    let mut stmt = db.prepare(&sql)?;
    let mut params_v: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(file_ids.len());
    for fid in &file_ids {
        params_v.push(Box::new(*fid));
    }
    let params_refs: Vec<&dyn rusqlite::ToSql> = params_v.iter().map(|b| b.as_ref()).collect();
    let mut rows = stmt.query(params_refs.as_slice())?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push((r.get(0)?, r.get(1)?));
    }
    Ok(out)
}

/// Get the actual phrase string for a phrase_id (used to scan source files).
fn phrase_text_for(db: &Connection, phrase_id: i64) -> rusqlite::Result<Option<String>> {
    let mut stmt = db.prepare_cached("SELECT phrase FROM phrases WHERE id = ?1")?;
    let mut rows = stmt.query(params![phrase_id])?;
    if let Some(r) = rows.next()? {
        Ok(Some(r.get(0)?))
    } else {
        Ok(None)
    }
}

/// Get block_id for a (file_id, line), computing if missing.
/// Lazy: if the block table is empty for this file, scan the source to
/// compute blocks on demand. For typical repos, the block table IS built
/// at trust time, so this is just an indexed lookup.
fn block_id_at_line(db: &Connection, file_id: i64, line: i32) -> rusqlite::Result<i64> {
    let mut stmt = db.prepare_cached(
        "SELECT block_id FROM block
         WHERE file_id = ?1 AND start_line <= ?2 AND end_line >= ?2
         ORDER BY (end_line - start_line) ASC LIMIT 1"
    )?;
    let mut rows = stmt.query(params![file_id, line])?;
    if let Some(r) = rows.next()? {
        Ok(r.get(0)?)
    } else {
        Ok(0)
    }
}

/// JIT build occurrence rows for a phrase by scanning source files.
/// Returns count of inserted rows.
///
/// Strategy: load source files, scan for tokens whose porter_stem matches
/// the target phrase. Insert one row per match with line, col, is_def, tag,
/// block_id. We use the same `scan_identifiers` + `porter_stem` + zone +
/// classify-line-tag pipeline as the indexer.
pub fn ensure_occurrence_for_phrase(
    db: &Connection,
    phrase_id: i64,
) -> rusqlite::Result<usize> {
    // V13: do NOT short-circuit on per-phrase has_occurrence. We need per-file
    // granularity because multiple files can share a phrase, and we must
    // populate all of them. The old check skipped remaining files once ANY
    // file had rows, meaning files later in phrase_occ blob order (like
    // runtime/runtime.rs for block_on) were never populated.

    let phrase_text = match phrase_text_for(db, phrase_id)? {
        Some(t) => t,
        None => return Ok(0),
    };

    let file_ids = file_ids_for_phrase(db, phrase_id)?;
    if file_ids.is_empty() {
        return Ok(0);
    }

    // Insert in a single transaction for speed.
    db.execute_batch("BEGIN IMMEDIATE")?;

    let mut stmt = db.prepare_cached(
        "INSERT INTO occurrence (phrase_id, file_id, line, col, is_def, block_id, tag)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
    )?;

    let mut total = 0usize;
    for (file_id, file_path) in &file_ids {
        // V13: per-file check — don't skip if another file already has rows
        // for this phrase.
        if occurrence_has_file(db, *file_id, phrase_id)? {
            continue;
        }
        let content = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // V54: use the pre-computed is_source column (set at trust time) instead
        // of re-scanning the whole file content per phrase. O(1) vs O(n).
        let is_source_col: bool = db.query_row(
            "SELECT is_source FROM file_map WHERE id = ?1",
            params![*file_id], |r| r.get(0),
        ).unwrap_or(true);
        if !is_source_col {
            continue;
        }

        let lines: Vec<&str> = content.lines().collect();

        // V54: batch-load block ranges for this file ONCE — binary search per
        // line instead of one SQL round-trip per matching token.
        let block_ranges: Vec<(i64, i32, i32)> = {
            let mut stmt = db.prepare_cached(
                "SELECT block_id, start_line, end_line FROM block WHERE file_id = ?1"
            )?;
            let mut rows = stmt.query(params![*file_id])?;
            let mut v = Vec::new();
            while let Some(r) = rows.next()? {
                v.push((r.get(0)?, r.get(1)?, r.get(2)?));
            }
            v
        };
        let block_id_for_line = |line_no: i32| -> i64 {
            let mut best: i64 = 0;
            let mut best_span: i32 = i32::MAX;
            for &(bid, sl, el) in &block_ranges {
                if sl <= line_no && line_no <= el {
                    let span = el - sl;
                    if span < best_span {
                        best_span = span;
                        best = bid;
                    }
                }
            }
            best
        };

        // Pre-compute line_tags (small array, reused).
        let mut line_tags: Vec<u8> = Vec::with_capacity(lines.len());
        let mut brace_depth: i32 = 0;
        // V59: capture the DEFINED NAME per line, not just a bool. The old
        // code marked every identifier on a def-line as is_def=1 — so
        // `StructuralResult` in a fn's return type inherited the fn's def
        // flag, and def-lookup returned the fn line for struct queries.
        let mut line_def_names: Vec<Option<String>> = Vec::with_capacity(lines.len());
        for line in &lines {
            let (open_count, close_count) = crate::ingest::count_braces(line);
            let prev_depth = brace_depth;
            brace_depth += open_count as i32 - close_count as i32;
            if brace_depth < 0 { brace_depth = 0; }
            let result = crate::structural::classify_structural(line, prev_depth, open_count > 0, false);
            line_tags.push(result.tag);
            line_def_names.push(result.defined_name.map(|s| s.to_string()));
        }

        let mut jit_debug_hits = 0usize;
        for (li, line) in lines.iter().enumerate() {
            let line_tag = line_tags.get(li).copied().unwrap_or(0);
            let line_def_name = line_def_names.get(li).cloned().flatten();
            for (col, token) in crate::scan_identifiers(line).into_iter().enumerate() {
                let stemmed = crate::stem_identifier(&token);
                if stemmed == phrase_text { jit_debug_hits += 1; }
                if stemmed != phrase_text {
                    continue;
                }
                if crate::keywords::is_keyword(&stemmed) {
                    continue;
                }
                let col_idx = col as i32;
                let line_no = li as i32;  // 0-based, matches MCP `+1` convention
                // V59: is_def iff this token IS the line's defined name.
                // Case-insensitive: scan_identifiers lowercases, the
                // classifier returns source-case names.
                let is_def_int = if line_tag >= 1 && line_tag <= 4
                    && line_def_name.as_deref().map(|d| d.eq_ignore_ascii_case(&token)).unwrap_or(false)
                { 1 } else { 0 };
                let tag = if is_def_int == 1 { line_tag } else { 0 };
                let block_id = block_id_for_line(line_no);
                stmt.execute(params![phrase_id, *file_id, line_no, col_idx, is_def_int, block_id, tag as i64])?;
                total += 1;
            }
        }
    }

    db.execute_batch("COMMIT")?;

    eprintln!("[jit-debug] phrase={} total={} (probe removed from scope)", phrase_text, total);
    { if total > 0 { bump_phrase_gen(db, phrase_id); } Ok(total) }
}

/// Bulk JIT build for multiple phrases. Used by `reliary_reindex_all_occurrences`
/// CLI and the `reliary_risk` / `reliary_architecture` tools that need
/// occurrence-level data for many phrases.
pub fn ensure_occurrence_for_phrases(
    db: &Connection,
    phrase_ids: &[i64],
) -> rusqlite::Result<usize> {
    let mut total = 0;
    for pid in phrase_ids {
        total += ensure_occurrence_for_phrase(db, *pid)?;
    }
    { bump_gen_if_inserted(total); Ok(total) }
}

/// Count distinct phrases that don't yet have occurrence rows.
/// Used by the architecture / risk tools to decide whether to JIT-build.
pub fn count_unresolved_phrases(db: &Connection) -> rusqlite::Result<i64> {
    let mut stmt = db.prepare_cached(
        "SELECT COUNT(*) FROM phrases p
         WHERE NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.phrase_id = p.id)"
    )?;
    let n: i64 = stmt.query_row([], |r| r.get(0))?;
    Ok(n)
}

/// Resolve all unresolved phrases by JIT-building them in dependency order.
/// Used by `reliary_reindex --build-occurrences` for power users who want
/// the full 97% coverage immediately.
///
/// P4-1: File-driven loop. Old: phrase-driven — read each file N times (once per phrase).
/// New: iterate over distinct files in phrase_occ, call ensure_occurrence_for_file
/// once per file. O(F) disk reads instead of O(N×F).
///
/// P4-4: Use a JOIN instead of a correlated subquery in the file-id selection.
pub fn build_all_occurrence(db: &Connection) -> rusqlite::Result<usize> {
    // V21 fix: phrase_occ doesn't have a file_id column — it has phrase_id + file_blob.
    // Query file_map for all file IDs instead.
    let mut stmt = db.prepare_cached(
        "SELECT id FROM file_map ORDER BY id"
    )?;
    let ids: Vec<i64> = stmt.query_map([], |r| r.get::<_, i64>(0))?
        .filter_map(|x| x.ok())
        .collect();
    drop(stmt);
    // V52: Load phrase cache once, share across all files. Was: per-file full table scan.
    let mut phrase_cache = load_phrase_cache(db);
    let mut total = 0usize;
    for fid in ids {
        total += ensure_occurrence_for_file_with_cache(db, fid, &mut phrase_cache).unwrap_or(0);
    }
    { bump_gen_if_inserted(total); Ok(total) }
}

/// V52: Load the full phrases table into an FxHashMap. Called once per build_all_occurrence,
/// shared across all file processing. Replaces the per-file full-table-scan.
fn load_phrase_cache(db: &Connection) -> FxHashMap<String, i64> {
    let mut cache: FxHashMap<String, i64> = FxHashMap::default();
    if let Ok(mut stmt) = db.prepare_cached("SELECT id, phrase FROM phrases ORDER BY id") {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
            for row in rows.flatten() {
                cache.insert(row.1, row.0);
            }
        }
    }
cache
}

pub fn ensure_occurrence_for_file(db: &Connection, file_id: i64) -> rusqlite::Result<usize> {
    // V52: Build a local cache for this file (no shared state across calls).
    let mut cache: FxHashMap<String, i64> = FxHashMap::default();
    ensure_occurrence_for_file_impl(db, file_id, &mut cache)
}

/// Shared implementation for both ensure_occurrence_for_file and
/// ensure_occurrence_for_file_with_cache. The caller passes a mutable
/// reference to their phrase cache; we populate it on first call.
fn ensure_occurrence_for_file_impl(
    db: &Connection, file_id: i64,
    phrase_cache: &mut FxHashMap<String, i64>,
) -> rusqlite::Result<usize> {
    // Fast guard: skip if already populated.
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM occurrence WHERE file_id = ?1)",
        params![file_id],
        |r| r.get(0),
    )?;
    if exists {
        return Ok(0);
    }

    // Look up file path.
    let file_path: Option<String> = db.query_row(
        "SELECT file_path FROM file_map WHERE id = ?1",
        params![file_id],
        |r| r.get(0),
    )?;
    let path = match file_path { Some(p) => p, None => return Ok(0) };

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Ok(0),
    };

    // Grammar-free content filter: skip non-source files (JSON results,
    // markdown docs, config files) that pollute find_references output.
    if !is_source_like(&content) {
        return Ok(0);
    }

    let lines: Vec<&str> = content.lines().collect();

    // Pre-compute line_tags and is_def flags (brace-depth tracking).
    let mut line_tags: Vec<u8> = Vec::with_capacity(lines.len());
    let mut lines_is_def: Vec<bool> = Vec::with_capacity(lines.len());
    let mut brace_depth: i32 = 0;
    for line in &lines {
        let (open_count, close_count) = crate::ingest::count_braces(line);
        let prev_depth = brace_depth;
        brace_depth += open_count as i32 - close_count as i32;
        if brace_depth < 0 { brace_depth = 0; }
        let result = crate::structural::classify_structural(line, prev_depth, open_count > 0, false);
        line_tags.push(result.tag);
        lines_is_def.push(result.is_def);
    }

    // Single INSERT statement, reused for every token.
    let mut stmt = db.prepare_cached(
        "INSERT INTO occurrence (phrase_id, file_id, line, col, is_def, block_id, tag)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
    )?;

    // Per-file phrase cache: resolve phrase_id once, reuse for duplicate tokens.
    // V52: if caller provided a pre-loaded cache (size > 0), use it. Otherwise load fresh.
    // Calling convention: build_all_occurrence passes a shared cache pre-populated
    // with the full phrases table; single-file callers pass an empty cache.
    if phrase_cache.is_empty() {
        // S4: Pre-load all phrase_id mappings in one query. Avoids per-token
        // INSERT OR IGNORE + SELECT roundtrips during the line loop.
        if let Ok(mut stmt) = db.prepare_cached("SELECT id, phrase FROM phrases ORDER BY id") {
            if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
                for row in rows.flatten() {
                    phrase_cache.insert(row.1, row.0);
                }
            }
        }
    }

    let mut total = 0usize;
    db.execute_batch("BEGIN IMMEDIATE")?;

    // P2-5: ensure blocks ONCE before the line loop instead of per-line.
    if let Err(e) = crate::lazy_tables::ensure_blocks_for_file(db, file_id) {
        eprintln!("[lazy_occurrence] ensure_blocks_for_file failed for file_id={}: {}", file_id, e);
    }
    // Pre-build block_id lookup for all lines (avoids one SQL query per line).
    let block_ids: Vec<i64> = {
        let mut stmt = db.prepare_cached(
            "SELECT start_line, end_line, block_id FROM block WHERE file_id = ?1 ORDER BY start_line"
        )?;
        let ranges: Vec<(i32, i32, i64)> = stmt.query_map(params![file_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?.filter_map(|r| r.ok()).collect();
        let mut map = vec![0i64; lines.len()];
        for (start, end, bid) in ranges {
            let start = start.max(0) as usize;
            let end = (end.max(0) as usize).min(map.len().saturating_sub(1));
            for li in start..=end {
                map[li] = bid;
            }
        }
        map
    };

    // M6: Accumulate rows into a batch, flush in chunks of 500.
    // Multi-row INSERT is 3-10× faster than individual executes for large files.
    let mut batch: Vec<(i64, i64, i32, i32, i32, i64, i64)> = Vec::with_capacity(500);

    for (li, line) in lines.iter().enumerate() {
        let line_tag = line_tags.get(li).copied().unwrap_or(0);
        let line_is_def = lines_is_def.get(li).copied().unwrap_or(false);
        let is_def_int = if line_is_def { 1 } else { 0 };

        // Use pre-built block_id lookup (O(1) instead of per-line SQL).
        let block_id: i64 = *block_ids.get(li).unwrap_or(&0);

        // Path B: strip trailing // comments before scanning identifiers.
        let code = crate::structural::strip_line_comment(line);

        for (col, token) in crate::scan_identifiers(code).into_iter().enumerate() {
            // V39: use stem_identifier to preserve snake_case identifiers.
            // porter_stem strips the `al` suffix from `structural` → `structur`,
            // destroying compound names like `classify_structural`.
            let stemmed = crate::stem_identifier(&token);
            if crate::keywords::is_keyword(&stemmed) {
                continue;
            }
            // Lookup or insert phrase_id.
            let phrase_id = match phrase_cache.get(&stemmed) {
                Some(&id) => id,
                None => {
                    db.execute(
                        "INSERT OR IGNORE INTO phrases (phrase) VALUES (?1)",
                        params![stemmed],
                    )?;
                    let id: i64 = match db.query_row(
                        "SELECT id FROM phrases WHERE phrase = ?1",
                        params![stemmed],
                        |r| r.get(0),
                    ) {
                        Ok(id) => id,
                        Err(_) => continue,
                    };
                    phrase_cache.insert(stemmed.clone(), id);
                    id
                }
            };

            // Add to batch instead of executing immediately.
            batch.push((phrase_id, file_id, li as i32, col as i32, is_def_int, block_id, line_tag as i64));
            total += 1;

            // Flush batch every 500 rows.
            if batch.len() >= 500 {
                flush_occurrence_batch(db, &mut batch)?;
            }
        }
    }

    // Flush remaining rows.
    if !batch.is_empty() {
        flush_occurrence_batch(db, &mut batch)?;
    }

    // V52: The caller's phrase_cache is now &mut — we already wrote new entries into it
    // during the line loop (via phrase_cache.insert). No merge needed at function end.

    let commit_result = db.execute_batch("COMMIT");
    if let Err(e) = commit_result {
        eprintln!("[lazy_occurrence] COMMIT failed for file_id={}: {}", file_id, e);
        return Err(e);
    }
    { if total > 0 { invalidate_all_phrase_gens(); } Ok(total) }
}

/// V52: Same as ensure_occurrence_for_file but accepts a pre-loaded phrase cache.
/// Used by build_all_occurrence to share the phrases table across all files.
/// For a 13K-phrase corpus, this avoids 500+ full-table scans during build-all.
pub fn ensure_occurrence_for_file_with_cache(
    db: &Connection, file_id: i64, phrase_cache: &mut FxHashMap<String, i64>,
) -> rusqlite::Result<usize> {
    ensure_occurrence_for_file_impl(db, file_id, phrase_cache)
}

/// M6: Flush a batch of occurrence rows using a single multi-row INSERT.
/// Uses parameter binding in a loop over a single prepared statement,
/// all within the existing transaction. 3-10× faster than per-row execute.
fn flush_occurrence_batch(
    db: &Connection,
    batch: &mut Vec<(i64, i64, i32, i32, i32, i64, i64)>,
) -> rusqlite::Result<()> {
    if batch.is_empty() { return Ok(()); }
    let mut stmt = db.prepare_cached(
        "INSERT INTO occurrence (phrase_id, file_id, line, col, is_def, block_id, tag)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
    )?;
    // rusqlite doesn't have native multi-row INSERT binding, but we can
    // build a dynamic VALUES clause for 5-10× speedup on large batches.
    let n = batch.len();
    if n >= 50 {
        // Build VALUES clause with UNIQUE parameter numbers per row.
        // SQLite limit is 999 parameters per statement — cap at 142 rows (142×7=994).
        let cap = n.min(142);
        let placeholders: Vec<String> = (0..cap)
            .map(|_| "(?,?,?,?,?,?,?)".to_string())
            .collect();
        let sql = format!(
            "INSERT INTO occurrence (phrase_id, file_id, line, col, is_def, block_id, tag) VALUES {}",
            placeholders.join(",")
        );
        // Flatten params — only for the capped rows.
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(cap * 7);
        for row in batch.iter().take(cap) {
            params.push(Box::new(row.0));
            params.push(Box::new(row.1));
            params.push(Box::new(row.2));
            params.push(Box::new(row.3));
            params.push(Box::new(row.4));
            params.push(Box::new(row.5));
            params.push(Box::new(row.6));
        }
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref() as &dyn rusqlite::ToSql).collect();
        db.execute(&sql, rusqlite::params_from_iter(param_refs))?;
        // If we capped, remaining rows go through per-row execute.
        if cap < n {
            for row in batch.iter().skip(cap) {
                stmt.execute(params![row.0, row.1, row.2, row.3, row.4, row.5, row.6])?;
            }
        }
    } else {
        // Small batch: individual executes within the transaction (still fast).
        for row in batch.iter() {
            stmt.execute(params![row.0, row.1, row.2, row.3, row.4, row.5, row.6])?;
        }
    }
    batch.clear();
    Ok(())
}

/// Content-aware variant: uses pre-read content instead of re-reading from disk.
pub fn ensure_occurrence_for_file_with_content(db: &Connection, file_id: i64, content: &str) -> rusqlite::Result<usize> {
    // Fast guard: skip if already populated.
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM occurrence WHERE file_id = ?1)",
        params![file_id],
        |r| r.get(0),
    )?;
    if exists {
        return Ok(0);
    }

    if !is_source_like(content) {
        return Ok(0);
    }

    let lines: Vec<&str> = content.lines().collect();

    let mut line_tags: Vec<u8> = Vec::with_capacity(lines.len());
    let mut lines_is_def: Vec<bool> = Vec::with_capacity(lines.len());
    let mut brace_depth: i32 = 0;
    for line in &lines {
        let (open_count, close_count) = crate::ingest::count_braces(line);
        let prev_depth = brace_depth;
        brace_depth += open_count as i32 - close_count as i32;
        if brace_depth < 0 { brace_depth = 0; }
        let result = crate::structural::classify_structural(line, prev_depth, open_count > 0, false);
        line_tags.push(result.tag);
        lines_is_def.push(result.is_def);
    }

    let mut stmt = db.prepare_cached(
        "INSERT INTO occurrence (phrase_id, file_id, line, col, is_def, block_id, tag)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
    )?;

    let mut phrase_cache: FxHashMap<String, i64> = FxHashMap::default();
    // S4: Pre-load all phrase_id mappings in one query.
    if let Ok(mut stmt) = db.prepare_cached("SELECT id, phrase FROM phrases ORDER BY id") {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
            for row in rows.flatten() {
                phrase_cache.insert(row.1, row.0);
            }
        }
    }
    let mut total = 0usize;
    db.execute_batch("BEGIN IMMEDIATE")?;

    // P4-3: Hoist ensure_blocks_for_file_with_content out of the per-line loop.
    // It was running SELECT EXISTS(...) once per line — L redundant queries per file.
    // Also pre-build block_ids array (mirrors ensure_occurrence_for_file which
    // already hoisted this).
    if let Err(e) = crate::lazy_tables::ensure_blocks_for_file_with_content(db, file_id, content) {
        eprintln!("[lazy_occurrence] ensure_blocks_for_file_with_content failed for file_id={}: {}", file_id, e);
    }
    // V52: Batch the block_id lookup — was: one SQL query per line (L rounds for L-line files).
    // Now: one query gets all ranges, Rust loop populates the map.
    let block_ids: Vec<i64> = {
        let mut stmt = db.prepare_cached(
            "SELECT start_line, end_line, block_id FROM block WHERE file_id = ?1 ORDER BY start_line"
        )?;
        let ranges: Vec<(i32, i32, i64)> = stmt.query_map(params![file_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?.filter_map(|r| r.ok()).collect();
        let mut map = vec![0i64; lines.len()];
        for (start, end, bid) in ranges {
            let start = start.max(0) as usize;
            let end = (end.max(0) as usize).min(map.len().saturating_sub(1));
            for li in start..=end {
                map[li] = bid;
            }
        }
        map
    };

    for (li, line) in lines.iter().enumerate() {
        let line_tag = line_tags.get(li).copied().unwrap_or(0);
        let line_is_def = lines_is_def.get(li).copied().unwrap_or(false);
        let is_def_int = if line_is_def { 1 } else { 0 };

        // P4-3: use pre-built block_ids array (hoisted out of the loop).
        let block_id = block_ids.get(li).copied().unwrap_or(0);

        for (col, token) in crate::scan_identifiers(line).into_iter().enumerate() {
            // V41: use stem_identifier to preserve snake_case identifiers.
            // porter_stem strips the `al` suffix from `structural` → `structur`,
            // which makes `classify_structural` unsearchable. stem_identifier
            // preserves the full identifier when it contains underscores.
            let stemmed = crate::stem_identifier(&token);
            if crate::keywords::is_keyword(&stemmed) {
                continue;
            }
            let phrase_id = match phrase_cache.get(&stemmed) {
                Some(&id) => id,
                None => {
                    db.execute(
                        "INSERT OR IGNORE INTO phrases (phrase) VALUES (?1)",
                        params![stemmed],
                    )?;
                    let id: i64 = match db.query_row(
                        "SELECT id FROM phrases WHERE phrase = ?1",
                        params![stemmed],
                        |r| r.get(0),
                    ) {
                        Ok(id) => id,
                        Err(_) => continue,
                    };
                    phrase_cache.insert(stemmed.clone(), id);
                    id
                }
            };

            let col_idx = col as i32;
            let line_no = li as i32;
            stmt.execute(params![phrase_id, file_id, line_no, col_idx, is_def_int, block_id, line_tag as i64])?;
            total += 1;
        }
    }

    db.execute_batch("COMMIT")?;
    { if total > 0 { invalidate_all_phrase_gens(); } Ok(total) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_occurrence_empty() {
        // Build an in-memory DB with schema.
        let db = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&db).unwrap();
        // No phrases — should be false.
        assert!(!has_occurrence(&db, 1).unwrap());
    }

    #[test]
    fn test_count_unresolved_zero() {
        let db = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&db).unwrap();
        assert_eq!(count_unresolved_phrases(&db).unwrap(), 0);
    }
}