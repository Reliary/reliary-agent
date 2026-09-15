//! V70 P1: `reliary verify` — mechanical claim verification.
//!
//! Extracts (symbol, file, line) claims from text and checks each against the
//! index. Deterministic: same text + same index => same verdict. Ported from
//! bench/deterministic_verify.py with identical claim forms and ±1 tolerance.

use regex::Regex;
use rusqlite::{params, Connection};
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
    /// The claim matches the index.
    Verified { actual: Option<(String, i32)> },
    /// The claim does not match the index (symbol+file+line not found).
    False { actual: Option<(String, i32)> },
}

#[derive(Debug, Default, Clone)]
pub struct VerifySummary {
    pub verified: usize,
    pub falsified: usize,
    pub claims: Vec<(Claim, Verdict)>,
}

fn re_sym_at_file_line() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // V70 P6: symbol before "at/in f:l". The optional `(?:is\s+)?` plus
        // skipping copula/verb English words lets "X is defined at f:l" and
        // "X defined at f:l" both capture X, not "defined".
        // V74: a leading `Type::` is captured in group 1 so `Runtime::block_on`
        // verifies `block_on` (the old non-capturing group verified `Runtime`).
        Regex::new(
            r"([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s+(?:is\s+)?(?:defined|declared|located|found|called)?\s*(?:at|in)\s+([A-Za-z0-9_./-]+\.rs):(\d+)",
        )
        .expect("re_sym_at_file_line")
    })
}

fn re_sym_in_file_line() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"([A-Za-z_][A-Za-z0-9_]*)\s+(?:is\s+)?defined\s+in\s+([A-Za-z0-9_./-]+\.rs)\s+at\s+line\s+(\d+)",
        )
        .expect("re_sym_in_file_line")
    })
}

fn re_file_line() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"([A-Za-z0-9_./-]+\.rs):(\d+)").expect("re_file_line"))
}

fn re_paren_file_line() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\(([A-Za-z0-9_./-]+\.rs):(\d+)\)").expect("re_paren_file_line"))
}

fn re_file_at_lines() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"([A-Za-z0-9_./-]+\.rs)\s+(?:at\s+)?lines?\s+(\d+(?:\s*,\s*\d+)*)")
            .expect("re_file_at_lines")
    })
}

fn re_file_paren_lines() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"([A-Za-z0-9_./-]+\.rs)\s*\(\s*lines?\s+(\d+(?:\s*,\s*\d+)*)")
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

/// Extract all verifiable claims from text. Mirrors the Python extractor:
/// backticks stripped, six claim forms, English-stop symbols blanked.
pub fn extract_claims(text: &str) -> Vec<Claim> {
    let text = text.replace('`', "");
    let mut out: Vec<Claim> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String, i32)> =
        std::collections::HashSet::new();
    let mut push = |sym: String, file: String, line: i32, out: &mut Vec<Claim>,
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

/// Verify one claim against the index (±tol lines).
pub fn verify_claim(db: &Connection, claim: &Claim, tol: i32) -> Verdict {
    if claim.symbol.is_empty() {
        // file:line-only claim: the file must exist in the index AND the
        // claimed line must be within the file's line count. Previously any
        // line number verified as long as the file existed (foo.rs:99999 OK).
        let file_info: Option<(String, i64)> = db
            .query_row(
                "SELECT fm.file_path, COALESCE(fs.content_len, 0) FROM file_map fm
                 LEFT JOIN file_stats fs ON fs.file_id = fm.id
                 WHERE fm.file_path LIKE ?1 LIMIT 1",
                params![format!("%/{}", claim.file)],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .ok(); // GUARDED: intentional — None means "file not indexed", a real FALSE
        let Some((path, content_len)) = file_info else {
            return Verdict::False { actual: None };
        };
        if content_len > 0 {
            // content_len is bytes; a generous line bound avoids false negatives
            // on long lines. Exact check is via occurrence rows when available.
            let line_exists: i64 = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM occurrence o JOIN file_map f ON f.id=o.file_id
                                   WHERE f.file_path = ?1 AND o.line BETWEEN ?2 AND ?3 LIMIT 1)",
                    params![path, claim.line - 1 - tol, claim.line - 1 + tol],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if line_exists == 0 && claim.line as i64 > content_len / 8 + 1000 {
                return Verdict::False { actual: None };
            }
        }
        return Verdict::Verified { actual: None };
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
            return if location_ok > 0 {
                Verdict::Verified { actual: None }
            } else {
                Verdict::False { actual: None }
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
        .ok(); // GUARDED: intentional — miss means "no occurrence at this line", not an error
    match hit {
        Some((path, line0)) => Verdict::Verified {
            actual: Some((basename(&path), line0 + 1)),
        },
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
        }
        summary.claims.push((claim, verdict));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

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
