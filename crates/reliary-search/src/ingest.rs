//! File walking, tokenization, and index insertion.
//! Uses Rayon for parallel parsing and FxHashMap for speed.
//!
//! Schema v2 additions (occurrence-level vocab):
//! - Computes indentation-anchored block boundaries (blank-line / EOF / dedent).
//! - Persists every occurrence into `occurrence` (phrase_id, file_id, line, col, is_def, block_id).
//! - Persists blocks into `block` (block_id, file_id, start_line, end_line, indent).
//! - Keeps the file-level `phrase_occ` table unchanged for BM25/proximity search (additive).
//!
//! Block detection is grammar-free: a block ends at a blank line, at EOF, or when a
//! non-empty line's indent drops below the block's leading-indent threshold.

/// S6: Shared skip list for build/dependency directories. Used by both ingest
/// (walkdir filter) and pack (is_source_file). One source of truth prevents drift.
pub const SKIP_DIRS: &[&str] = &[
    "target", "node_modules", "dist", "build",
    "vendor", ".next", ".cache", "out",
    "__pycache__", ".venv", "venv", ".gradle",
];

use rusqlite::{params, Connection};
use walkdir::WalkDir;
use rustc_hash::FxHashMap;
use rayon::prelude::*;

use crate::schema::{classify_line, pack_flags};
use crate::{scan_identifiers, stem_identifier, is_likely_binary};

// A-HIGH-6: block-comment-aware state tracking across lines.
// Static assert ensures single-threaded usage (count_unmatched is always
// called from a serial per-file loop, never concurrent on the same file).
thread_local! { static IN_BLOCK_COMMENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

/// Count unmatched occurrences of a byte in a line (outside strings).
/// A-HIGH-6: skips `//` comments, `/* */` block comments (multi-line via
/// thread-local `in_block_comment` state), and char/string literals.
/// `in_block_comment` is thread-local, so this is NOT re-entrant for the
/// same file (which it never needs to be — callers are serial per file).
pub fn count_unmatched(line: &str, target: u8) -> usize {
    IN_BLOCK_COMMENT.with(|in_bc| {
        let bytes = line.as_bytes();
        let mut count = 0;
        let mut in_string = false;
        let mut escape = false;
        let mut in_char = false;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if escape { escape = false; i += 1; continue; }
            if b == b'\\' && (in_string || in_char) { escape = true; i += 1; continue; }
            if b == b'"' && !in_char { in_string = !in_string; i += 1; continue; }
            if b == b'\'' && !in_string { in_char = !in_char; i += 1; continue; }
            if in_string || in_char { i += 1; continue; }
            // Skip line comments.
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' { break; }
            // Track block comment state.
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                in_bc.set(true);
                i += 2; continue;
            }
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_bc.set(false);
                i += 2; continue;
            }
            if in_bc.get() { i += 1; continue; }
            if b == target { count += 1; }
            i += 1;
        }
        count
    })
}

/// L1: Combined count of both `{` and `}` in a single pass. Returns (open, close).
/// Used by lazy_occurrence to eliminate the double-scan of count_unmatched.
pub fn count_braces(line: &str) -> (usize, usize) {
    IN_BLOCK_COMMENT.with(|in_bc| {
        let bytes = line.as_bytes();
        let mut open = 0;
        let mut close = 0;
        let mut in_string = false;
        let mut escape = false;
        let mut in_char = false;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if escape { escape = false; i += 1; continue; }
            if b == b'\\' && (in_string || in_char) { escape = true; i += 1; continue; }
            if b == b'"' && !in_char { in_string = !in_string; i += 1; continue; }
            if b == b'\'' && !in_string { in_char = !in_char; i += 1; continue; }
            if in_string || in_char { i += 1; continue; }
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' { break; }
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                in_bc.set(true);
                i += 2; continue;
            }
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_bc.set(false);
                i += 2; continue;
            }
            if in_bc.get() { i += 1; continue; }
            if b == b'{' { open += 1; }
            else if b == b'}' { close += 1; }
            i += 1;
        }
        (open, close)
    })
}

/// One block row.
pub(crate) struct BlockRow {
    pub(crate) start_line: i32,
    pub(crate) end_line: i32,
    pub(crate) indent: i32,
}
/// Output of detect_blocks: the list of blocks and the per-line block index.
pub(crate) struct DetectBlocksOut {
    pub(crate) blocks: Vec<BlockRow>,
    pub(crate) line_block: Vec<usize>,
}

/// Per-token location: (line, zone, col, is_def, block_local_id, tag).
type PhraseLoc = (usize, u8, usize, bool, usize, u8);
/// phrase -> all its locations in one file.
type PhraseLocations = FxHashMap<String, Vec<PhraseLoc>>;

#[derive(Default)]
struct FileResult {
    file: String,
    content: String,
    content_len: usize,
    /// phrase_id -> Vec<OccRow> for the new occurrence table (filled later when phrase_ids are known).
    phrase_locations: PhraseLocations,
    #[allow(dead_code)]
    lines_is_def: Vec<bool>, // pre-calculated is_def for each line
    /// Block rows computed per-file (in file-local block_id order; mapped to global ids at insert time).
    #[allow(dead_code)]
    blocks: Vec<BlockRow>,
    #[allow(dead_code)]
    /// block_local_id for each line (indexed by line number).
    line_block: Vec<usize>,
}

/// V73: shared per-file phrase extraction, used by both trust-time ingest and
/// reindex. Must stay identical to the trust pipeline (stem_identifier, keyword
/// skip, is_def detection, block mapping) so a reindexed file produces the same
/// phrase_occ entries as a full trust — the old reindex used `porter_stem`
/// (stripping `-er`/`-al` from identifiers) and a flags=0 stub, silently
/// degrading search and losing the definition boost until a full re-trust.
///
/// Returns (phrase -> locations, line_count). line_count is the file_stats
/// token_len proxy used by BM25.
pub fn extract_file_phrases(
    content: &str,
) -> (PhraseLocations, i64) {
    let lines: Vec<&str> = content.lines().collect();
    let mut phrase_locations: PhraseLocations =
        FxHashMap::default();

    let mut line_tags: Vec<u8> = Vec::with_capacity(lines.len());
    let mut lines_is_def: Vec<bool> = Vec::with_capacity(lines.len());
    let mut defined_names: Vec<Option<String>> = Vec::with_capacity(lines.len());
    let mut brace_depth: i32 = 0;
    for line in lines.iter() {
        let (open_count, close_count) = count_braces(line);
        let prev_depth = brace_depth;
        brace_depth += open_count as i32 - close_count as i32;
        if brace_depth < 0 { brace_depth = 0; }
        let result = crate::structural::classify_structural(line, prev_depth, open_count > 0, false);
        line_tags.push(result.tag);
        // V74: carry the classifier's is_def BOOL verbatim (as the original
        // inline ingest did). Inferring it from the tag is equivalent today
        // (tags 1-2 are the only is_def returns) but silently diverges if a
        // future tag gains is_def — the pipeline must not guess.
        lines_is_def.push(result.is_def);
        defined_names.push(result.defined_name.map(|s| s.to_string()));
    }

    let DetectBlocksOut { blocks: _, line_block } = detect_blocks(&lines);

    for (li, line) in lines.iter().enumerate() {
        let zone = crate::schema::classify_line(line);
        let block_local = line_block[li];
        let line_tag = line_tags[li];
        let line_is_def = lines_is_def.get(li).copied().unwrap_or(false);
        let line_def_name: Option<&str> = defined_names.get(li).and_then(|n| n.as_deref());
        let code = crate::structural::strip_line_comment(line);
        for (col, token) in scan_identifiers(code).into_iter().enumerate() {
            let stemmed = stem_identifier(&token);
            if crate::keywords::is_keyword(&stemmed) {
                continue;
            }
            let id_tag = if (col == 0 && line_tag >= 5)
                || (line_tag >= 1 && line_def_name == Some(token.as_str()))
            {
                line_tag
            } else {
                0
            };
            phrase_locations.entry(stemmed).or_default().push((
                li,
                zone,
                col,
                line_is_def,
                block_local,
                id_tag,
            ));
        }
    }

    (phrase_locations, lines.len() as i64)
}

/// Detect indentation-anchored block boundaries in a sequence of lines.
///
/// A block is a contiguous run of non-blank lines whose indent is >= the block's
/// leading indent. Block ends at: blank line, EOF, or a non-empty line whose indent
/// is less than the block's leading indent. This is the same grammar-free pattern
/// used by stria's edit.rs (~154 lines), reduced to a single pass.
///
/// Returns (blocks, line_block) where `line_block[i]` is the index into `blocks`
/// for line `i` (or `usize::MAX` for blank lines, which belong to no block).
pub(crate) fn detect_blocks(lines: &[&str]) -> DetectBlocksOut {
    let mut blocks: Vec<BlockRow> = Vec::new();
    let mut line_block: Vec<usize> = vec![usize::MAX; lines.len()];

    let mut i = 0;
    while i < lines.len() {
        // Skip blank lines - they belong to no block.
        while i < lines.len() && lines[i].trim().is_empty() {
            i += 1;
        }
        if i >= lines.len() { break; }
        // Start of a block: leading indent of first non-blank line.
        let start = i as i32;
        let indent = (lines[i].len() - lines[i].trim_start().len()) as i32;
        // Extend until blank line or dedent.
        while i < lines.len() {
            let raw = lines[i];
            if raw.trim().is_empty() { break; }
            let cur_indent = (raw.len() - raw.trim_start().len()) as i32;
            if cur_indent < indent { break; }
            line_block[i] = blocks.len();
            i += 1;
        }
        let end_line = if i > 0 { (i - 1) as i32 } else { start };
        blocks.push(BlockRow { start_line: start, end_line, indent });
    }

    DetectBlocksOut { blocks, line_block }
}

/// Classify the dominant role of identifiers on a line.
///
/// Arc 21: classify_line_tag is DEPRECATED — use structural::classify_structural instead.
/// Kept for backward compat (some external code may still call it). All internal callers
/// have been migrated to the grammar-free structural detector in `crates/reliary-search/src/structural.rs`.
#[deprecated(note = "Use structural::classify_structural instead — grammar-free")]
pub fn classify_line_tag(t: &str, in_impl: bool) -> u8 {
    let _ = (t, in_impl);
    0 // Deprecated — always returns occurrence tag.
}
/// Find the column of the "primary" identifier on a def line — the one being defined.
/// For `fn foo(...)`, it's `foo` (the first identifier after `fn`/`def`).
/// For `struct Foo`, it's `Foo`.
/// For `let foo = ...`, the caller already returns 6 for the whole line.
pub fn primary_identifier_col(line: &str, line_tag: u8) -> usize {
    // Tokenize identifiers by position; return the column of the first identifier on the line.
    let mut col = 0usize;
    let mut chars = line.char_indices().peekable();
    let mut first = None;
    while let Some((i, c)) = chars.next() {
        if c.is_alphabetic() || c == '_' {
            if first.is_none() { first = Some(i); }
            // Consume the rest of the identifier.
            while let Some(&(_, nc)) = chars.peek() {
                if nc.is_alphanumeric() || nc == '_' { chars.next(); } else { break; }
            }
            col += 1;
            if col == 1 {
                return i;
            }
        }
    }
    let _ = line_tag;
    0
}

/// Index all supported files in a directory. Returns file count.
pub fn index_directory(db: &Connection, dir: &str) -> Result<usize, String> {
    let mut paths = Vec::new();
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            // Skip hidden entries (.git, .reliary, .canary, etc.)
            if name.starts_with('.') && name != "." { return false; }
            // Skip common build/dependency output dirs at any depth.
            // Without this, indexing reliary8 itself walks 28K files including
            // cargo build artifacts (.o, .rlib, .bin, .rmeta) and npm packages.
            if crate::ingest::SKIP_DIRS.contains(&name) {
                return false;
            }
            true
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() { continue; }

        // Arc 39: content-based binary detection — grammar-free, no extension list.
        if is_likely_binary(path, 8192) { continue; }
        // Store absolute path so lookups work regardless of cwd at query time.
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        paths.push(abs);
    }

    // Safety cap: refuse to auto-index extremely large repos. The caller
    // (typically auto_trust_if_needed) should handle the error gracefully.
    const MAX_AUTO_INDEX_FILES: usize = 50_000;
    if paths.len() > MAX_AUTO_INDEX_FILES {
        return Err(format!(
            "project too large for auto-indexing ({} files > {} limit). Run `reliary trust .` manually.",
            paths.len(),
            MAX_AUTO_INDEX_FILES
        ));
    }

    // Arc 33 Layer 12: pre-sort paths by extension. Same-extension files share
    // parser cost, hot paths cluster better in rayon work-stealing.
    paths.sort_by(|a, b| {
        let ea = a.extension().and_then(|x| x.to_str()).unwrap_or("");
        let eb = b.extension().and_then(|x| x.to_str()).unwrap_or("");
        ea.cmp(eb)
    });

    let results: Vec<FileResult> = paths.par_iter().filter_map(|path| {
        let file = path.to_string_lossy().to_string();
        // Guard: skip files larger than 10MB to prevent OOM during parallel ingest
        if let Ok(meta) = path.metadata() {
            if meta.len() > 10_000_000 {
                return None;
            }
        }
        let content = std::fs::read_to_string(path).ok()?;

        let lines: Vec<&str> = content.lines().collect();
        let mut phrase_locations: PhraseLocations = FxHashMap::default();
        let mut lines_is_def = Vec::with_capacity(lines.len());

        // Arc 33 Phase A: merge brace_depth + classify_structural + defined_name
        // into a single pass. The original code did this loop TWICE — once for
        // brace_depth/tag/is_def, and a separate O(n²) loop for defined_names
        // that re-scanned all prior lines for every line. On Linux kernel files
        // (avg 200+ lines, headers up to 14K), this quadratic cost dominated.
        // Knuth's "always check for accidental n²" — verified here.
        let mut line_tags: Vec<u8> = Vec::with_capacity(lines.len());
        let mut defined_names: Vec<Option<String>> = Vec::with_capacity(lines.len());
        let mut brace_depth: i32 = 0;
        for (li, line) in lines.iter().enumerate() {
            let _ = li;
            // V52: Use unified count_braces (single byte-scan) instead of two count_unmatched calls.
            // Doubles ingest throughput for brace-heavy files.
            let (open_count, close_count) = count_braces(line);
            let prev_depth = brace_depth;
            brace_depth += open_count as i32 - close_count as i32;
            if brace_depth < 0 { brace_depth = 0; }

            let result = crate::structural::classify_structural(line, prev_depth, open_count > 0, false);
            line_tags.push(result.tag);
            lines_is_def.push(result.is_def);
            defined_names.push(result.defined_name.map(|s| s.to_string()));
        }

        let DetectBlocksOut { blocks, line_block } = detect_blocks(&lines);

        // Arc 33 Phase B Trick #4: detect "table files" — kernel NLS tables,
        // big const arrays. If first 100 lines are >80% numeric AND the file
        // is large (>5000 lines), skip per-line extraction. These files
        // contribute millions of useless occurrence rows that bloat DB.
        let is_table_file = lines.len() > 5000 && {
            let sample: Vec<&str> = lines.iter().take(100).copied().collect();
            let total = sample.len();
            let numeric = sample.iter().filter(|l| {
                let trimmed = l.trim();
                trimmed.is_empty()
                    || trimmed.starts_with("/*")
                    || trimmed.starts_with("//")
                    || trimmed.starts_with('*')
                    || trimmed.chars().filter(|c| {
                        !c.is_whitespace()
                            && !c.is_ascii_hexdigit()
                            && !matches!(c, ',' | ';' | '(' | ')' | '{' | '}' | '[' | ']' | '_')
                    }).count() < 5
            }).count();
            total > 0 && numeric * 10 >= total * 8
        };

        if is_table_file {
            // Skip extraction — file is mostly a const table.
            // We still emit a single placeholder phrase_occ so the file is registered.
            let mut minimal: PhraseLocations = FxHashMap::default();
            minimal.insert("__table_file__".to_string(), vec![(0, 0, 0, false, 0, 0)]);
            return Some(FileResult {
                file,
                content: String::new(),
                content_len: content.len(),
                phrase_locations: minimal,
                lines_is_def,
                blocks,
                line_block,
            });
        }

        for (li, line) in lines.iter().enumerate() {
            let zone = classify_line(line);
            let block_local = line_block[li];
            let line_tag = line_tags[li];
            let line_is_def = lines_is_def.get(li).copied().unwrap_or(false);
            // P9-4: use &str reference instead of cloning Option<String>.
            let line_def_name: Option<&str> = defined_names.get(li).and_then(|n| n.as_deref());
            // Path B: strip trailing // comments before scanning identifiers.
            // Without this, doc-comment prose like "consume" gets indexed as
            // a real identifier and pollutes find_references with false call sites.
            let code = crate::structural::strip_line_comment(line);
            for (col, token) in scan_identifiers(code).into_iter().enumerate() {
                let stemmed = crate::stem_identifier(&token);

                // Arc 33 Phase B Trick #1: skip noise keywords. ~30% of
                // occurrence rows are `int`, `void`, `let`, `mut`, `pub` — they
                // don't carry discriminative meaning and bloat the DB.
                if crate::keywords::is_keyword(&stemmed) {
                    continue;
                }

                // Per-identifier tag: use line_tag, but distinguish the defined name
                // (returned by structural detector) from other identifiers.
                let id_tag = if (col == 0 && line_tag >= 5)
                    || (line_tag >= 1 && line_def_name == Some(token.as_str()))
                {
                    line_tag
                } else {
                    0
                };
                phrase_locations.entry(stemmed).or_default().push((
                    li,
                    zone,
                    col,
                    line_is_def,
                    block_local,
                    id_tag,
                ));
            }
        }

        if phrase_locations.is_empty() && blocks.is_empty() { return None; }

        Some(FileResult {
            file,
            content: content.to_string(),
            content_len: content.len(),
            phrase_locations,
            lines_is_def,
            blocks,
            line_block,
        })
    }).collect();

    let mut count = 0;

    // Arc 33 Layer 5+7: cache phrase_id lookups across all 376 files. Without
    // this, the same phrase does INSERT+SELECT = 2 round trips per file × 376
    // files = ~2.7M round trips for ~7K unique phrases.
    let mut phrase_id_cache: FxHashMap<String, i64> = FxHashMap::default();

    // Arc 33 Layer 14: collect file_stats to insert in one batch after COMMIT.
    let mut pending_file_stats: Vec<(i64, i64, i64)> = Vec::new();

    // Arc 37 schema v4 (deferred flush): accumulate (phrase_id → file_blob)
    // across ALL files during the file loop, then issue ONE INSERT per phrase
    // after the loop completes. This converts ~10M UPSERTs (most of which are
    // UPDATEs that re-write the B-tree leaf) into ~4M pure INSERTs.
    // Memory cost: 4M phrases × ~50 bytes blob = ~200 MB on Linux kernel.
    let mut phrase_blobs: FxHashMap<i64, Vec<u8>> = FxHashMap::default();

    // P1-5: accumulate per-file phrase_ids for file_phrases table.
    let mut file_phrase_ids: FxHashMap<i64, Vec<i64>> = FxHashMap::default();

    // Arc 33 Layer 2: wrap entire insert-loop in a single transaction. Without
    // this, every statement autocommits = 700K fsyncs on tokio.
    db.execute_batch("BEGIN IMMEDIATE").map_err(|e| format!("begin tx: {}", e))?;

    // C7: Drop guard ensures ROLLBACK runs on any early `?` return between
    // BEGIN IMMEDIATE and the explicit COMMIT below. Without this, an error
    // leaves the transaction open until the connection drops, and on a crash
    // the 5GB in-memory journal (synchronous=OFF) could corrupt the DB.
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
    let tx_guard = TxGuard { conn: db, committed: std::cell::Cell::new(false) };

    // Arc 33 Layer 4: prepare statements once and reuse (rusqlite caches them
    // via prepare_cached). With 309K occurrences × 7 statements each, this saves
    // ~2M SQL re-parses.
    let mut ins_phrase = db.prepare_cached("INSERT OR IGNORE INTO phrases (phrase) VALUES (?1)")
        .map_err(|e| format!("prepare phrase: {}", e))?;
    let mut sel_phrase_id = db.prepare_cached("SELECT id FROM phrases WHERE phrase = ?1")
        .map_err(|e| format!("prepare sel phrase: {}", e))?;

    // Arc 33 Layer 6: detect fresh build by counting rows in occurrence BEFORE
    // the first delete. If 0 rows total, all DELETEs are no-ops and we skip them.
    let fresh_build: bool = db.query_row("SELECT COUNT(*) FROM occurrence", [], |r| r.get::<_, i64>(0))
        .map(|n| n == 0)
        .unwrap_or(false);
    if fresh_build {
        eprintln!("[profile] fresh build — skipping DELETE statements per file (Layer 6)");
    }

    // Arc 33 Phase B Trick #3 (Layer 15 redo): drop+recreate indexes around
    // ingest. At Linux kernel scale (110M+ rows), maintaining 10 indexes
    // costs ~30% per insert. Drop them BEFORE ingest, rebuild AFTER main
    // COMMIT. On the kernel, this is mandatory; on small corpora it's
    // a wash but never hurts.
    let t_indexes_start = std::time::Instant::now();
    db.execute_batch(
        "DROP INDEX IF EXISTS idx_block_file; DROP INDEX IF EXISTS idx_block_range;"
    ).map_err(|e| format!("drop indexes: {}", e))?;
    eprintln!("[profile] dropped indexes in {:?}", t_indexes_start.elapsed());

    // Arc 34 Step 2: per-table timing accumulators
    let mut t_filemap = std::time::Duration::ZERO;
    let mut t_occ_collect = std::time::Duration::ZERO;
    let mut t_phrase_occ = std::time::Duration::ZERO;

    for res in results {
        let _t = std::time::Instant::now();
        db.execute("INSERT OR IGNORE INTO file_map (file_path) VALUES (?1)", params![res.file])
            .map_err(|e| format!("insert file: {}", e))?;
        let file_id: i64 = db.query_row("SELECT id FROM file_map WHERE file_path = ?1", params![res.file], |r| r.get(0))
            .map_err(|e| format!("get file id: {}", e))?;
        // P1-1: populate is_source column from first 4KB of content.
        let is_source_val: bool = crate::lazy_occurrence::is_source_like(&res.content);
        db.execute("UPDATE file_map SET is_source = ?1 WHERE id = ?2", params![is_source_val as i32, file_id])
            .map_err(|e| format!("update is_source: {}", e))?;
        t_filemap += _t.elapsed();

        // ─── Arc 31 Phase A: DELETE existing rows for this file before re-inserting.
        // Without this, every re-index doubles the table contents (occurrence,
        // block, scope_binding, method_occurrence all accumulated duplicates on
        // tokio DB — 99.7% bloat, 215 MiB → 7 MiB after fix).
        // Arc 33 Layer 6: on fresh builds, these are guaranteed no-ops — skip them.
        if !fresh_build {
            db.execute("DELETE FROM occurrence WHERE file_id = ?1", params![file_id])
                .map_err(|e| format!("delete occurrence: {}", e))?;
            db.execute("DELETE FROM block WHERE file_id = ?1", params![file_id])
                .map_err(|e| format!("delete block: {}", e))?;
        }

        // ─── v2: lazy blocks (Arc 35) ───
        // Trust NO LONGER inserts into `block`. The blocks table stays
        // empty until first query that needs blocks for this file (see
        // lazy_tables.rs::ensure_blocks_for_file). Drivers/ trust time
        // drops from 600s+ → ~10s.

        // ─── v2: insert one occurrence row per (phrase, occurrence) ───
        // Arc 33 Phase B Trick #2: COLLECT all occurrences for the file,
        // then issue one INSERT ... VALUES (?,..),(?,..),... with N rows.
        //
        // Arc 34 Step 3 LAZY OCCURRENCE: occurrence insertion is 41-50% of
        // trust time on Linux kernel. Skip at trust time. Build on-demand
        // at first find_references query per phrase (see symbol.rs).
        #[allow(dead_code)]
        const OCC_BATCH: usize = 200;

        let mut phrase_data: Vec<(i64, u32, u32, u32, bool)> = Vec::new();

        let _tc = std::time::Instant::now();
        for (phrase, locs) in &res.phrase_locations {
            let phrase_id = if let Some(&cached) = phrase_id_cache.get(phrase) {
                cached
            } else {
                ins_phrase.execute(params![phrase]).map_err(|e| format!("insert phrase: {}", e))?;
                let id: i64 = match sel_phrase_id.query_row(params![phrase], |r| r.get(0)) {
                    Ok(id) => id,
                    Err(e) => {
                        eprintln!("[ingest] phrase_id lookup failed for {:?}: {} — skipping", phrase, e);
                        continue;
                    }
                };
                phrase_id_cache.insert(phrase.clone(), id);
                id
            };
            if phrase_id == 0 { continue; }

            let num_locs = locs.len() as u32;
            let avg_zone = locs.iter().map(|(_, z, _, _, _, _)| *z as u32).sum::<u32>() / num_locs.max(1);
            let is_def_any = locs.iter().any(|(_, _, _, d, _, _)| *d);
            let first_line = locs.first().map(|(l, _, _, _, _, _)| *l as u32).unwrap_or(0);

            // Arc 34: NO occurrence insert at trust time. JIT on first query.
            phrase_data.push((phrase_id, num_locs, avg_zone, first_line, is_def_any));
        }
        t_occ_collect += _tc.elapsed();

        // Append phrase → (file_id, flags) into the deferred-flush accumulator.
        // File loop accumulates; the actual INSERT happens after the loop.
        let _tp = std::time::Instant::now();
        for (phrase_id, num_locs, avg_zone, _first_line, is_def_any) in &phrase_data {
            let flags = pack_flags(if *is_def_any { 1 } else { 0 }, *avg_zone as i32, *num_locs);
            // Pack the (file_id, flags) entry: varint(file_id) + flags[1]
            let mut entry: Vec<u8> = Vec::with_capacity(3);
            crate::schema::encode_varint(file_id, &mut entry);
            entry.push(flags[0]);
            // Append entry into the per-phrase blob. Reuse the entry Vec's
            // bytes by appending (avoid double-alloc).
            let blob = phrase_blobs.entry(*phrase_id).or_default();
            blob.extend_from_slice(&entry);
        }
        // P1-5: accumulate file_phrases entries for this file.
        file_phrase_ids.entry(file_id).or_default()
            .extend(phrase_data.iter().map(|(pid, ..)| *pid));
        t_phrase_occ += _tp.elapsed();

        // ─── Arc 31 Phase A: scope/method extraction. Arc 35: now fully lazy. ───
        // C1: extract_bindings_for_file and extract_methods_for_file removed
        // (scope_binding and method_occurrence tables were always empty).
        // C1: scope/method timing accumulators removed with the tables.

        // Arc 33 Layer 14: defer file_stats INSERT to end-of-index batch.
        let token_len = res.phrase_locations.len() as i64;
        let content_len = res.content_len as i64;
        pending_file_stats.push((file_id, token_len, content_len));

        // Arc 59 Phase 1: warm file_meta cache during trust.
        // P9-2: use compute_from_content to avoid a second disk read —
        // res.content is already in memory from the tokenization loop above.
        // Saves ~5ms per file + 1 disk read per file.
        let _ = crate::file_meta::compute_from_content(&res.file, &res.content);

        count += 1;
    }

    // Arc 37 schema v4 deferred flush: insert all phrase_occ rows now that
    // the file loop is done. One INSERT per phrase with the full packed blob.
    // This is 4M pure INSERTs (no UPDATE-overhead) for Linux kernel.
    let phrase_count = phrase_blobs.len();
    let _tp2 = std::time::Instant::now();
    let mut ins_phrase_occ_final = db.prepare_cached(
        "INSERT INTO phrase_occ (phrase_id, file_blob) VALUES (?1, ?2)"
    ).map_err(|e| format!("prepare ins_phrase_occ_final: {}", e))?;
    for (pid, blob) in &phrase_blobs {
        if let Err(e) = ins_phrase_occ_final.execute(params![*pid, &blob[..]]) {
            eprintln!("[ingest] phrase_occ final INSERT: {}", e);
        }
    }
    drop(ins_phrase_occ_final);
    eprintln!("[profile] phrase_occ flush: {} phrases in {:?}",
        phrase_count, _tp2.elapsed());

    // P1-5: flush file_phrases before COMMIT (inside same transaction).
    // Batch INSERT for efficiency: one multi-row INSERT per file.
    if !file_phrase_ids.is_empty() {
        // Build a single INSERT per file with explicit VALUES.
        for (fid, phrase_ids) in &file_phrase_ids {
            if phrase_ids.is_empty() { continue; }
            let values: Vec<String> = std::iter::repeat_n("(?, ?)", phrase_ids.len()).map(|s| s.to_string()).collect();
            let sql = format!(
                "INSERT OR IGNORE INTO file_phrases (file_id, phrase_id) VALUES {}",
                values.join(", ")
            );
            let mut stmt = match db.prepare(&sql) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut param_idx = 1usize;
            for pid in phrase_ids {
                let _ = stmt.raw_bind_parameter(param_idx, *fid);
                let _ = stmt.raw_bind_parameter(param_idx + 1, *pid);
                param_idx += 2;
            }
            // Execute — errors here mean phrases are missing from the index.
            stmt.raw_execute().map_err(|e| format!("phrase_occ insert: {}", e))?;
            drop(stmt);
        }
    }

    let commit_result = db.execute_batch("COMMIT").map_err(|e| format!("commit tx: {}", e));
    tx_guard.committed.set(true); // mark before next ops so Drop skips ROLLBACK
    commit_result?;

    // Arc 33 Phase B Trick #3 (cont): recreate indexes after main COMMIT.
    // Arc 34 §75 Step D: skip the two occurrence composite indexes. Occurrence
    // table is empty at trust time (lazy build). Recreating them costs ~2s
    // on drivers and they're empty. The JIT rebuilds them lazily when the
    // first occurrence row is inserted.
    let _t_recreate = std::time::Instant::now();
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_block_file ON block(file_id); CREATE INDEX IF NOT EXISTS idx_block_range ON block(file_id, start_line, end_line);"
    ).map_err(|e| format!("recreate indexes: {}", e))?;

    // Layer 14 (cont): one batch INSERT for all file_stats outside main tx.
    if !pending_file_stats.is_empty() {
        // GUARDED: intentional best-effort BEGIN — if this fails (nested tx),
        // the per-row upserts still run in autocommit mode, just slower.
        if let Err(e) = db.execute_batch("BEGIN IMMEDIATE") {
            eprintln!("[ingest] file_stats BEGIN failed: {}", e);
        }
        for (file_id, token_len, content_len) in &pending_file_stats {
            db.execute(
                "INSERT INTO file_stats (file_id, token_len, content_len) VALUES (?1, ?2, ?3)
                 ON CONFLICT(file_id) DO UPDATE SET
                   token_len = excluded.token_len,
                   content_len = excluded.content_len",
                params![*file_id, *token_len, *content_len],
            ).map_err(|e| format!("file_stats upsert: {}", e))?;
        }
        // GUARDED: intentional best-effort commit — the upserts above already
        // succeeded; a COMMIT failure here is logged by SQLite and the next
        // write transaction recovers. Swallowing keeps trust non-fatal.
        if let Err(e) = db.execute_batch("COMMIT") {
            eprintln!("[ingest] file_stats COMMIT failed: {}", e);
        }
    }

    { crate::lazy_occurrence::bump_gen_if_inserted(count); Ok(count) }
}