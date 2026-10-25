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
    // V80: build-output / generated-code directories. A .NET repo's `obj/`
    // holds thousands of generated `.cs` files (assembly attributes, designer
    // stubs) — indexing them flooded the pack and made `diff`'s call-edge pass
    // effectively hang. Same for the other ecosystems' caches. Deliberately
    // excludes `bin/` (Rust's `src/bin/` is real source) and `packages/`
    // (monorepo workspace layouts), which are ambiguous.
    "obj", ".vs", "TestResults",
    ".pytest_cache", ".mypy_cache", ".ruff_cache", ".tox",
    "coverage", ".nyc_output", ".parcel-cache", ".turbo", ".svelte-kit",
    "Pods", "DerivedData", ".dart_tool", "bazel-out", ".terraform",
];

use rusqlite::{params, Connection};
use walkdir::WalkDir;
use rustc_hash::FxHashMap;
use rayon::prelude::*;

use crate::schema::{classify_line, pack_flags};
use crate::{scan_identifiers, stem_identifier, is_likely_binary};

/// Reset the per-thread lexical state (block comment + multi-line literal).
/// MUST be called at the start of every per-file scan: the state is carried
/// across the lines of one file (so a `/* …` or `"""…` spanning lines is
/// tracked), but if a file ends mid-comment/literal the state would
/// otherwise leak into the NEXT file on the same thread. Under
/// `index_directory`'s Rayon `par_iter` that made indexing
/// scheduling-dependent — the same tree produced different indexes.
pub fn reset_comment_state() {
    LEX_STATE.with(|s| s.borrow_mut().reset());
}

/// L1: Combined count of both `{` and `}` in a single pass. Returns (open, close).
/// Used by lazy_occurrence to eliminate the double-scan of count_unmatched.
///
/// V81: delegates to the shared grammar-free lexer (`crate::lex`), which
/// tracks block comments AND multi-line string literals (Python `"""`,
/// Go/JS backticks, C# verbatim, Rust raw). `hash_comment=false`: `#` is
/// code here so `#define FOO {` bodies keep their braces; the `#`-comment
/// case for Python/JS is applied by the tokenizer mask, not the depth count.
pub fn count_braces(line: &str) -> (usize, usize) {
    LEX_STATE.with(|s| crate::lex::brace_counts(line, &mut s.borrow_mut(), false))
}

// Per-thread cross-line lexical state for `count_braces`. Block comments
// and multi-line literals must carry across the lines of one file, and
// must be reset per file (a prior file may end mid-literal under Rayon).
thread_local! {
    static LEX_STATE: std::cell::RefCell<crate::lex::LexState> =
        const { std::cell::RefCell::new(crate::lex::LexState::new()) };
}

/// One block row.
pub struct BlockRow {
    pub start_line: i32,
    pub end_line: i32,
    pub indent: i32,
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
    let (locs, n, _, _) = extract_file_phrases_ctx(content, &IslandCtx::disabled());
    (locs, n)
}

/// Path-aware variant: prose formats are indexed but never supply definitions.
///
/// A `.md` file is documentation. Its fenced snippets and its prose both match
/// the shape rules (`1. **Classify** every line` reads as a definition of
/// `line`; a fenced `pub fn new(...)` defines `new`), and there is no
/// content test that separates a plan document from source — 625 of 4,750
/// definition rows on this repository came from markdown. Documentation is
/// still *searchable*; it just never answers "where is X defined".
///
/// An extension may exclude a decision but never admit one, so this is a
/// deny-list check on the path, not a language allow-list.
pub fn extract_file_phrases_for_path(
    content: &str,
    path: &str,
) -> (PhraseLocations, i64) {
    let (locs, n, _, _) = extract_file_phrases_ctx_with_path(
        content,
        &IslandCtx::disabled(),
        Some(path),
    );
    (locs, n)
}

/// True for documentation formats: indexed for search, never a definition source.
pub fn is_prose_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    if !lower.contains('.') {
        return false;
    }
    matches!(ext, "md" | "mdx" | "markdown" | "rst" | "adoc" | "txt")
}

/// Island-classifier context for the flag-gated wiring path.
/// Disabled (default): byte-identical legacy behavior.
/// Enabled: islands + DF guard loaded from the index, block-scoped
/// backward sets, DP segment starts per file.
pub struct IslandCtx<'a> {
    pub enabled: bool,
    pub islands: &'a [crate::islands::Island],
    pub guard: Option<&'a crate::islands::DfGuard>,
}

impl<'a> IslandCtx<'a> {
    pub fn disabled() -> Self {
        IslandCtx { enabled: false, islands: &[], guard: None }
    }
}

pub fn extract_file_phrases_ctx(
    content: &str,
    ictx: &IslandCtx,
) -> (PhraseLocations, i64, Vec<BlockRow>, Vec<usize>) {
    extract_file_phrases_ctx_with_path(content, ictx, None)
}

/// `path` gates definition extraction (see [`is_prose_path`]) and carries the
/// cross-line literal state that keeps prose inside strings out of the index.
pub fn extract_file_phrases_ctx_with_path(
    content: &str,
    ictx: &IslandCtx,
    path: Option<&str>,
) -> (PhraseLocations, i64, Vec<BlockRow>, Vec<usize>) {
    let defs_allowed = path.map(|p| !is_prose_path(p)).unwrap_or(true);
    let lines: Vec<&str> = content.lines().collect();
    let mut phrase_locations: PhraseLocations =
        FxHashMap::default();

    let mut line_tags: Vec<u8> = Vec::with_capacity(lines.len());
    let mut lines_is_def: Vec<bool> = Vec::with_capacity(lines.len());
    let mut defined_names: Vec<Option<String>> = Vec::with_capacity(lines.len());
    let mut brace_depth: i32 = 0;
    // Per-file comment state must start clean (a prior file may have ended
    // mid-block-comment on this thread under Rayon).
    reset_comment_state();
    // Flag path state: DP segment starts + block-scoped backward sets.
    // segments: computed once per file (needs brace events from a first
    // depth pass); backward: a stack of per-depth word sets (shadowing:
    // same name in two blocks types twice; a use in an inner block of
    // an outer def waters via chain lookup).
    let timing_on = *TIMING_ENABLED.get_or_init(|| std::env::var("RELIARY_TIMING").is_ok());
    let seg_t = std::time::Instant::now();
    let seg_starts: std::collections::HashSet<usize> =
        if ictx.enabled && std::env::var("RELIARY_NO_SEG").is_err() {
        // A2/C: the DP segmenter was pure waste — measured rule1=84452
        // vs rule2=38 hits with IDENTICAL recall on a blind tokio run
        // (0.855 both, 48s of CPU for 38 fallback hits that recovered
        // nothing). Replaced by a cheap positional proxy: a segment
        // starts at line 1, after a blank line, or after a line ending
        // in `}` (a block/brace boundary). O(n), no DP, no tokenize.
        cheap_seg_starts(&lines)
    } else {
        std::collections::HashSet::new()
    };
    // The classify loop below and `detect_blocks` both call count_braces,
    // which carries thread-local block-comment state; start clean.
    reset_comment_state();
    let mut backward_stack: Vec<std::collections::HashSet<String>> =
        vec![std::collections::HashSet::new()];
    let mut stack_depth: i32 = 0;
    let seg_elapsed = seg_t.elapsed();
    let loop_t = std::time::Instant::now();    // Cross-line lexical state for the prose gate.
    let mut lex_state = crate::lex::LexState::new();
    // Import-group depth (blind-gate find): `{` opened through `::`
    // (`use std::{`) lists re-exports; bare words inside water.
    let mut import_group_depth: Option<i32> = None;
    for line in lines.iter() {
        let (open_count, close_count) = count_braces(line);
        let prev_depth = brace_depth;
        brace_depth += open_count as i32 - close_count as i32;
        if brace_depth < 0 { brace_depth = 0; }
        if ictx.enabled {
            // Maintain the backward stack against the NEW depth: entering
            // deeper pushes a fresh set; leaving pops back (floor: root).
            while stack_depth < brace_depth {
                backward_stack.push(std::collections::HashSet::new());
                stack_depth += 1;
            }
            while stack_depth > brace_depth {
                backward_stack.pop();
                stack_depth -= 1;
                if backward_stack.is_empty() {
                    backward_stack.push(std::collections::HashSet::new());
                    break;
                }
            }
            let masked = crate::structural::tokenizable_code(line);
            // Track import-group depth BEFORE classifying: a `::{` opener
            // marks the new depth; leaving it clears.
            if open_count > 0
                && brace_depth > prev_depth
                && crate::islands::opens_import_group(&masked)
            {
                import_group_depth = Some(brace_depth);
            }
            if let Some(gd) = import_group_depth {
                if brace_depth < gd {
                    import_group_depth = None;
                }
            }
            let in_import_group =
                import_group_depth.map(|gd| brace_depth >= gd).unwrap_or(false);
            // V83 B4: import lines must not vaccinate the backward
            // vocabulary. `use tokio::{sync::RwLock, task};` marked `task`
            // as seen, so every later `async fn task(...)` in the file
            // watered (4/4 sites in the tokio benches). A local definition
            // legally shadows an imported name; the import is not a
            // binding in this scope. Multi-line groups are covered by
            // in_import_group; single-line groups must be skipped too
            // (their braces open and close on one line, so the group
            // state never activates).
            let import_line = in_import_group || crate::islands::opens_import_group(&masked);
            // Chain lookup: current block outward (a use in an inner
            // block of an outer def waters); registration below lands in
            // the current block only (shadowing re-types). A2/A3: the
            // stack is borrowed directly — the old per-line union clone
            // was O(L*V) String copies (the #1 flag-path cost).
            let li = line_tags.len();
            let seg_start = seg_starts.contains(&(li + 1));
            // A2b: analyze the header once; the classifier consumes the
            // view instead of recomputing header_pattern 4-5× per line.
            let view = crate::islands::analyze_header(&masked);
            // Import-group members never introduce (re-export list).
            // A line that begins inside a string/block comment has no code at
            // column 0, so every word on it is prose. Classifying one is how a
            // Python docstring produced a definition of `string`.
            let in_literal = !crate::lex::starts_in_code(line, &mut lex_state, false);
            let island_hit = if in_import_group || !defs_allowed || in_literal {
                None
            } else {
                crate::islands::classify_island_line_raw_view(
                    line,
                    &masked,
                    &view,
                    seg_start,
                    prev_depth,
                    &backward_stack,
                    ictx.islands,
                    ictx.guard.map(|g| g as &dyn crate::islands::DfLookup),
                )
            };
            match island_hit {
                Some((name, tag)) => {
                    line_tags.push(tag);
                    lines_is_def.push(true);
                    defined_names.push(Some(name.clone()));
                    // B4: receiver-reached words never introduce — feed with
                    // the lookbehind (a use through `.`/`::` must not
                    // vaccinate later lines).
                    if let Some(top) = backward_stack.last_mut() {
                        for (s, w) in crate::scan_identifiers_with_offsets(&masked) {
                            if w.bytes().next().unwrap().is_ascii_digit() {
                                continue;
                            }
                            if crate::islands::is_receiver(&masked, s) {
                                continue;
                            }
                            top.insert(w);
                        }
                    }
                    // V83 B5: the DEF name belongs to the ENCLOSING block
                    // (the line's own depth, prev_depth), not to the body
                    // frame this line's `{` just pushed. Inserting into the
                    // body frame made the name vanish when the body popped,
                    // so a call in a later function false-typed as a def
                    // (`alpha();` inside `main` after `fn alpha`).
                    let frame = (prev_depth.max(0) as usize).min(backward_stack.len() - 1);
                    backward_stack[frame].insert(name);
                }
                None => {
                    line_tags.push(0);
                    lines_is_def.push(false);
                    defined_names.push(None);
                    // V83 B4: an import line does not introduce names into
                    // the file's vocabulary (see import_line above).
                    if !import_line {
                        if let Some(top) = backward_stack.last_mut() {
                            for (s, w) in crate::scan_identifiers_with_offsets(&masked) {
                                if w.bytes().next().unwrap().is_ascii_digit() {
                                    continue;
                                }
                                if crate::islands::is_receiver(&masked, s) {
                                    continue;
                                }
                                top.insert(w);
                            }
                        }
                    }
                }
            }
            continue;
        }
        // V87: documentation files never define, and a line that begins inside
        // a multi-line literal is prose. This is the per-file pipeline every
        // caller (trust, reindex, JIT) shares, so the gate lives here too.
        let result = if !defs_allowed || !crate::lex::starts_in_code(line, &mut lex_state, false) {
            crate::structural::StructuralResult::water()
        } else {
            crate::structural::classify_structural(line, prev_depth, open_count > 0, false)
        };
        line_tags.push(result.tag);
        // V74: carry the classifier's is_def BOOL verbatim (as the original
        // inline ingest did). Inferring it from the tag is equivalent today
        // (tags 1-2 are the only is_def returns) but silently diverges if a
        // future tag gains is_def — the pipeline must not guess.
        lines_is_def.push(result.is_def);
        defined_names.push(result.defined_name.map(|s| s.to_string()));
    }

    let DetectBlocksOut { blocks, line_block } = detect_blocks(&lines);

    // V93: tokenization needs CROSS-LINE literal state. The per-line
    // `tokenizable_code` resets its lexer on every call, so every line of a
    // Python docstring or JS template literal was tokenized as code and prose
    // words became call sites (`request` picked up 241 docstring hits in
    // psf/requests). One state, reset here, carried through the loop.
    let mut token_state = crate::lex::LexState::new();
    for (li, line) in lines.iter().enumerate() {
        let zone = crate::schema::classify_line(line);
        let block_local = line_block[li];
        let line_tag = line_tags[li];
        let line_is_def = lines_is_def.get(li).copied().unwrap_or(false);
        let line_def_name: Option<&str> = defined_names.get(li).and_then(|n| n.as_deref());
        // Strip comments AND string-literal contents before tokenizing: a
        // name inside `panic!("call_me()")` is not a call site. The state is
        // carried across lines so a docstring interior is blanked too.
        let code = crate::structural::tokenizable_code_with(line, &mut token_state);
        for (col, token) in scan_identifiers(&code).into_iter().enumerate() {
            let stemmed = stem_identifier(&token);
            let is_def_name = line_def_name
                .map(|d| crate::islands::def_name_matches_token(d, token.as_str()))
                .unwrap_or(false);
            // V83 B3: keyword-shaped DEFINED NAMES (`fn new`, `fn default`,
            // `From::from`) are real symbols; only their uses are noise.
            if !is_def_name && crate::keywords::is_noise_token(&token, &stemmed) {
                continue;
            }
            // Same operator-name link as the trust loop (extracts to a
            // helper if a third site appears; two sites share the doc).
            let id_tag = if (col == 0 && line_tag >= 5)
                || (line_tag >= 1
                    && line_def_name
                        .map(|d| crate::islands::def_name_matches_token(d, token.as_str()))
                        .unwrap_or(false))
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

    if timing_on {
        SEG_NS.fetch_add(seg_elapsed.as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        LOOP_NS.fetch_add(loop_t.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        FILE_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    (phrase_locations, lines.len() as i64, blocks, line_block)
}

/// Cheap segment-start proxy (replaces the DP segmenter on the flag
/// path). A line begins a segment if it is the first line, follows a
/// blank line, or follows a line whose last non-space char is `}` —
/// the block/brace boundaries the DP was approximating, without the
/// O(n·MAX) likelihood DP (measured: DP recovered nothing recall-
/// relevant; rule2 was 38/84490 hits). Positional only.
pub fn cheap_seg_starts(lines: &[&str]) -> std::collections::HashSet<usize> {
    let mut out = std::collections::HashSet::new();
    for (i, _line) in lines.iter().enumerate() {
        let prev_blank = i == 0 || lines[i - 1].trim().is_empty();
        let prev_close = i > 0 && lines[i - 1].trim_end().ends_with('}');
        if prev_blank || prev_close {
            out.insert(i + 1);
        }
    }
    out
}

pub static SEG_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static LOOP_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static FILE_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static TIMING_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Print accumulated extract sub-timings (RELIARY_TIMING=1).
pub fn print_extract_timings() {
    use std::sync::atomic::Ordering::Relaxed;
    let (r1, r2) = crate::islands::rule_hit_counts();
    eprintln!(
        "[timing] extract seg_total={}ms loop_total={}ms files={} rule1={} rule2={}",
        SEG_NS.load(Relaxed) / 1_000_000,
        LOOP_NS.load(Relaxed) / 1_000_000,
        FILE_N.load(Relaxed),
        r1,
        r2
    );
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
    index_directory_excluding(db, dir, &std::collections::BTreeSet::new())
}

/// `index_directory` with repo-relative paths that must not be indexed.
///
/// Used by `reliary warrant`, where the head tree is a working checkout that
/// can hold gitignored files (bench output, build artifacts written outside
/// `SKIP_DIRS`). Those are not part of a change: indexing them wastes time and
/// makes the head index disagree with the head scan, which excludes them.
pub fn index_directory_excluding(
    db: &Connection,
    dir: &str,
    exclude_rel: &std::collections::BTreeSet<String>,
) -> Result<usize, String> {
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
        // V84: known data/config formats are skipped by role. DENY-list only —
        // unknown extensions still reach the content-based source test below,
        // so new languages keep working without a list update.
        if crate::is_known_data_ext(path) { continue; }
        if !exclude_rel.is_empty() {
            if let Ok(rel) = path.strip_prefix(dir) {
                if exclude_rel.contains(&rel.to_string_lossy().replace('\\', "/")) {
                    continue;
                }
            }
        }
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

    // Step 4b wiring (A1 piggyback): trust-time island census runs on the
    // existing parallel read — each worker counts generalized header
    // patterns over the content it already holds; the main thread merges
    // and selects. No serial pre-pass, no second disk read (the old
    // learn_island_shapes serial loop is deleted). Disabled by default:
    // zero behavior change (census skipped, ctx disabled).
    let flag_on = crate::segment::segment_enabled();
    let timing = *TIMING_ENABLED.get_or_init(|| std::env::var("RELIARY_TIMING").is_ok());
    let t_census = std::time::Instant::now();
    // Phase 1 shape: path + content read in parallel (+ census when on).
    struct CensusFile {
        file: String,
        content: String,
        counts: std::collections::BTreeMap<Vec<crate::islands::Cat>, usize>,
    }
    let census: Vec<CensusFile> = paths
        .par_iter()
        .filter_map(|path| {
            let file = path.to_string_lossy().to_string();
            if let Ok(meta) = path.metadata() {
                if meta.len() > 10_000_000 {
                    return None;
                }
            }
            let content = std::fs::read_to_string(path).ok()?;
            let counts = if flag_on {
                count_island_shapes(&content)
            } else {
                std::collections::BTreeMap::new()
            };
            Some(CensusFile { file, content, counts })
        })
        .collect();
    // Merge + select on the main thread (KBs; the only serial step).
    let island_selected: Vec<crate::islands::Island> = if flag_on {
        let mut merged: std::collections::BTreeMap<Vec<crate::islands::Cat>, usize> =
            std::collections::BTreeMap::new();
        for cf in &census {
            for (pat, c) in &cf.counts {
                *merged.entry(pat.clone()).or_insert(0) += c;
            }
        }
        let mut ranked: Vec<(Vec<crate::islands::Cat>, usize)> = merged.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let distinct = ranked.len();
        crate::islands::select_islands(&ranked, distinct)
    } else {
        Vec::new()
    };
    let island_ctx = IslandCtx {
        enabled: !island_selected.is_empty(),
        islands: &island_selected,
        guard: None, // df unavailable pre-insert; IDF post-pass below
    };
    if timing {
        eprintln!(
            "[timing] census+select={:?} islands={}",
            t_census.elapsed(),
            island_selected.len()
        );
    }

    // Phase 2: full per-file pipeline in parallel over resident contents.
    // Peak memory unchanged vs baseline: contents were already all held
    // simultaneously by the post-collect insert loop (batch, not stream).
    let t_phase2 = std::time::Instant::now();
    let results: Vec<FileResult> = census
        .into_par_iter()
        .filter_map(|cf| {
            let CensusFile { file, content, counts: _ } = cf;
        // Step 4b: flag path delegates the whole per-file pipeline to the
        // shared ctx implementation (islands + block-scoped backward +
        // DP segments). Legacy inline loop below stays byte-identical
        // when the flag is off.
        if island_ctx.enabled {
            let (phrase_locations, _lc, blocks, line_block) =
                extract_file_phrases_ctx_with_path(&content, &island_ctx, Some(&file));
            if phrase_locations.is_empty() && blocks.is_empty() {
                return None;
            }
            // lines_is_def is consumed by callers for stats; recompute
            // cheaply is skipped — flag path stores per-phrase flags in
            // phrase_locations (4th tuple elem), which is what persists.
            return Some(FileResult {
                file,
                content: content.to_string(),
                content_len: content.len(),
                phrase_locations,
                lines_is_def: Vec::new(),
                blocks,
                line_block,
            });
        }

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
        let defs_allowed = !is_prose_path(&file);
        // Prose gate: a `.md` file never supplies definitions, and a line that
        // begins inside a multi-line literal is prose whatever the extension.
        let mut lex_state = crate::lex::LexState::new();
        // V93: cross-line state for tokenization (see the other pipeline).
        let mut token_state = crate::lex::LexState::new();
        reset_comment_state();
        for (li, line) in lines.iter().enumerate() {
            let _ = li;
            // V52: Use unified count_braces (single byte-scan) instead of two count_unmatched calls.
            // Doubles ingest throughput for brace-heavy files.
            let (open_count, close_count) = count_braces(line);
            let prev_depth = brace_depth;
            brace_depth += open_count as i32 - close_count as i32;
            if brace_depth < 0 { brace_depth = 0; }

            let result = if !defs_allowed || !crate::lex::starts_in_code(line, &mut lex_state, false) {
                crate::structural::StructuralResult::water()
            } else {
                crate::structural::classify_structural(line, prev_depth, open_count > 0, false)
            };
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
            // V93: cross-line state so docstring interiors are blanked.
            let code = crate::structural::tokenizable_code_with(line, &mut token_state);
            for (col, token) in scan_identifiers(&code).into_iter().enumerate() {
                let stemmed = crate::stem_identifier(&token);

                // Arc 33 Phase B Trick #1: skip noise keywords. ~30% of
                // occurrence rows are `int`, `void`, `let`, `mut`, `pub` — they
                // don't carry discriminative meaning and bloat the DB.
                // PascalCase raw tokens are exempt (is_noise_token).
                // V83 B3: a keyword-shaped DEFINED NAME (`fn new`,
                // `fn default`) is a real symbol; only its uses are noise.
                let is_def_name = line_def_name
                    .map(|d| crate::islands::def_name_matches_token(d, token.as_str()))
                    .unwrap_or(false);
                if !is_def_name && crate::keywords::is_noise_token(&token, &stemmed) {
                    continue;
                }

                // Per-identifier tag: use line_tag, but distinguish the defined name
                // (returned by structural detector) from other identifiers.
                // def_name_matches_token links operator names (`operator==`
                // claims the `operator` token); exact otherwise.
                let id_tag = if (col == 0 && line_tag >= 5)
                    || (line_tag >= 1
                        && line_def_name
                            .map(|d| crate::islands::def_name_matches_token(d, token.as_str()))
                            .unwrap_or(false))
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

    if timing {
        eprintln!("[timing] phase2_extract={:?} files={}", t_phase2.elapsed(), results.len());
    }
    let t_insert = std::time::Instant::now();
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

    { crate::lazy_occurrence::bump_gen_if_inserted(count); }

    // V86: drop rows for files that no longer exist. Nothing used to do this,
    // so a deleted file stayed a search candidate and kept its definition
    // citations alive (23,510 occurrence rows on this repo pointed at files
    // that were gone). Deleting a file is a normal way to finish a change, so
    // this is part of finishing an index, not an optional cleanup.
    //
    // Safety: a path is only pruned when its *parent directory* still exists.
    // An unmounted volume would otherwise look like thousands of deletions and
    // wipe a perfectly good index.
    if let Err(e) = crate::ingest::prune_missing_files(db) {
        eprintln!("[ingest] orphan prune failed: {}", e);
    }

    if timing {
        print_extract_timings();
        eprintln!("[timing] insert={:?} rows={}", t_insert.elapsed(), count);
    }

    // Step 4b wiring: trust-time island learning (flag-gated).
    // The pre-pass selected islands from masked corpus lines; persist
    // them for query/JIT consumers. Re-trust rebuilds.
    if island_ctx.enabled {
        if let Err(e) = persist_islands(db, &island_selected) {
            eprintln!("[ingest] island persist failed: {} — flag path falls back to structural", e);
        }
        // IDF post-pass: clear is_def flags for corpus-noise stems.
        // df comes from the just-flushed file_phrases (GROUP BY...
        // HAVING over half the source files); blobs are decoded,
        // def bits cleared, re-encoded. Only noisy phrases touched.
        if let Err(e) = apply_idf_guard(db) {
            eprintln!("[ingest] idf guard failed: {}", e);
        }
    }

    Ok(count)
}

/// Remove rows for indexed files that no longer exist on disk.
///
/// Skips paths whose parent directory is missing, so an unmounted volume is
/// never mistaken for a mass deletion. Returns the number of files pruned.
pub fn prune_missing_files(db: &Connection) -> Result<usize, String> {
    let rows: Vec<(i64, String)> = {
        let mut stmt = db
            .prepare("SELECT id, file_path FROM file_map ORDER BY id")
            .map_err(|e| format!("prune prepare: {}", e))?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| format!("prune query: {}", e))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    let mut pruned = 0usize;
    for (id, path) in rows {
        let p = std::path::Path::new(&path);
        if p.exists() {
            continue;
        }
        // An unmounted volume looks like a mass deletion. Only prune when the
        // parent directory is still there, so the file itself is what vanished.
        if p.parent().map(|d| !d.exists()).unwrap_or(false) {
            continue;
        }

        // Strip the file from phrase_occ blobs FIRST: `file_phrases` is the
        // index of which blobs mention it, and it is deleted below.
        let phrase_ids: Vec<i64> = {
            let mut stmt = db
                .prepare("SELECT phrase_id FROM file_phrases WHERE file_id = ?1")
                .map_err(|e| format!("prune file_phrases prepare: {}", e))?;
            let mapped = stmt
                .query_map([id], |r| r.get::<_, i64>(0))
                .map_err(|e| format!("prune file_phrases query: {}", e))?;
            let out: Vec<i64> = mapped.filter_map(|r| r.ok()).collect();
            out
        };
        for pid in phrase_ids {
            let blob: Option<Vec<u8>> = match db.query_row(
                "SELECT CAST(file_blob AS BLOB) FROM phrase_occ WHERE phrase_id = ?1",
                [pid],
                |r| r.get(0),
            ) {
                Ok(b) => Some(b),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(format!("prune read blob: {}", e)),
            };
            let Some(blob) = blob else { continue };
            let entries: Vec<(i64, u8)> = crate::schema::unpack_file_blob(&blob)
                .filter(|(fid, _)| *fid != id)
                .collect();
            if entries.is_empty() {
                db.execute("DELETE FROM phrase_occ WHERE phrase_id = ?1", [pid])
                    .map_err(|e| format!("prune delete phrase_occ: {}", e))?;
            } else {
                let packed = crate::schema::pack_file_blob(&entries);
                db.execute(
                    "UPDATE phrase_occ SET file_blob = ?1 WHERE phrase_id = ?2",
                    rusqlite::params![&packed[..], pid],
                )
                .map_err(|e| format!("prune update phrase_occ: {}", e))?;
            }
        }

        for table in ["occurrence", "block", "file_phrases", "file_stats", "count_overflow"] {
            let exists: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if exists > 0 {
                db.execute(&format!("DELETE FROM {} WHERE file_id = ?1", table), [id])
                    .map_err(|e| format!("prune delete {}: {}", table, e))?;
            }
        }
        db.execute("DELETE FROM file_map WHERE id = ?1", [id])
            .map_err(|e| format!("prune delete file_map: {}", e))?;
        pruned += 1;
    }
    Ok(pruned)
}

/// Step 4b: IDF post-pass. Stems occurring in more than half the source
/// files are structural noise, never definitions (majority rule —
/// DfGuard::allows). Clears their is_def bits in phrase_occ blobs.
pub fn apply_idf_guard(db: &Connection) -> Result<usize, String> {
    let total: i64 = db
        .query_row("SELECT COUNT(*) FROM file_map WHERE is_source = 1", [], |r| {
            r.get(0)
        })
        .map_err(|e| format!("idf file count: {}", e))?;
    if total == 0 {
        return Ok(0);
    }
    // Noisy phrase ids: df > total/2 (strict majority).
    let noisy: Vec<i64> = {
        let mut stmt = db
            .prepare(
                "SELECT phrase_id FROM file_phrases GROUP BY phrase_id
                 HAVING COUNT(DISTINCT file_id) * 2 > ?1",
            )
            .map_err(|e| format!("idf noisy prepare: {}", e))?;
        let rows = stmt
            .query_map([total], |r| r.get::<_, i64>(0))
            .map_err(|e| format!("idf noisy rows: {}", e))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let mut cleared = 0usize;
    for pid in &noisy {
        // A missing blob just means no rows to fix (phrase with no blob).
        let blob: Option<Vec<u8>> = db
            .query_row("SELECT file_blob FROM phrase_occ WHERE phrase_id = ?1", [*pid], |r| {
                r.get(0)
            })
            .ok(); // GUARDED: intentional — missing blob means nothing to fix.
        let blob = match blob {
            Some(b) => b,
            None => continue,
        };
        let entries: Vec<(i64, u8)> = crate::schema::unpack_file_blob(&blob).collect();
        if !entries.iter().any(|(_, f)| crate::schema::unpack_is_def(*f) > 0) {
            continue;
        }
        // Re-pack with is_def cleared (bit layout: low 2 bits = is_def+1;
        // clearing means is_def_packed = 0 -> is_def -1... careful:
        // unpack_is_def = (flags & 3) - 1. def rows have is_def>=0 ->
        // packed 1..3. Clearing to "not a def" = packed 0 -> is_def -1?
        // Check unpack semantics: is_def=-1 means "no info"? Look at
        // pack call sites: pack_flags(is_def_any as i32...) with bool as
        // 0/1 -> packed 1/2. Clearing sets packed 0 -> unpack gives -1.
        // Consumers treat >0 as def, so -1/0 both read non-def. Use
        // pack_flags(0,...) form: packed is_def bits = 1 -> unpack 0.
        let mut repacked: Vec<(i64, u8)> = Vec::with_capacity(entries.len());
        for (fid, f) in &entries {
            if crate::schema::unpack_is_def(*f) > 0 {
                let zone = crate::schema::unpack_zone_int(*f);
                let count = crate::schema::unpack_count(*f);
                let nf = crate::schema::pack_flags(0, zone, count)[0];
                repacked.push((*fid, nf));
                cleared += 1;
            } else {
                repacked.push((*fid, *f));
            }
        }
        let new_blob = crate::schema::pack_file_blob(&repacked);
        if let Err(e) = db.execute(
            "UPDATE phrase_occ SET file_blob = ?1 WHERE phrase_id = ?2",
            rusqlite::params![&new_blob[..], *pid],
        ) {
            eprintln!("[ingest] idf blob rewrite: {}", e);
        }
    }
    eprintln!("[ingest] idf guard: {} noisy phrases, {} def entries cleared", noisy.len(), cleared);
    Ok(cleared)
}

/// Step 4b (A1): per-file island-shape census over resident content.
/// Same masking/pattern/generalize as the query path; counts only.
/// Called by parallel workers — no disk I/O here.
fn count_island_shapes(
    content: &str,
) -> std::collections::BTreeMap<Vec<crate::islands::Cat>, usize> {
    use crate::islands::{generalize, header_pattern};
    let mut counts: std::collections::BTreeMap<Vec<crate::islands::Cat>, usize> =
        std::collections::BTreeMap::new();
    for line in content.lines() {
        let masked = crate::structural::tokenizable_code(line);
        let g = generalize(&header_pattern(&masked));
        if g.is_empty() {
            continue;
        }
        *counts.entry(g).or_insert(0) += 1;
    }
    counts
}

/// Step 4b: persist trust-time selected islands (replaces the stub
/// build_islands disk re-read below when called with a set).
pub fn persist_islands(
    db: &Connection,
    selected: &[crate::islands::Island],
) -> Result<usize, String> {
    db.execute("DELETE FROM islands", [])
        .map_err(|e| format!("islands clear: {}", e))?;
    {
        let mut stmt = db
            .prepare("INSERT INTO islands (pattern, count) VALUES (?1, ?2)")
            .map_err(|e| format!("islands insert prepare: {}", e))?;
        for isl in selected {
            let pat = format!("{:?}", isl.pattern);
            if let Err(e) = stmt.execute(rusqlite::params![pat, isl.count as i64]) {
                eprintln!("[ingest] islands insert: {}", e);
            }
        }
    }
    eprintln!("[ingest] islands: {} selected", selected.len());
    Ok(selected.len())
}

/// Load persisted islands (pattern debug-format, count). Empty when the
/// index was trusted without the flag — callers fall back to structural.
pub fn load_islands(db: &Connection) -> Vec<crate::islands::Island> {
    use crate::islands::Cat;
    let mut out = Vec::new();
    let mut stmt = match db.prepare("SELECT pattern, count FROM islands") {
        Ok(s) => s,
        Err(_) => return out, // pre-v6 DB without migration: no islands
    };
    let rows = match stmt.query_map([], |r| {
        let p: String = r.get(0)?;
        let c: i64 = r.get(1)?;
        Ok((p, c))
    }) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for row in rows.flatten() {
        let (pat_s, count) = row;
        // Pattern was written with {:?} over Cat variants; parse back
        // positionally (no values involved — variant names only).
        let inner = pat_s.trim().trim_start_matches('[').trim_end_matches(']');
        let mut pat = Vec::new();
        let mut ok = true;
        for tok in inner.split(',') {
            let t = tok.trim();
            if t.is_empty() {
                continue;
            }
            let c = match t {
                "Word" => Cat::Word,
                "Num" => Cat::Num,
                "OpenParen" => Cat::OpenParen,
                "CloseParen" => Cat::CloseParen,
                "OpenBrace" => Cat::OpenBrace,
                "CloseBrace" => Cat::CloseBrace,
                "OpenBracket" => Cat::OpenBracket,
                "CloseBracket" => Cat::CloseBracket,
                "Colon" => Cat::Colon,
                "Semi" => Cat::Semi,
                "Comma" => Cat::Comma,
                "Other" => Cat::Other,
                _ => {
                    ok = false;
                    break;
                }
            };
            pat.push(c);
        }
        if ok && !pat.is_empty() {
            out.push(crate::islands::Island { pattern: pat, count: count as usize });
        }
    }
    out
}
#[cfg(test)]
mod comment_state_tests {
    use super::*;

    #[test]
    fn block_comment_state_does_not_leak_between_files() {
        // A file ending inside an unterminated `/*` must not corrupt the next
        // file's brace count on the same thread.
        reset_comment_state();
        let (o1, c1) = count_braces("/* unterminated comment with a { brace");
        assert_eq!(o1, 0, "braces inside a /* comment must not count");
        assert_eq!(c1, 0);
        // The next file starts clean — WITHOUT the reset this would inherit the
        // still-open block comment and report 0 braces for the brace below.
        reset_comment_state();
        let (o2, c2) = count_braces("fn foo() {");
        assert_eq!((o2, c2), (1, 0), "reset must clear inherited block-comment state");
    }

    #[test]
    fn negative_control_without_reset_leaks() {
        // Prove the guard is load-bearing: simulate the buggy behaviour by NOT
        // resetting, and confirm the state leaks.
        reset_comment_state();
        let _ = count_braces("/* open");
        let (o, _) = count_braces("fn foo() {");
        assert_eq!(o, 0, "without reset the brace is (wrongly) inside the comment");
        reset_comment_state();
    }
}

#[cfg(test)]
mod v86_orphan_tests {
    use super::*;
    use rusqlite::Connection;

    fn fixture() -> (tempfile::TempDir, Connection) {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("corpus");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "pub fn alpha() -> i32 { 1 }\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "pub fn beta() { alpha(); }\n").unwrap();
        let db = Connection::open(d.path().join("i.sqlite")).unwrap();
        crate::schema::create_new_db(&db).unwrap();
        index_directory(&db, root.to_str().unwrap()).unwrap();
        crate::lazy_occurrence::build_all_occurrence(&db).unwrap();
        (d, db)
    }

    /// A deleted file must leave no rows behind. Before this, `file_map` and
    /// `file_stats` survived, so the file stayed a search candidate and its
    /// definition citations kept resolving to a path that no longer existed.
    #[test]
    fn prune_removes_every_table_for_a_deleted_file() {
        let (d, db) = fixture();
        let root = d.path().join("corpus");
        let target = root.join("src/b.rs");
        let fid: i64 = db
            .query_row("SELECT id FROM file_map WHERE file_path = ?1", [target.to_str().unwrap()], |r| r.get(0))
            .unwrap();
        assert!(
            db.query_row("SELECT COUNT(*) FROM occurrence WHERE file_id = ?1", [fid], |r| r.get::<_, i64>(0)).unwrap() > 0,
            "fixture must have occurrence rows to remove"
        );
        std::fs::remove_file(&target).unwrap();
        let pruned = prune_missing_files(&db).unwrap();
        assert_eq!(pruned, 1, "exactly the deleted file");
        for table in ["occurrence", "block", "file_phrases", "file_stats", "count_overflow"] {
            let exists: i64 = db
                .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1", [table], |r| r.get(0))
                .unwrap();
            if exists > 0 {
                let n: i64 = db
                    .query_row(&format!("SELECT COUNT(*) FROM {} WHERE file_id = ?1", table), [fid], |r| r.get(0))
                    .unwrap();
                assert_eq!(n, 0, "{table} still has rows for the deleted file");
            }
        }
        let n: i64 = db.query_row("SELECT COUNT(*) FROM file_map WHERE id = ?1", [fid], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "file_map row survived");
        // The surviving file is untouched.
        assert_eq!(db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

    /// An unmounted volume looks like a mass deletion. Only prune when the
    /// parent directory still exists, so a missing mount cannot wipe an index.
    #[test]
    fn prune_skips_paths_under_a_missing_parent() {
        let (d, db) = fixture();
        let root = d.path().join("corpus");
        let mounted = root.join("src");
        // Remove the whole directory: the parent is gone too.
        std::fs::remove_dir_all(&mounted).unwrap();
        let pruned = prune_missing_files(&db).unwrap();
        assert_eq!(pruned, 0, "a missing parent directory must not be pruned");
        assert_eq!(db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }
}

#[cfg(test)]
mod v87_prose_tests {
    use super::*;

    fn defs(phrases: &PhraseLocations) -> Vec<String> {
        let mut out: Vec<String> = phrases
            .iter()
            .filter(|(_, locs)| locs.iter().any(|l| l.3))
            .map(|(k, _)| k.clone())
            .collect();
        out.sort();
        out
    }

    /// Documentation is searchable but never a definition source.
    ///
    /// A `.md` file's fenced snippets and its prose both match the shape
    /// rules. 627 of 4,750 definition rows on this repository came from
    /// markdown, so `def_only` answered with plan documents.
    #[test]
    fn prose_files_are_indexed_but_never_define() {
        // No code fences: a fence opens a template literal in the lexer, which
        // would suppress the following lines for a *different* reason and make
        // this test vacuous for the prose gate (caught by its own control).
        let content = "# Notes\n\npub fn from_markdown() {}\n\n1. **Classify** every line.\n";
        let (md, _) = extract_file_phrases_for_path(content, "docs/notes.md");
        assert!(
            md.contains_key("from_markdown"),
            "documentation must stay searchable"
        );
        assert!(
            defs(&md).is_empty(),
            "markdown must supply no definitions, got {:?}",
            defs(&md)
        );

        // Source still defines. (The markdown bytes are not reused here: the
        // fence backticks legitimately open a template literal, so "the same
        // bytes as .rs" would be testing the lexer's string handling, not the
        // prose gate.)
        let src = "pub fn from_source() {}\n";
        let (rs, _) = extract_file_phrases_for_path(src, "src/notes.rs");
        assert!(
            defs(&rs).contains(&"from_source".to_string()),
            "source must define, got {:?}",
            defs(&rs)
        );
    }

    #[test]
    fn prose_path_predicate_covers_doc_formats_only() {
        for p in ["a.md", "a.MD", "a.mdx", "a.rst", "a.adoc", "a.txt", "docs/b.markdown"] {
            assert!(is_prose_path(p), "{p} is documentation");
        }
        for p in ["a.rs", "a.py", "a.js", "a.c", "a", "dir.rs/a", "src/lib.rs"] {
            assert!(!is_prose_path(p), "{p} is source");
        }
    }

    /// A line that begins inside a multi-line string is prose.
    ///
    /// `A symbol mentioned only inside a string ("... write_vocab() ...")`
    /// inside a Python docstring was classified as code, producing a
    /// definition of `string` that `def_only("String")` answered with.
    #[test]
    fn lines_inside_a_docstring_never_define() {
        let content = "def real_fn():\n    \"\"\"Docs.\n\n    A symbol mentioned in a string (\"... call_me() ...\")\n    \"\"\"\n    return 1\n";
        let (locs, _) = extract_file_phrases_for_path(content, "bench/x.py");
        let defined = defs(&locs);
        assert!(
            defined.contains(&"real_fn".to_string()),
            "the real function still defines, got {defined:?}"
        );
        for phantom in ["call", "call_me", "string", "symbol"] {
            assert!(
                !defined.contains(&phantom.to_string()),
                "docstring prose must not define {phantom}, got {defined:?}"
            );
        }
    }
}
