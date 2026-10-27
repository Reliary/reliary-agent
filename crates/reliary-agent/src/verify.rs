//! V70 P1: `reliary verify` — mechanical claim verification.
//!
//! Extracts (symbol, file, line) claims from text and checks each against the
//! index. Deterministic: same text + same index => same verdict. Ported from
//! bench/deterministic_verify.py with identical claim forms and ±1 tolerance.

use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    /// Symbol name, or empty for file:line-only claims.
    pub symbol: String,
    /// Basename of the file (e.g. "structural.rs").
    pub file: String,
    /// 1-indexed line as written in the claim.
    pub line: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The claim matches the index AND the source on disk.
    Verified { actual: Option<(String, i32)> },
    /// The claim does not match. `actual` is a source-backed hint when the
    /// source was readable, index-derived otherwise.
    False { actual: Option<(String, i32)> },
    /// The claim could not be checked against the source: the file is gone, or
    /// it changed after the index was built. Not the same as FALSE — reporting
    /// it as FALSE would call a correct claim a lie because the index is old.
    UnverifiedStale { reason: String },
}

#[derive(Debug, Default, Clone)]
pub struct VerifySummary {
    pub verified: usize,
    pub falsified: usize,
    pub unverified: usize,
    pub claims: Vec<(Claim, Verdict)>,
}

/// Source-file extensions the verifier recognises in `file:line` citations.
/// Grammar-free: this is a citation-shape list, not language analysis, and it
/// mirrors the Python verifier (`bench/deterministic_verify.py`) so the two
/// agree. The pattern is built once from this list.
const SRC_EXT: &str =
    "rs|py|go|ts|tsx|js|jsx|c|cc|cpp|h|hpp|java|rb|php|cs|kt|swift|scala|m|mm|dart|lua|pl|sh|bash|tex";

fn src_ext_pattern() -> &'static str {
    static R: OnceLock<String> = OnceLock::new();
    R.get_or_init(|| format!("[A-Za-z0-9_./-]+\\.(?:{})", SRC_EXT))
}

fn re_sym_at_file_line() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // V70 P6: symbol before "at/in f:l". The optional `(?:is\s+)?` plus
        // skipping copula/verb English words lets "X is defined at f:l" and
        // "X defined at f:l" both capture X, not "defined".
        // V74: a leading `Type::` is captured in group 1 so `Runtime::block_on`
        // verifies `block_on` (the old non-capturing group verified `Runtime`).
        Regex::new(&format!(
            r"([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s+(?:is\s+)?(?:defined|declared|located|found|called)?\s*(?:at|in)\s+({}):(\d+)",
            src_ext_pattern()
        ))
        .expect("re_sym_at_file_line")
    })
}

fn re_sym_in_file_line() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(&format!(
            r"([A-Za-z_][A-Za-z0-9_]*)\s+(?:is\s+)?defined\s+in\s+({})\s+at\s+line\s+(\d+)",
            src_ext_pattern()
        ))
        .expect("re_sym_in_file_line")
    })
}

fn re_file_line() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(&format!(r"({}):(\d+)", src_ext_pattern())).expect("re_file_line")
    })
}

fn re_paren_file_line() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(&format!(r"\(({}):(\d+)\)", src_ext_pattern())).expect("re_paren_file_line")
    })
}

fn re_file_at_lines() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(&format!(
            r"({})\s+(?:at\s+)?lines?\s+(\d+(?:\s*,\s*\d+)*)",
            src_ext_pattern()
        ))
        .expect("re_file_at_lines")
    })
}

fn re_file_paren_lines() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(&format!(
            r"({})\s*\(\s*lines?\s+(\d+(?:\s*,\s*\d+)*)",
            src_ext_pattern()
        ))
        .expect("re_file_paren_lines")
    })
}

/// English words that appear in "X at file:line" position but are not symbols.
const ENGLISH_STOP: &[&str] = &[
    "is", "was", "are", "were", "be", "been", "the", "a", "an", "it", "this",
    "that", "which", "who", "what", "when", "where", "here", "there", "defined",
    "declared", "located", "found", "called", "returns", "return", "takes",
    "accepts", "signature", "declaration", "implementation", "impl", "method",
    "function", "struct", "type", "trait", "field", "fields",
];

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Escape LIKE metacharacters (`\`, `%`, `_`) with a backslash so a filename is
/// matched literally (paired with `ESCAPE '\'`).
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Extract all verifiable claims from text. Mirrors the Python extractor:
/// backticks stripped, six claim forms, English-stop symbols blanked.
pub fn extract_claims(text: &str) -> Vec<Claim> {
    let text = text.replace('`', "");
    let mut out: Vec<Claim> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String, i32)> =
        std::collections::HashSet::new();
    let push = |sym: String, file: String, line: i32, out: &mut Vec<Claim>,
                    seen: &mut std::collections::HashSet<(String, String, i32)>| {
        let key = (sym.clone(), file.clone(), line);
        if seen.insert(key) {
            out.push(Claim { symbol: sym, file, line });
        }
    };

    for cap in re_sym_at_file_line().captures_iter(&text) {
        // V74: for `Type::method` claims verify the METHOD, not the type —
        // the regex captures the full qualified name, then we take the last
        // segment. (`Runtime::block_on at x.rs:1` verified Runtime before.)
        let mut sym = cap[1].rsplit("::").next().unwrap_or(&cap[1]).to_string();
        if ENGLISH_STOP.contains(&sym.to_lowercase().as_str()) {
            sym = String::new();
        }
        // V74: prose words are handled in verify_claim (which has the index):
        // an unknown bare lowercase word is downgraded to a file-only claim
        // there, so the claim still verifies the location without inventing a
        // FALSE symbol claim.
        let file = basename(&cap[2]);
        let line: i32 = cap[3].parse().unwrap_or(0);
        push(sym, file, line, &mut out, &mut seen);
    }
    for cap in re_file_line().captures_iter(&text) {
        push(String::new(), basename(&cap[1]), cap[2].parse().unwrap_or(0), &mut out, &mut seen);
    }
    for cap in re_paren_file_line().captures_iter(&text) {
        push(String::new(), basename(&cap[1]), cap[2].parse().unwrap_or(0), &mut out, &mut seen);
    }
    for cap in re_sym_in_file_line().captures_iter(&text) {
        let mut sym = cap[1].to_string();
        if ENGLISH_STOP.contains(&sym.to_lowercase().as_str()) {
            sym = String::new();
        }
        push(sym, basename(&cap[2]), cap[3].parse().unwrap_or(0), &mut out, &mut seen);
    }
    for cap in re_file_at_lines().captures_iter(&text) {
        let file = basename(&cap[1]);
        for n in cap[2].split(',') {
            if let Ok(line) = n.trim().parse::<i32>() {
                push(String::new(), file.clone(), line, &mut out, &mut seen);
            }
        }
    }
    for cap in re_file_paren_lines().captures_iter(&text) {
        let file = basename(&cap[1]);
        for n in cap[2].split(',') {
            if let Ok(line) = n.trim().parse::<i32>() {
                push(String::new(), file.clone(), line, &mut out, &mut seen);
            }
        }
    }
    out
}

/// Read a bounded window of a file from DISK and return the lines.
///
/// `None` when the file cannot be read (deleted, moved, permissions). This is
/// the difference between "the index says no" and "the source is gone": the
/// first is a verdict, the second is not.
fn read_source_window(path: &str, first: i64, last: i64) -> Option<Vec<String>> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let lo = first.max(1) as usize;
    let hi = last.max(first) as usize;
    if hi.saturating_sub(lo) > 500 {
        return None; // bounded: never read a large window into memory
    }
    let mut out = Vec::new();
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let n = i + 1;
        if n > hi {
            break;
        }
        if n >= lo {
            out.push(line.ok()?);
        }
    }
    Some(out)
}

/// Does `line` contain `symbol` as a whole identifier?
///
/// Whole-word only: `Buf` must not match `buffer`, and `peek` must not match
/// `peekable`. This is what stops a phantom index row from verifying, because
/// the source is the authority on whether the name is there.
fn contains_whole_ident(line: &str, symbol: &str) -> bool {
    if symbol.is_empty() {
        return true;
    }
    let b = line.as_bytes();
    let mut from = 0usize;
    while let Some(pos) = line[from..].find(symbol) {
        let at = from + pos;
        let end = at + symbol.len();
        let before_ok = at == 0 || (!b[at - 1].is_ascii_alphanumeric() && b[at - 1] != b'_');
        let after_ok = end >= b.len() || (!b[end].is_ascii_alphanumeric() && b[end] != b'_');
        if before_ok && after_ok {
            return true;
        }
        from = at + 1;
    }
    false
}

/// Verify one claim against the index (±tol lines).
pub fn verify_claim(db: &Connection, claim: &Claim, tol: i32) -> Verdict {
    // Clamp tolerance and the line number so `line - 1 - tol` / `line - 1 + tol`
    // cannot overflow (a huge client-supplied line or tol panicked the server).
    let tol = tol.clamp(0, 10_000);
    let line = claim.line.clamp(0, 50_000_000);
    let claim = &Claim { symbol: claim.symbol.clone(), file: claim.file.clone(), line };
    if claim.symbol.is_empty() {
        // file:line-only claim: the file must exist in the index AND the
        // claimed line must be within the file's line count. Previously any
        // line number verified as long as the file existed (foo.rs:99999 OK).
        // V79: match the basename exactly (the claim stores a basename). `_` and
        // `%` are LIKE metacharacters, so `foo_bar.rs` used to match `fooXbar.rs`;
        // and an unordered LIMIT 1 picked an arbitrary same-named file. Now the
        // suffix is matched with an explicit ESCAPE and the shortest path wins
        // deterministically.
        let suffix = format!("%/{}", escape_like(&claim.file));
        let file_info: Option<(String, i64)> = db
            .query_row(
                "SELECT fm.file_path, COALESCE(fs.content_len, 0) FROM file_map fm
                 LEFT JOIN file_stats fs ON fs.file_id = fm.id
                 WHERE (fm.file_path = ?1 OR fm.file_path LIKE ?2 ESCAPE '\\')
                 ORDER BY LENGTH(fm.file_path) ASC, fm.file_path ASC LIMIT 1",
                params![claim.file, suffix],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()
            .inspect_err(|e| eprintln!("[verify] file lookup failed: {}", e))
            .unwrap_or(None); // None = file not indexed → a real FALSE
        let Some((path, content_len)) = file_info else {
            return Verdict::False { actual: None };
        };
        if content_len > 0 {
            // Sound line bound: a file of `content_len` bytes has at most
            // `content_len` lines (every line is >= 1 byte), so a claim beyond
            // that is impossible. The old `content_len/8 + 1000` bound was ~8x
            // too generous and rubber-stamped e.g. `foo.rs:5000` in a 300-line
            // file.
            if claim.line as i64 > content_len {
                return Verdict::False { actual: None };
            }
            // Tighter check when the occurrence table is populated for this
            // file: the claimed line must actually hold a token. Only apply it
            // when the file HAS occurrence rows — on a lazy index the table is
            // empty and this would produce false negatives.
            let has_occ: i64 = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM occurrence o JOIN file_map f ON f.id=o.file_id
                                   WHERE f.file_path = ?1 LIMIT 1)",
                    params![path], |r| r.get(0),
                )
                .unwrap_or(0);
            if has_occ > 0 {
                let line_exists: i64 = db
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM occurrence o JOIN file_map f ON f.id=o.file_id
                                       WHERE f.file_path = ?1 AND o.line BETWEEN ?2 AND ?3 LIMIT 1)",
                        params![path, claim.line - 1 - tol, claim.line - 1 + tol],
                        |r| r.get(0),
                    )
                    .inspect_err(|e| eprintln!("[verify] line existence check failed: {}", e))
                    .unwrap_or(1);
                if line_exists == 0 {
                    return Verdict::False { actual: None };
                }
            }
        }
        // V99: the index says this file has that many lines; the SOURCE says
        // whether it still exists. A deleted file is unverified, never verified.
        return match read_source_window(&path, claim.line as i64, claim.line as i64) {
            Some(window) => {
                if window.is_empty() {
                    Verdict::False { actual: None }
                } else {
                    Verdict::Verified { actual: None }
                }
            }
            None => Verdict::UnverifiedStale {
                reason: format!("cannot read {path} (file moved or deleted since indexing)"),
            },
        }
    }
    // Symbol claim: resolve phrase then check occurrence at file ±tol.
    let stemmed = reliary_search::stem_identifier(&claim.symbol);
    // V74: a DB error must not be silently reported as FALSE — the index
    // could be locked or corrupt, and the claim is unverifiable, not wrong.
    let pid: Option<i64> = match reliary_search::symbol::phrase_id_for(db, &stemmed) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[verify] phrase lookup failed for {:?}: {}", claim.symbol, e);
            return Verdict::False { actual: None }; // keep FALSE for the CLI contract
        }
    };
    let Some(pid) = pid else {
        // V74: an unknown PROSE word (all-lowercase, no underscore/digit) is
        // not a false symbol claim — degrade to a file:line check so the row
        // doesn't pollute precision.
        let is_prose = claim.symbol.chars().all(|c| c.is_ascii_lowercase())
            && !claim.symbol.contains('_')
            && !claim.symbol.contains(|c: char| c.is_ascii_digit());
        if is_prose {
            let location_ok: i64 = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM occurrence o JOIN file_map f ON f.id=o.file_id
                                   WHERE f.file_path LIKE ?1 AND o.line BETWEEN ?2 AND ?3 LIMIT 1)",
                    params![format!("%/{}", claim.file), claim.line - 1 - tol, claim.line - 1 + tol],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            // V99: a prose word is only accepted when the INDEX knows nothing
            // of it (unknown-name case). If the source line cannot be read we
            // say so instead of guessing.
            if location_ok == 0 {
                return Verdict::False { actual: None };
            }
            let path: Option<String> = db
                .query_row(
                    "SELECT file_path FROM file_map WHERE file_path LIKE ?1 LIMIT 1",
                    params![format!("%/{}", claim.file)],
                    |r| r.get(0),
                )
                .optional()
                .unwrap_or(None);
            return match path.and_then(|p| {
                read_source_window(&p, (claim.line - tol) as i64, (claim.line + tol) as i64)
            }) {
                Some(window) => {
                    // The prose word must be present in the source line, not
                    // merely "some token is here".
                    if window.iter().any(|l| contains_whole_ident(l, &claim.symbol)) {
                        Verdict::Verified { actual: None }
                    } else {
                        Verdict::False { actual: None }
                    }
                }
                None => Verdict::UnverifiedStale {
                    reason: format!(
                        "cannot read {} (file moved or deleted since indexing)",
                        claim.file
                    ),
                },
            };
        }
        return Verdict::False { actual: None };
    };
    let hit: Option<(String, i32)> = db
        .query_row(
            "SELECT f.file_path, o.line FROM occurrence o
             JOIN file_map f ON f.id = o.file_id
             WHERE o.phrase_id = ?1 AND f.file_path LIKE ?2
               AND o.line BETWEEN ?3 AND ?4 AND o.is_def = 1
               AND f.is_source = 1 AND f.file_path NOT LIKE '%.md'
               AND f.file_path NOT LIKE '%/docs/%'
             ORDER BY ABS(o.line - ?5) ASC LIMIT 1",
            params![
                pid,
                format!("%/{}", claim.file),
                claim.line - 1 - tol,
                claim.line - 1 + tol,
                claim.line - 1
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?)),
        )
        .optional()
        .inspect_err(|e| eprintln!("[verify] occurrence lookup failed: {}", e))
        .unwrap_or(None); // None = no occurrence at this line → FALSE
    match hit {
        Some((path, line0)) => {
            let actual = Some((basename(&path), line0 + 1));
            // Source is the authority. The index can hold a phantom definition
            // (a stale row, a misclassified line) and it can lag the working
            // tree; verifying against it alone answers "the index agrees",
            // which is not what a caller is asking.
            match read_source_window(&path, (line0 + 1 - tol) as i64, (line0 + 1 + tol) as i64) {
                Some(window) => {
                    if claim.symbol.is_empty()
                        || window.iter().any(|l| contains_whole_ident(l, &claim.symbol))
                    {
                        Verdict::Verified { actual }
                    } else {
                        // The index has a def here but the source does not carry
                        // the name: the entry is stale or wrong. Search the
                        // window's neighbours is not worth a second read; report
                        // FALSE with the index's claim as the hint.
                        Verdict::False { actual }
                    }
                }
                None => Verdict::UnverifiedStale {
                    reason: format!("cannot read {path} (file moved or deleted since indexing)"),
                },
            }
        }
        None => {
            // Best-effort actual location for the FALSE message.
            let actual: Option<(String, i32)> = db
                .query_row(
                    "SELECT f.file_path, o.line FROM occurrence o
                     JOIN file_map f ON f.id = o.file_id
                     WHERE o.phrase_id = ?1 AND o.is_def = 1
                       AND f.is_source = 1 AND f.file_path NOT LIKE '%.md'
                       AND f.file_path NOT LIKE '%/docs/%'
                     ORDER BY o.line ASC LIMIT 1",
                    params![pid],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?)),
                )
                .ok()
                .map(|(p, l)| (basename(&p), l + 1));
            Verdict::False { actual }
        }
    }
}

/// Extract and verify every claim in text.
pub fn verify_text(db: &Connection, text: &str, tol: i32) -> VerifySummary {
    let mut summary = VerifySummary::default();
    for claim in extract_claims(text) {
        let verdict = verify_claim(db, &claim, tol);
        match verdict {
            Verdict::Verified { .. } => summary.verified += 1,
            Verdict::False { .. } => summary.falsified += 1,
            Verdict::UnverifiedStale { .. } => summary.unverified += 1,
        }
        summary.claims.push((claim, verdict));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_backed_verdicts() {
        let dir = std::env::temp_dir().join(format!("reliary-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("s.rs");
        std::fs::write(&f, "fn alpha() {}\nfn beta() { let buffer = 1; }\n").unwrap();
        let db = Connection::open_in_memory().unwrap();
        reliary_search::schema::create_new_db(&db).unwrap();
        reliary_search::ingest::index_directory(&db, dir.to_str().unwrap()).unwrap();
        reliary_search::lazy_occurrence::build_all_occurrence(&db).unwrap();

        // 1. A real symbol at the real line verifies.
        let c = Claim { symbol: "alpha".into(), file: "s.rs".into(), line: 1 };
        assert!(matches!(verify_claim(&db, &c, 1), Verdict::Verified { .. }));

        // 2. Whole-identifier only: `beta` must not verify because `buffer`
        //    contains it, and vice versa. This is the rule that stops a phantom
        //    match from being accepted.
        let c = Claim { symbol: "bet".into(), file: "s.rs".into(), line: 2 };
        assert!(matches!(verify_claim(&db, &c, 1), Verdict::False { .. } | Verdict::UnverifiedStale { .. }));

        // 3. A deleted file is UNVERIFIED, not FALSE: the index still holds it.
        std::fs::remove_file(&f).unwrap();
        let c = Claim { symbol: "alpha".into(), file: "s.rs".into(), line: 1 };
        match verify_claim(&db, &c, 1) {
            Verdict::UnverifiedStale { .. } => {}
            other => panic!("deleted file must be stale, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extracts_six_claim_forms() {
        let claims = extract_claims(
            "classify_structural at structural.rs:31, also (brace_graph.rs:27), \
             defined in search.rs at line 154, session.rs at lines 28, 39, \
             take.rs (lines 8, 56), and plain lazy_occurrence.rs:453",
        );
        let keys: Vec<(String, String, i32)> =
            claims.iter().map(|c| (c.symbol.clone(), c.file.clone(), c.line)).collect();
        assert!(keys.contains(&("classify_structural".into(), "structural.rs".into(), 31)));
        assert!(keys.contains(&("".into(), "brace_graph.rs".into(), 27)));
        assert!(keys.contains(&("".into(), "search.rs".into(), 154)));
        assert!(keys.contains(&("".into(), "session.rs".into(), 28)));
        assert!(keys.contains(&("".into(), "session.rs".into(), 39)));
        assert!(keys.contains(&("".into(), "take.rs".into(), 8)));
        assert!(keys.contains(&("".into(), "take.rs".into(), 56)));
        assert!(keys.contains(&("".into(), "lazy_occurrence.rs".into(), 453)));
    }

    #[test]
    fn english_words_are_not_symbols() {
        let claims = extract_claims("it is defined at structural.rs:31");
        // "defined at" — the word before "at" is "defined" (stopword) => blank symbol
        assert!(claims.iter().any(|c| c.symbol.is_empty() && c.file == "structural.rs"));
        assert!(claims.iter().all(|c| c.symbol != "defined"));
    }

    #[test]
    fn backticks_stripped() {
        let claims = extract_claims("`foo_bar` at `structural.rs:31`");
        assert!(claims.iter().any(|c| c.symbol == "foo_bar" && c.file == "structural.rs"));
    }

    #[test]
    fn empty_text_no_claims() {
        assert!(extract_claims("").is_empty());
        assert!(extract_claims("no claims here at all").is_empty());
    }

    #[test]
    fn extracts_claims_for_non_rust_languages() {
        // V78: the extractor was hardcoded to `.rs`, so `verify` silently found
        // no claims on a .NET/Java/TS corpus. Every source extension the index
        // handles must be recognised.
        for (text, sym, file, line) in [
            ("RoleConfigurationController at RoleConfigurationController.cs:16", "RoleConfigurationController", "RoleConfigurationController.cs", 16),
            ("handleRequest defined in Server.java at line 42", "handleRequest", "Server.java", 42),
            ("fetchUser at api/client.ts:99", "fetchUser", "client.ts", 99),
            ("Process at worker.go:7", "Process", "worker.go", 7),
            ("Widget at widget.py:3", "Widget", "widget.py", 3),
        ] {
            let claims = extract_claims(text);
            let hit = claims.iter().any(|c| c.symbol == sym && c.file == file && c.line == line);
            assert!(hit, "no claim parsed from {:?} (got {:?})", text, claims);
        }
    }

    #[test]
    fn line_bound_rejects_impossible_lines() {
        // A file:line far beyond a small file's line count must be FALSE, not
        // rubber-stamped. Build a tiny index and query an absurd line.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("reliary_v79v_{}", nanos));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("small.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let db = Connection::open(dir.join("i.sqlite")).unwrap();
        reliary_search::schema::create_new_db(&db).unwrap();
        reliary_search::ingest::index_directory(&db, dir.to_str().unwrap()).unwrap();

        let far = Claim { symbol: String::new(), file: "small.rs".into(), line: 5000 };
        assert!(matches!(verify_claim(&db, &far, 1), Verdict::False { .. }),
            "a line beyond the file must be FALSE");
        // Negative control: a real line still verifies.
        let ok = Claim { symbol: String::new(), file: "small.rs".into(), line: 1 };
        assert!(matches!(verify_claim(&db, &ok, 1), Verdict::Verified { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn huge_tol_does_not_overflow() {
        let db = Connection::open_in_memory().unwrap();
        reliary_search::schema::create_new_db(&db).unwrap();
        // Must not panic.
        let c = Claim { symbol: String::new(), file: "x.rs".into(), line: i32::MAX };
        let _ = verify_claim(&db, &c, i32::MAX);
        let _ = verify_claim(&db, &c, i32::MIN);
    }

    #[test]
    fn non_source_citations_are_not_claims() {
        // Negative control: prose/README citations must not become claims, or
        // the extractor would credit locations that are not code.
        let claims = extract_claims("see README.md:12 and notes.txt:3 for details");
        assert!(claims.iter().all(|c| c.file != "README.md" && c.file != "notes.txt"),
            "non-source citations leaked: {:?}", claims);
    }

    #[test]
    fn dedupes_identical_claims() {
        // Python parity: "x at a.rs:1" yields TWO distinct claims — the
        // symbol form ("x", a.rs, 1) and the bare file:line form ("", a.rs, 1).
        // Repeats of each form dedupe to one.
        let claims = extract_claims("x at a.rs:1 and x at a.rs:1");
        assert_eq!(claims.len(), 2);
        let sym_claims: Vec<_> = claims.iter().filter(|c| !c.symbol.is_empty()).collect();
        let file_claims: Vec<_> = claims.iter().filter(|c| c.symbol.is_empty()).collect();
        assert_eq!(sym_claims.len(), 1);
        assert_eq!(file_claims.len(), 1);
    }
}
