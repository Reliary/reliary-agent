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

/// Step 4b: per-file island context for the flag-gated JIT path.
/// Loads persisted islands + source file count when the flag is on
/// AND the index holds a set; disabled otherwise (legacy byte-identical).
/// Owns the island vec (lifetimes: callers hold the struct, borrow its
/// fields — no self-reference).
pub struct FileIslandCtx {
    pub enabled: bool,
    pub islands: Vec<crate::islands::Island>,
    pub total_files: usize,
}

pub fn load_island_ctx(db: &Connection) -> FileIslandCtx {
    if !crate::segment::segment_enabled() {
        return FileIslandCtx { enabled: false, islands: Vec::new(), total_files: 0 };
    }
    let islands = crate::ingest::load_islands(db);
    if islands.is_empty() {
        return FileIslandCtx { enabled: false, islands: Vec::new(), total_files: 0 };
    }
    let total_files: usize = db
        .query_row("SELECT COUNT(*) FROM file_map WHERE is_source = 1", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n.max(0) as usize)
        .unwrap_or(0);
    FileIslandCtx { enabled: true, islands, total_files }
}

/// Step 4b: doc frequency of one stem = phrase_occ blob entry count
/// (one indexed PK lookup + varint decode). No schema change, no scan.
/// Result cached per file by the caller via `cache`.
pub fn df_for_stem(
    db: &Connection,
    stem: &str,
    cache: &mut FxHashMap<String, usize>,
) -> usize {
    if let Some(&d) = cache.get(stem) {
        return d;
    }
    let d: usize = db
        .query_row("SELECT id FROM phrases WHERE phrase = ?1", [stem], |r| {
            r.get::<_, i64>(0)
        })
        .ok()
        .and_then(|pid| {
            db.query_row(
                "SELECT file_blob FROM phrase_occ WHERE phrase_id = ?1",
                [pid],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .ok()
        })
        .map(|blob| crate::schema::unpack_file_blob(&blob).count())
        .unwrap_or(0);
    cache.insert(stem.to_string(), d);
    d
}

/// Deferred IDF guard (A2b): resolves `df_for_stem` only when the
/// classifier asks about an anchorless candidate name — never a
/// per-token prebuild of the whole line's vocabulary. `allows` matches
/// `DfGuard::allows` exactly (majority rule), so results are identical.
struct LazyDfGuard<'a> {
    db: &'a Connection,
    cache: &'a std::cell::RefCell<FxHashMap<String, usize>>,
    total_files: usize,
}

impl crate::islands::DfLookup for LazyDfGuard<'_> {
    fn allows(&self, stem: &str) -> bool {
        if self.total_files == 0 {
            return true;
        }
        let cached = self.cache.borrow().get(stem).copied();
        let d = match cached {
            Some(d) => d,
            None => {
                let d = df_for_stem_plain(self.db, stem, &mut self.cache.borrow_mut());
                d
            }
        };
        d * 2 <= self.total_files
    }
}

/// Same body as `df_for_stem` but taking a plain `&mut` cache (the
/// `RefCell`-wrapped caller passes the borrow). Kept separate so the
/// public `df_for_stem` signature (used by tests/tools) is unchanged.
fn df_for_stem_plain(
    db: &Connection,
    stem: &str,
    cache: &mut FxHashMap<String, usize>,
) -> usize {
    if let Some(&d) = cache.get(stem) {
        return d;
    }
    let pid: Option<i64> = db
        .query_row("SELECT id FROM phrases WHERE phrase = ?1", [stem], |r| r.get(0))
        .ok(); // GUARDED: intentional — absent stem: "nothing to fix", treat as df 0
    let d = pid
        .and_then(|pid| {
            db.query_row(
                "SELECT file_blob FROM phrase_occ WHERE phrase_id = ?1",
                [pid],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .ok()
        })
        .map(|blob| crate::schema::unpack_file_blob(&blob).count())
        .unwrap_or(0);
    cache.insert(stem.to_string(), d);
    d
}

/// V58b: PER-PHRASE generation counters. The global counter (V58) was too
/// coarse — any JIT build anywhere invalidated every cached result, so
/// cross-query repeats never hit. A repeat is now served from cache iff THAT
/// phrase's occurrence rows are unchanged.
static PHRASE_GENS: std::sync::Mutex<Option<FxHashMap<i64, u64>>> = std::sync::Mutex::new(None);

#[allow(dead_code)]
fn phrase_gens() -> FxHashMap<i64, u64> {
    let mut g = PHRASE_GENS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(FxHashMap::default).clone()
}

/// V61: single-entry lookup without cloning the whole map.
/// The hot path (mcp.rs phrase_generation) was cloning the ENTIRE
/// generation map under lock on every call.
fn phrase_gen(phrase_id: i64) -> Option<u64> {
    let g = PHRASE_GENS.lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref().and_then(|m| m.get(&phrase_id).copied())
}

fn phrase_gens_store<F: FnOnce(&mut FxHashMap<i64, u64>)>(f: F) {
    let mut g = PHRASE_GENS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(FxHashMap::default));
}

/// Current generation for one phrase (0 if never built).
pub fn phrase_generation(phrase_id: i64) -> u64 {
    phrase_gen(phrase_id).unwrap_or(0)
}

#[inline]
pub(crate) fn bump_gen_if_inserted(n: usize) {
    let _ = n; // retained for callers; per-phrase bumps happen in ensure_* paths
}

/// Full invalidation (file-level rebuild touches many phrases).
pub fn invalidate_all_phrase_gens() {
    let mut g = PHRASE_GENS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = g.as_mut() {
        m.clear();
    }
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

    // V84: literate Haskell detection. A `.lhs` file prefixes every code
    // line with `> `; a Markdown blockquote uses the same prefix for prose.
    // The file-level majority decides: if most non-blank lines carry the
    // prefix, it is literate code and the prefix is stripped before
    // classification. A doc with a quote-heavy section never reaches
    // majority without bullet/link markers elsewhere.
    let quote_count = sample
        .iter()
        .filter(|l| l.trim_start().starts_with("> "))
        .count();
    let sample_nonblank = sample.iter().filter(|l| !l.trim().is_empty()).count();
    let literate = quote_count > 0 && quote_count * 2 > sample_nonblank;

    let mut code_lines = 0usize;
    let mut strong_lines = 0usize;
    let mut non_blank = 0usize;
    let mut markdown_lines = 0usize;
    let mut comment_lines = 0usize;
    let mut data_lines = 0usize;

    for line in &sample {
        let raw = line.trim();
        if raw.is_empty() { continue; }
        let trimmed = if literate {
            raw.strip_prefix("> ").unwrap_or(raw)
        } else {
            raw
        };
        non_blank += 1;

        // V84: language attributes are CODE evidence, not comments. Rust
        // `#[derive(...)]` / `#![allow(...)]` open with `#` like a markdown
        // heading, but they are declaration syntax.
        if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
            code_lines += 1;
            strong_lines += 1;
            continue;
        }

        // V84: unambiguously code-comment shapes. These are NEUTRAL — they
        // neither count as code nor as prose in the density denominator.
        // Comment-heavy source (Haskell/OCaml headers, 80% `--` files) was
        // being rejected because comments diluted the code ratio; that is a
        // denominator bug, not a signal about the file's role.
        let is_comment = trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*/")
            || trimmed.starts_with("* ")
            || (trimmed.starts_with("--") && !trimmed.starts_with("-->"))
            || trimmed.starts_with("{-")
            || trimmed.starts_with("(*")
            || trimmed.starts_with("<!--")
            || (trimmed.starts_with('#')
                && !trimmed.starts_with("#include")
                && !trimmed.starts_with("#define")
                && !trimmed.starts_with("#pragma")
                && !trimmed.starts_with("#if")
                && !trimmed.starts_with("#elif")
                && !trimmed.starts_with("#else")
                && !trimmed.starts_with("#endif")
                && !trimmed.starts_with("#undef"));
        if is_comment {
            comment_lines += 1;
            // V84: `#`-led lines are comments in Python/Ruby/Shell and
            // headings in Markdown; `* `-led lines are block-comment
            // continuations in C-family code and bullets in Markdown. They
            // are NEUTRAL for the code ratio (a comment-heavy source file
            // must not fail on density), but they still count toward the
            // doc-marker tally below. The doc test requires marker dominance
            // AND weak code evidence, which separates a 70%-`#` Ruby file
            // from a 70%-bullet README.
            if trimmed.starts_with('#') || trimmed.starts_with("* ") {
                markdown_lines += 1;
            }
            continue;
        }

        // V63f: markdown-marker lines (`- `, `> `, `` ` ``, table rows,
        // and markdown links `[text](url)`).
        // `#` headings are handled above as neutral comments.
        // V84: `> ` is ALSO the literate-Haskell code prefix — in a literate
        // file every code line starts with it, so a blanket marker rule
        // rejects real programs (`Setup.lhs`). Distinguish by context below:
        // a file where `> ` lines are the majority of *content* is literate
        // code, not a blockquote-heavy doc; blockquotes in docs never reach
        // majority without markers elsewhere.
        let is_marker = (trimmed.starts_with("- ") && !trimmed.starts_with("-- "))
            || trimmed.starts_with('`')
            || (trimmed.starts_with('|') && trimmed.ends_with('|'))
            || trimmed.contains("](");
        if is_marker {
            // V84: prose-marker lines are NEUTRAL — a bullet or a markdown
            // link is not code evidence, and counting it toward `code` (via
            // the `(` in `[text](url)`) was how README pages slipped past
            // the density test. They still count toward the doc-marker tally.
            markdown_lines += 1;
            continue;
        }

        // Data-like: looks like JSON key-value ("key": value) or bare string
        // JSON objects/arrays start with { or [
        // V84: also the JSON punctuation shapes — a line that is only a
        // structural bracket (`[`, `{`, `}`, `},`, `],`) or a bare quoted
        // string is data, not code. Without this, a JSON array of objects
        // leaves the bracket/string lines in the denominator and the code
        // ratio of a pure-data file creeps over the floor.
        let stripped_brackets = trimmed.trim_end_matches(',');
        let bracket_only = stripped_brackets
            .bytes()
            .all(|b| matches!(b, b'[' | b']' | b'{' | b'}' | b' '));
        let quote_cleaned = trimmed.trim_end_matches(',').trim_end_matches('\'');
        let bare_string = trimmed.starts_with('"')
            && quote_cleaned.ends_with('"')
            && quote_cleaned.len() > 1
            && !quote_cleaned[1..quote_cleaned.len() - 1].contains('"');
        if (trimmed.starts_with('"') && trimmed.contains("\":"))
            || trimmed.starts_with("{\"") || trimmed.starts_with("[{")
            || trimmed.starts_with('[') && trimmed.ends_with(']')
            || trimmed.starts_with('{') && trimmed.ends_with('}')
            || bracket_only
            || bare_string
        {
            data_lines += 1;
            continue; // data payload, not code
        }

        // V84: TOML/INI section headers (`[build]`, `[[redirects]]`) are data
        // structure, not declarations. Without this, `[[redirects]]` slips
        // past the JSON-array check and a config file can reach the ratio
        // test with a config section as its only "code".
        let bare = trimmed.trim_start_matches('[').trim_end_matches(']');
        if trimmed.starts_with('[') && trimmed.ends_with(']')
            && !bare.contains('[') && !bare.contains('\'') && !bare.contains('"')
        {
            data_lines += 1;
            continue;
        }

        // V84: `key = value` where the key is a bare identifier and the value
        // is a quoted string or bare scalar — TOML/INI/env shape. It is NOT a
        // declaration even when the key happens to be a decl-ish word
        // (`from = "/"`, `type = "lib"`). The has_decl_shape test below would
        // otherwise count it because `from` is an OCaml opener.
        let toml_binding = {
            let mut ok = false;
            if let Some(eq) = trimmed.find('=') {
                let prev = trimmed.as_bytes().get(eq.wrapping_sub(1)).copied();
                let next = trimmed.as_bytes().get(eq + 1).copied();
                let bare_op = !matches!(prev, Some(b'=') | Some(b'!') | Some(b'<') | Some(b'>'))
                    && !matches!(next, Some(b'=') | Some(b'>'));
                if bare_op {
                    let key = trimmed[..eq].trim();
                    let val = trimmed[eq + 1..].trim();
                    // V84: `key = "literal"` / `key = 'literal'` / `key =
                    // [array]` / `key = """` — config-value shapes. A bare
                    // identifier key with a quoted, bracketed, or triple-quoted
                    // value is data in every config language, and the key is
                    // never a symbol definition. This is what keeps the
                    // remaining Cargo.toml files out even though `name`,
                    // `version`, `default`, and `time` are all "identifiers".
                    ok = !key.is_empty()
                        && !key.contains(' ')
                        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
                        && (val.starts_with('"') || val.starts_with('\'')
                            || val.starts_with('[') || val.starts_with('{'));
                }
            }
            ok
        };
        if toml_binding {
            data_lines += 1;
            continue;
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
                // V78: C#/Java/Kotlin declaration forms. A declaration-only file
                // (interfaces, DTOs, enums) has no `(`, `{`-at-end or assignment
                // on its lines, so its code ratio was 0 and the file was rejected
                // as "not source" — every symbol in it silently unsearchable.
                || lower.starts_with("public ") || lower.starts_with("private ")
                || lower.starts_with("protected ") || lower.starts_with("internal ")
                || lower.starts_with("interface ") || lower.starts_with("record ")
                || lower.starts_with("enum ") || lower.starts_with("namespace ")
                || lower.starts_with("using ") || lower.starts_with("sealed ")
                || lower.starts_with("abstract ") || lower.starts_with("partial ")
        };
        let has_assignment = trimmed.contains('=') && trimmed.ends_with(';')
            && !trimmed.starts_with('=');
        // V83 B11: declaration/equation bindings are code too, even when
        // they carry no `(`, `{`, `;` or arrow — `module Fiber_io = Fiber_io`,
        // `let level = ref 0`, `type t = int`. A whole-file class of
        // OCaml module-alias files (9 .ml + 53 .mli on the ocaml-lsp corpus,
        // 62 files: every symbol silently unsearchable) has NOTHING else on
        // its first lines. Positional signal: a top-level `=` that is not an
        // operator adjacency (==, !=, <=, >=, =>) and has an identifier
        // before it — a binding shape, not prose. Prose lines (`a = b`
        // inside docs) are excluded by the file-level ratio as before:
        // this only ADDS a code line signal.
        // V83 B11b: signature/declaration shapes are code too:
        // `val print_ml : unit -> unit`, `type t`, `include module type of
        // Rpc.Client`. A line whose first token is a declaration-ish word
        // (shared predicate) with more tokens after it is source, not prose.
        // V84: strip a leading `pub(...)` qualifier group before the test so
        // `pub(crate) mod collect;` and `pub(super) use foo::Bar;` count —
        // a whole class of module/use-header files (16 tokio .rs files, 165
        // ctags sites) was rejected with code=9/98 and every symbol in them
        // was silently unsearchable.
        let has_decl_shape = {
            let mut head = trimmed;
            if head.starts_with("pub(") {
                // Skip `pub(...)` including the closing paren, then spaces.
                if let Some(close) = head.find(')') {
                    head = head[close + 1..].trim_start();
                }
            }
            let mut it = head.split_whitespace();
            if let Some(first) = it.next() {
                let more = it.next().is_some();
                let decl = crate::structural::is_declarator_word_pub(first)
                    || first == "include"
                    || first == "val"
                    || first == "type"
                    || first == "module"
                    || first == "open"
                    || first == "sig"
                    || first == "struct"
                    // V84: module/dependency loaders are declarations of a
                    // file's structure. `require_relative 'rack/version'`,
                    // `autoload :BodyProxy, "rack/body_proxy"`, `use foo::Bar;`
                    // appear in header-only files with no braces or parens.
                    || first == "require"
                    || first == "require_relative"
                    || first == "autoload"
                    || first == "load"
                    || first == "use"
                    || first == "import"
                    || first == "from"
                    // V84: macro definitions (`macro_rules!`, `macro`) are
                    // declarations; the `!` suffix marks the definition form.
                    || first.ends_with('!')
                    // V84: C preprocessor declarations in header-only files.
                    || first.starts_with('#');
                more && decl
            } else {
                false
            }
        };
        let has_binding_eq = {
            let b = trimmed.as_bytes();
            let mut found = false;
            for i in 0..b.len() {
                if b[i] == b'=' {
                    let prev = if i > 0 { b[i - 1] } else { b' ' };
                    let next = if i + 1 < b.len() { b[i + 1] } else { b' ' };
                    let op = matches!(prev, b'=' | b'!' | b'<' | b'>')
                        || matches!(next, b'=' | b'>');
                    if !op {
                        // identifier immediately before (skip spaces)
                        let mut j = i;
                        while j > 0 && (b[j - 1] == b' ' || b[j - 1] == b'\t') { j -= 1; }
                        if j > 0 && (b[j - 1].is_ascii_alphanumeric() || b[j - 1] == b'_') {
                            found = true;
                            break;
                        }
                    }
                }
            }
            found && !trimmed.starts_with('=') && !trimmed.ends_with('.')
        };

        if ends_with_brace || has_arrow || has_keyword
            || (has_call_pattern && !trimmed.ends_with(','))
            || has_assignment
            || has_binding_eq
            || has_decl_shape
        {
            code_lines += 1;
            // V84 (P-A'): split strong from weak evidence. A data/config file
            // can fake a binding (`name = "tokio"`, `version = "1.0"`) but
            // cannot fake `()`, `{`-at-end, `->`/`=>`, a declaration keyword,
            // `;`-terminated assignment, or a module/dependency header. A file
            // whose only code-shaped lines are bindings is DATA, no matter
            // how many of them there are — this is what re-rejects the 17
            // Cargo.toml files that B11's binding_eq signal admitted, while
            // keeping OCaml module aliases (`module Fiber_io = Fiber_io`,
            // which has_decl_shape also matches) and Python modules (which
            // always contain imports/defs/calls somewhere).
            let strong = ends_with_brace
                || has_arrow
                || has_keyword
                || (has_call_pattern && !trimmed.ends_with(','))
                || has_assignment
                || has_decl_shape;
            if strong {
                strong_lines += 1;
            }
        }
    }

    if non_blank == 0 {
        eprintln!("[guard:is_source_like] skipped (empty content)");
        return false;
    }
    // V84: the population is CONTENT lines — comments and data payloads are
    // neutral, neither code nor prose. Comment-heavy source (an 80% `--`
    // Haskell header, an 85% doc-comment Rust module) is still source, and
    // counting comments in the denominator was what rejected it.
    let content = non_blank.saturating_sub(comment_lines + data_lines);
    if content == 0 {
        eprintln!(
            "[guard:is_source_like] skipped (no content lines: comments={}, data={}, non_blank={})",
            comment_lines, data_lines, non_blank
        );
        return false;
    }
    let ratio = code_lines as f64 / content as f64;
    // V63f/V84: markdown-marker dominance identifies a prose document, but
    // ONLY when the file also lacks strong code evidence. A Ruby file that is
    // 70% `#` comments still has `def`/`end`/method-call lines; a README that
    // is 70% headings and bullets has none of those beyond embedded fences.
    // Requiring both keeps comment-heavy source while rejecting docs.
    let marker_dominant = non_blank >= 10
        && markdown_lines as f64 / non_blank as f64 > 0.5
        && (strong_lines as f64 / content as f64) < 0.20;
    if marker_dominant {
        eprintln!("[guard:is_source_like] skipped (markdown-dominant: {}/{} marker lines, strong={}/{})", markdown_lines, non_blank, strong_lines, content);
        return false;
    }
    // V61: small comment-heavy files (e.g. 11 doc lines + 1 fn) were rejected
    // at ratio <= 0.10 — accept any file with >= 1 code line when tiny.
    if ratio <= 0.10 && !(content < 20 && code_lines >= 1) {
        eprintln!(
            "[guard:is_source_like] skipped (ratio={:.3} <= 0.10, code={}, content={}, non_blank={})",
            ratio, code_lines, content, non_blank
        );
        return false;
    }
    // V84 (P-A'): binding-only files are data. `name = "tokio"`, `edition =
    // "2021"` are the shape of every TOML/INI/conf file, and B11's binding
    // signal admitted 17 Cargo.toml files that way. Require at least one
    // STRONG code signal (call/brace/arrow/keyword/declaration) for admission.
    // OCaml module aliases keep their decl shape; Python modules keep their
    // imports and defs; a hypothetical file of nothing but `k = v` bindings
    // is indistinguishable from config and is treated as such.
    if strong_lines == 0 {
        eprintln!(
            "[guard:is_source_like] skipped (binding-only: strong=0, content={}, non_blank={})",
            content, non_blank
        );
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
    let placeholders = std::iter::repeat_n("?", file_ids.len()).collect::<Vec<_>>().join(",");
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
#[allow(dead_code)]
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

    let result = (|| -> rusqlite::Result<usize> {
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

            // V73: ensure blocks exist for this file BEFORE reading block
            // ranges. Previously the range query ran against a possibly-empty
            // block table, so every JIT-built occurrence got block_id=0 —
            // permanently breaking block-bag similarity for phrases first
            // resolved via callgraph/type_flow. ensure_blocks_for_file_with_content
            // is SAVEPOINT-aware, so it nests safely inside our BEGIN IMMEDIATE.
            if let Err(e) = crate::lazy_tables::ensure_blocks_for_file_with_content(db, *file_id, &content) {
                eprintln!("[lazy_occurrence] ensure_blocks failed for file_id={}: {}", file_id, e);
            }

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
            // V75: carry the classifier's is_def BOOL verbatim, exactly like
            // `extract_file_phrases` does. Re-deriving it from the tag makes
            // this path disagree with ingest: the rule used to admit tags
            // 1..=6, but tag 6 is `local_binding`, which V66c deliberately made
            // is_def=false — so a `let x = ...` line was a definition here and
            // a usage at trust time. Same input, two answers depending on
            // whether the row was written by JIT or by ingest.
            let mut line_is_def: Vec<bool> = Vec::with_capacity(lines.len());
            let mut brace_depth: i32 = 0;
            // V59: capture the DEFINED NAME per line, not just a bool. The old
            // code marked every identifier on a def-line as is_def=1 — so
            // `StructuralResult` in a fn's return type inherited the fn's def
            // flag, and def-lookup returned the fn line for struct queries.
            let mut line_def_names: Vec<Option<String>> = Vec::with_capacity(lines.len());
            let defs_allowed = !crate::ingest::is_prose_path(file_path);
            let mut lex_state = crate::lex::LexState::new();
            crate::ingest::reset_comment_state();
            for line in &lines {
                let (open_count, close_count) = crate::ingest::count_braces(line);
                let prev_depth = brace_depth;
                brace_depth += open_count as i32 - close_count as i32;
                if brace_depth < 0 { brace_depth = 0; }
                let result = if !defs_allowed || !crate::lex::starts_in_code(line, &mut lex_state, false) {
                    crate::structural::StructuralResult::water()
                } else {
                    crate::structural::classify_structural(line, prev_depth, open_count > 0, false)
                };
                line_tags.push(result.tag);
                line_is_def.push(result.is_def);
                line_def_names.push(result.defined_name.map(|s| s.to_string()));
            }

        for (li, line) in lines.iter().enumerate() {
            let line_tag = line_tags.get(li).copied().unwrap_or(0);
            let line_def = line_is_def.get(li).copied().unwrap_or(false);
            let line_def_name = line_def_names.get(li).cloned().flatten();
                // V79: strip comments/strings before tokenizing, matching
                // ingest and the other two JIT paths. Scanning the raw line made
                // a name inside a string/comment (`panic!("write_vocab")`) a
                // phantom occurrence, and diverged the `col` ordinal from the
                // file-path build.
                let code_line = crate::structural::tokenizable_code(line);
                for (col, token) in crate::scan_identifiers(&code_line).into_iter().enumerate() {
                    let stemmed = crate::stem_identifier(&token);
                    if stemmed != phrase_text {
                        continue;
                    }
                    // V84: keyword-shaped DEFINED NAMES are real symbols
                    // (`fn new`, `fn default`, `From::from`). Only uses are
                    // noise. This path is per-phrase, so almost every token is
                    // a use — the exemption applies only when the classifier
                    // bound this token as the line's definition.
                    let token_is_def_name = line_def
                        && line_def_name.as_deref().map(|d| {
                            d.eq_ignore_ascii_case(&token)
                                || crate::islands::def_name_matches_token(
                                    &d.to_ascii_lowercase(),
                                    &token,
                                )
                        }).unwrap_or(false);
                    if !token_is_def_name && crate::keywords::is_noise_token(&token, &stemmed) {
                        continue;
                    }
                    let col_idx = col as i32;
                    let line_no = li as i32;  // 0-based, matches MCP `+1` convention
                    // V59/V75: is_def iff the classifier said so AND this token
                    // IS the line's defined name. Case-insensitive:
                    // scan_identifiers lowercases, the classifier returns
                    // source-case names. Operator names (`operator==`)
                    // claim their identifier token via the shared link.
                    let is_def_int = if line_def
                        && line_def_name.as_deref().map(|d| {
                            d.eq_ignore_ascii_case(&token)
                                || crate::islands::def_name_matches_token(
                                    &d.to_ascii_lowercase(),
                                    &token,
                                )
                        }).unwrap_or(false)
                    { 1 } else { 0 };
                    let tag = if is_def_int == 1 { line_tag } else { 0 };
                    let block_id = block_id_for_line(line_no);
                    stmt.execute(params![phrase_id, *file_id, line_no, col_idx, is_def_int, block_id, tag as i64])?;
                    total += 1;
                }
            }
        }
        Ok(total)
    })();

    // V60: COMMIT on success, ROLLBACK on error so the shared connection is
    // never left inside an open write transaction (a stuck BEGIN IMMEDIATE
    // would make every subsequent JIT build fail with "transaction within a
    // transaction", and callers' is_err→continue would skip the phrase
    // forever on this connection).
    match result {
        Ok(total) => {
            if let Err(e) = db.execute_batch("COMMIT") {
                let _ = db.execute_batch("ROLLBACK");
                return Err(e);
            }
            if total > 0 { bump_phrase_gen(db, phrase_id); }
            Ok(total)
        }
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(e)
        }
    }
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

    // Pre-compute line_tags, is_def and DEFINED NAMES (brace-depth tracking).
    // V73: capture the defined name per line — marking every token on a def
    // line as is_def=1 made return-type/parameter identifiers look like
    // definitions (the V59 bug), and this path was missed by that fix.
    // V75: carry the classifier's is_def VERBATIM. Re-deriving it from the tag
    // (the old `matches!(line_tag, 1..=6)`) admitted tag 6 `local_binding`,
    // which V66c made is_def=false — so this path called a `let` binding a
    // definition while ingest called it a usage.
    let mut line_tags: Vec<u8> = Vec::with_capacity(lines.len());
    let mut line_is_def: Vec<bool> = Vec::with_capacity(lines.len());
    let mut line_def_names: Vec<Option<String>> = Vec::with_capacity(lines.len());
    let mut brace_depth: i32 = 0;
    crate::ingest::reset_comment_state();
    // Step 4b wiring: flag path classifies with islands (loaded from the
    // index) + block-scoped backward sets + per-stem df guard via
    // phrase_occ blobs. Legacy path stays byte-identical when off.
    let island_ctx = load_island_ctx(db);
    // Per-file df cache: stem -> df (one PK blob decode per stem). Resolved
    // lazily (deferred) below — only for anchorless candidate names.
    let df_cache: std::cell::RefCell<FxHashMap<String, usize>> =
        std::cell::RefCell::new(FxHashMap::default());
    let mut backward_stack: Vec<std::collections::HashSet<String>> =
        vec![std::collections::HashSet::new()];
    let mut stack_depth: i32 = 0;
    // Import-group depth (see ingest: `::{` lists re-exports).
    let mut import_group_depth: Option<i32> = None;
    // Prose gate: documentation files and lines that begin inside a multi-line
    // literal never supply definitions (see ingest::extract_file_phrases_for_path).
    let defs_allowed = !crate::ingest::is_prose_path(&path);
    let mut lex_state = crate::lex::LexState::new();
    // DP segment starts for the flag path (depth-open events as proxy).
    // A2a: segment borrows the resident `&str`s — no owned clone.
    let seg_starts: std::collections::HashSet<usize> =
        if island_ctx.enabled && std::env::var("RELIARY_NO_SEG").is_err() {
        // C: cheap positional proxy (see ingest::cheap_seg_starts). The DP
        // segmenter was measured pure waste (rule2 38/84490, no recall).
        crate::ingest::cheap_seg_starts(&lines)
    } else {
        std::collections::HashSet::new()
    };
    crate::ingest::reset_comment_state();
    for (li, line) in lines.iter().enumerate() {
        let (open_count, close_count) = crate::ingest::count_braces(line);
        let prev_depth = brace_depth;
        brace_depth += open_count as i32 - close_count as i32;
        if brace_depth < 0 { brace_depth = 0; }
        if island_ctx.enabled {
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
            // V83 B4: imports must not vaccinate the backward vocabulary
            // (see ingest.rs). Multi-line groups activate import_group_depth;
            // single-line `use ...;` braces open+close on one line.
            let import_line = in_import_group || crate::islands::opens_import_group(&masked);
            // A2b/A3: analyze the header once; borrow the backward stack
            // directly (the old per-line union clone was O(L*V) copies).
            // DfGuard is DEFERRED — it resolves df only for the (rare)
            // anchorless candidate name, not every token of every line.
            let view = crate::islands::analyze_header(&masked);
            let lazy_guard = LazyDfGuard {
                db,
                cache: &df_cache,
                total_files: island_ctx.total_files,
            };
            // Import-group members never introduce (re-export list). A line
            // that begins inside a literal is prose; so is a documentation file.
            let in_literal = !crate::lex::starts_in_code(line, &mut lex_state, false);
            let island_hit = if in_import_group || !defs_allowed || in_literal {
                None
            } else {
                crate::islands::classify_island_line_raw_view(
                    line,
                    &masked,
                    &view,
                    seg_starts.contains(&(li + 1)),
                    prev_depth,
                    &backward_stack,
                    &island_ctx.islands,
                    Some(&lazy_guard as &dyn crate::islands::DfLookup),
                )
            };
            match island_hit {
                Some((name, tag)) => {
                    line_tags.push(tag);
                    line_is_def.push(true);
                    line_def_names.push(Some(name.clone()));
                    // B4: receiver-reached words never introduce (see ingest).
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
                    // V83 B5: def name -> ENCLOSING frame (see ingest.rs).
                    let frame = (prev_depth.max(0) as usize).min(backward_stack.len() - 1);
                    backward_stack[frame].insert(name);
                }
                None => {
                    line_tags.push(0);
                    line_is_def.push(false);
                    line_def_names.push(None);
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
        let result = if !defs_allowed || !crate::lex::starts_in_code(line, &mut lex_state, false) {
            crate::structural::StructuralResult::water()
        } else {
            crate::structural::classify_structural(line, prev_depth, open_count > 0, false)
        };
        line_tags.push(result.tag);
        line_is_def.push(result.is_def);
        line_def_names.push(result.defined_name.map(|s| s.to_string()));
    }

    // Single INSERT statement, reused for every token.
    let _stmt = db.prepare_cached(
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

    let result = (|| -> rusqlite::Result<usize> {
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
                for slot in map[start..=end].iter_mut() {
                    *slot = bid;
                }
            }
            map
        };

        // M6: Accumulate rows into a batch, flush in chunks of 500.
        // Multi-row INSERT is 3-10× faster than individual executes for large files.
        let mut batch: Vec<(i64, i64, i32, i32, i32, i64, i64)> = Vec::with_capacity(500);

        for (li, line) in lines.iter().enumerate() {
            let line_tag = line_tags.get(li).copied().unwrap_or(0);
            let line_def = line_is_def.get(li).copied().unwrap_or(false);
            let line_def_name = line_def_names.get(li).cloned().flatten();

            // Use pre-built block_id lookup (O(1) instead of per-line SQL).
            let block_id: i64 = *block_ids.get(li).unwrap_or(&0);

            // Path B: strip trailing // comments and string-literal contents
            // before scanning identifiers (a name in a string is not a call site).
            let code = crate::structural::tokenizable_code(line);

            for (col, token) in crate::scan_identifiers(&code).into_iter().enumerate() {
                // V39: use stem_identifier to preserve snake_case identifiers.
                // porter_stem strips the `al` suffix from `structural` → `structur`,
                // destroying compound names like `classify_structural`.
                let stemmed = crate::stem_identifier(&token);
                // V83 B3: a keyword-shaped DEFINED NAME (`fn new`,
                // `fn default`) is a real symbol; only its uses are noise.
                let is_def_name = line_def_name
                    .as_deref()
                    .map(|d| crate::islands::def_name_matches_token(d, token.as_str()))
                    .unwrap_or(false);
                if !is_def_name && crate::keywords::is_noise_token(&token, &stemmed) {
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

                // V73/V75: per-token definition flag — the classifier must have
                // marked the LINE as a definition, and only the token that IS
                // the defined name gets is_def/tag. Other identifiers on the
                // same line stay usages. (`col == 0 && line_tag >= 5` was a
                // tag-derived rule that disagreed with ingest — removed.)
                // Operator names (`operator==`) claim their identifier
                // token via the shared link (exact match would never fire:
                // punctuation never survives tokenization).
                let is_def_int = if line_def
                    && line_def_name.as_deref().map(|d| {
                        d == token.as_str()
                            || crate::islands::def_name_matches_token(
                                &d.to_ascii_lowercase(),
                                &token,
                            )
                    }).unwrap_or(false)
                {
                    1
                } else {
                    0
                };
                let tag = if is_def_int == 1 { line_tag as i64 } else { 0 };
                batch.push((phrase_id, file_id, li as i32, col as i32, is_def_int, block_id, tag));
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
        Ok(total)
    })();

    // V60: COMMIT on success, ROLLBACK on error so the shared connection is
    // never left inside an open write transaction.
    match result {
        Ok(total) => {
            if let Err(e) = db.execute_batch("COMMIT") {
                eprintln!("[lazy_occurrence] COMMIT failed for file_id={}: {}", file_id, e);
                let _ = db.execute_batch("ROLLBACK");
                return Err(e);
            }
            if total > 0 { invalidate_all_phrase_gens(); }
            Ok(total)
        }
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(e)
        }
    }
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

    // V75: carry the classifier's is_def VERBATIM (see ensure_occurrence_for_phrase).
    let mut line_tags: Vec<u8> = Vec::with_capacity(lines.len());
    let mut line_is_def: Vec<bool> = Vec::with_capacity(lines.len());
    let mut line_def_names: Vec<Option<String>> = Vec::with_capacity(lines.len());
    let mut brace_depth: i32 = 0;
    let mut lex_state = crate::lex::LexState::new();
    crate::ingest::reset_comment_state();
    for line in &lines {
        let (open_count, close_count) = crate::ingest::count_braces(line);
        let prev_depth = brace_depth;
        brace_depth += open_count as i32 - close_count as i32;
        if brace_depth < 0 { brace_depth = 0; }
        let result = if !crate::lex::starts_in_code(line, &mut lex_state, false) {
            crate::structural::StructuralResult::water()
        } else {
            crate::structural::classify_structural(line, prev_depth, open_count > 0, false)
        };
        line_tags.push(result.tag);
        line_is_def.push(result.is_def);
        line_def_names.push(result.defined_name.map(|s| s.to_string()));
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

    let result = (|| -> rusqlite::Result<usize> {
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
                for slot in map[start..=end].iter_mut() {
                    *slot = bid;
                }
            }
            map
        };

        for (li, line) in lines.iter().enumerate() {
            let line_tag = line_tags.get(li).copied().unwrap_or(0);
            let line_def = line_is_def.get(li).copied().unwrap_or(false);
            let line_def_name = line_def_names.get(li).cloned().flatten();

            // P4-3: use pre-built block_ids array (hoisted out of the loop).
            let block_id = block_ids.get(li).copied().unwrap_or(0);

            // V79: strip comments/strings before tokenizing (see the other path).
            let code_line = crate::structural::tokenizable_code(line);
            for (col, token) in crate::scan_identifiers(&code_line).into_iter().enumerate() {
                // V41: use stem_identifier to preserve snake_case identifiers.
                // porter_stem strips the `al` suffix from `structural` → `structur`,
                // which makes `classify_structural` unsearchable. stem_identifier
                // preserves the full identifier when it contains underscores.
                let stemmed = crate::stem_identifier(&token);
                if crate::keywords::is_noise_token(&token, &stemmed) {
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
                // V73/V75: only the token that IS the defined name gets
                // is_def/tag, and only when the classifier called the line a
                // definition. (`col == 0 && line_tag >= 5` was tag-derived and
                // disagreed with ingest — removed.) Operator names claim
                // their identifier token via the shared link.
                let is_def_int = if line_def
                    && line_def_name.as_deref().map(|d| {
                        d == token.as_str()
                            || crate::islands::def_name_matches_token(
                                &d.to_ascii_lowercase(),
                                &token,
                            )
                    }).unwrap_or(false)
                {
                    1
                } else {
                    0
                };
                let tag = if is_def_int == 1 { line_tag as i64 } else { 0 };
                stmt.execute(params![phrase_id, file_id, line_no, col_idx, is_def_int, block_id, tag])?;
                total += 1;
            }
        }
        Ok(total)
    })();

    // V60: COMMIT on success, ROLLBACK on error so the shared connection is
    // never left inside an open write transaction.
    match result {
        Ok(total) => {
            if let Err(e) = db.execute_batch("COMMIT") {
                let _ = db.execute_batch("ROLLBACK");
                return Err(e);
            }
            if total > 0 { invalidate_all_phrase_gens(); }
            Ok(total)
        }
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(e)
        }
    }
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

    // ── V84 admission model ────────────────────────────────────────────────
    // Each test is a negative control: it fails when the named rule is
    // removed, verified by mutation (see the V84 commit message).

    #[test]
    fn v84_type_name_before_extends_clause() {
        use crate::structural::classify_structural;
        // The declared name is BEFORE the clause, at any generic depth.
        let r = classify_structural("export interface $ZSFAny extends $ZSF {", 0, false, false);
        assert_eq!(r.defined_name, Some("ZSFAny"));
        let r = classify_structural(
            "export interface ZSFArray<PrefixItems extends ZSF[] = ZSF[]> extends ZSF {",
            0, false, false);
        assert_eq!(r.defined_name, Some("ZSFArray"), "generic-embedded extends must not cut");
        let r = classify_structural("class Foo extends Bar implements Baz {", 0, false, false);
        assert_eq!(r.defined_name, Some("Foo"), "first depth-0 clause cuts");
        let r = classify_structural("class Foo implements Baz {", 0, false, false);
        assert_eq!(r.defined_name, Some("Foo"));
        // `where` clauses still resolve the type name.
        let r = classify_structural("struct Foo<T> where T: Clone {", 0, false, false);
        assert_eq!(r.defined_name, Some("Foo"));
    }

    #[test]
    fn v84_pointfree_definitions() {
        use crate::structural::pointfree_name;
        // Haskell/OCaml point-free and module constants are definitions.
        assert_eq!(pointfree_name("arrayValues = List.map f xs", 0), Some("arrayValues"));
        assert_eq!(pointfree_name("MAX_RETRIES = 5", 0), Some("MAX_RETRIES"));
        assert_eq!(pointfree_name("externalNames = Set.fromList names", 0), Some("externalNames"));
        // Indented statements are local bindings. The prefix is a single
        // valid identifier, so only the indent gate can reject it.
        assert_eq!(pointfree_name("    value = 5", 0), None);
        assert_eq!(pointfree_name("\tvalue = 5", 0), None);
        // Binder-led value bindings are not definitions. `let`/`const` are
        // single words, so only the binder gate can reject them.
        assert_eq!(pointfree_name("let = 5", 0), None);
        assert_eq!(pointfree_name("const = 5", 0), None);
        assert_eq!(pointfree_name("pub = 5", 0), None);
        // Multi-word binder-led forms stay rejected by the purity rule.
        assert_eq!(pointfree_name("let bg = callee()", 0), None);
        assert_eq!(pointfree_name("pub const FOO = 1;", 0), None);
        // Member/index/annotation assignment is not a definition.
        assert_eq!(pointfree_name("a.b = 1", 0), None);
        assert_eq!(pointfree_name("x[0] = 1", 0), None);
        assert_eq!(pointfree_name("x: i32 = 5", 0), None);
        // PascalCase heads are types/constructors, handled elsewhere.
        assert_eq!(pointfree_name("Foo = bar", 0), None);
        // Nested scopes are never module-level definitions.
        assert_eq!(pointfree_name("value = 42", 1), None);
        // Control-flow and empty RHS.
        assert_eq!(pointfree_name("if x = y", 0), None);
        assert_eq!(pointfree_name("x =", 0), None);
    }

    #[test]
    fn v84_long_identifiers_are_indexed() {
        // The scanner cap was 40 bytes; real test names and generated
        // symbols exceed it. 80-byte identifiers must round-trip.
        let long = "a".repeat(80);
        let line = format!("fn {long}() {{ }}");
        let toks = crate::scan_identifiers(&line);
        assert!(toks.iter().any(|t| t == &long), "80-char identifier must be scanned");
        assert!(crate::scan_identifiers_with_offsets(&line).iter().any(|(_, t)| t == &long));
        // The cap still bounds pathological input.
        let huge = "b".repeat(140);
        assert!(!crate::scan_identifiers(&huge).iter().any(|t| t.len() > 100));
    }

    #[test]
    fn v84_comment_heavy_source_is_admitted() {
        // 60 comment lines + 4 real declarations. Comments must be NEUTRAL:
        // if they re-enter the denominator, density falls to 4/64 and the
        // file is rejected even though it is source.
        let mut src = String::new();
        for _ in 0..60 { src.push_str("-- | module documentation line\n"); }
        src.push_str("module Data.Thing (thing) where\n");
        src.push_str("thing :: Int -> Int\nthing x = x + 1\n");
        assert!(is_source_like(&src), "comment-heavy source must be admitted");
    }

    #[test]
    fn v84_binding_only_config_is_rejected() {
        // Every line is `key = value` — no call, brace, arrow, or keyword.
        let cfg = "[package]\nname = \"tokio\"\nversion = \"1.0\"\n"
            .to_string()
            + "edition = \"2021\"\nlicense = \"MIT\"\n";
        assert!(!is_source_like(&cfg), "binding-only config must be rejected");
        // TOML key that is also an OCaml opener (`from`) and a quoted value.
        let redirect = "from = \"/\"\nto = \"/tokio\"\npublish = \"target/doc\"\n"
            .to_string()
            + "[build]\ncommand = \"cargo doc\"\n";
        assert!(!is_source_like(&redirect), "quoted `key = value` is data");
    }

    #[test]
    fn v84_markdown_links_are_not_code() {
        // 40 markdown link lines. The `(url)` parens must not count as code;
        // if the link-marker rule is removed, density clears the floor.
        let mut doc = String::new();
        for i in 0..40 {
            doc.push_str(&format!("See [page {i}](https://example.com/{i}) for details.\n"));
        }
        assert!(!is_source_like(&doc), "markdown link lines must not be code");
    }

    #[test]
    fn v84_declaration_only_header_files_are_admitted() {
        // Module/use header files with no definitions of their own — the
        // class that was silently unsearchable (16 tokio .rs files).
        let rs = "pub(crate) mod collect;\nmod fuse;\nmod next;\n"
            .to_string()
            + "pub use take_while::TakeWhile;\nuse core::future::Future;\n"
            + "pub(crate) mod timeout;\n";
        assert!(is_source_like(&rs), "declaration-only Rust module file must be admitted");
        let mli = "val print_ml : unit -> unit\nval parse : string -> ast\n"
            .to_string()
            + "type t\ninclude module type of Rpc.Client\n";
        assert!(is_source_like(&mli), "OCaml .mli declaration file must be admitted");
    }

    #[test]
    fn v84_doc_with_code_fences_is_rejected() {
        // Docs pages that embed example code: headings dominate, the fenced
        // examples are binding-shaped, and there are just enough strong lines
        // to clear the ratio test. Marker dominance (weak strong-share) is
        // the only rule that separates this from a source file.
        let mut doc = String::new();
        for i in 0..20 { doc.push_str(&format!("# Section {i}\n")); }
        for i in 0..15 { doc.push_str(&format!("setting{i} = {i}\n")); }
        doc.push_str("fn example() {\n}\n");
        doc.push_str("fn other() {\n}\n");
        assert!(!is_source_like(&doc), "fence-heavy doc must be rejected");
    }

    #[test]
    fn v84_unquoted_binding_file_is_rejected() {
        // `.ini`/`.env` shape with bare values: every line parses as a weak
        // binding, so the density test passes. The strong-signal gate is the
        // only thing that rejects it.
        let mut cfg = String::new();
        for i in 0..30 { cfg.push_str(&format!("setting_{i} = {i}\n")); }
        assert!(!is_source_like(&cfg), "binding-only (unquoted) config must be rejected");
    }

    #[test]
    fn v84_json_payload_is_rejected() {
        // Structural brackets and bare strings are data; only the quoted
        // key-value lines were previously recognised, so the file cleared
        // the density floor on its brackets.
        let mut json = String::from("[\n");
        for i in 0..30 {
            json.push_str(&format!("  {{\n    \"id\": {i},\n    \"name\": \"user\"\n  }},\n"));
        }
        json.push_str("]\n");
        assert!(!is_source_like(&json), "JSON payload must be rejected");
    }
}