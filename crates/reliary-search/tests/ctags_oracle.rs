//! Differential oracle: universal-ctags vs our def-set over fixture files.
//!
//! ctags is a TEST dependency only (never in the binary). If the `ctags`
//! binary is absent the test skips cleanly (same policy as missing git).
//! CI installs it (`universal-ctags` via apt).
//!
//! Adjudication rule (C1): on disagreement, source truth wins. Shapes we
//! deliberately do not index (function pointers, operator overloads, bare
//! `#if EXPR`) live in KNOWN_GAPS with reasons. Everything else ctags
//! finds must be in our def-set, or the test fails and the miss becomes
//! a shape rule plus a unit test. Extras WE find (e.g. `with open`) are
//! reported but do not fail: recall is the gate, precision is advisory.
//!
//! Fixture files live in tests/fixtures/oracle/ (one per shape class).

use reliary_search::ingest::index_directory;
use reliary_search::lazy_occurrence::build_all_occurrence;
use reliary_search::schema::create_new_db;
use std::collections::{HashMap, HashSet};

/// (ctags name substring, reason) pairs we deliberately do not index.
/// Substring match because ctags spells some names oddly (`operator==`).
/// When a gap gets implemented, delete its entry here: the recall gate
/// below then enforces the new behavior automatically.
const KNOWN_GAPS: &[(&str, &str)] = &[
    // Function pointers are declarations of variables, not definitions.
    ("fp_handler", "function pointer is not a definition"),
    ("signal_cb", "function pointer is not a definition"),
    // Symbolic operator names have no identifier to bind.
    ("operator", "operator overload has no identifier"),
    // Bare `#if EXPR` needs an expression parser to separate names from
    // operators; only `defined()` forms are indexed.
    ("BARE_IF", "bare #if expression stays water"),
    // Body-less prototypes (`int use_proto(void);`). Queued, not indexed:
    // admitting them needs a single-word+semicolon rule that must exclude
    // `let x = foo();` — real work, filed as follow-up, not fitted here.
    ("use_proto", "body-less prototype queued as follow-up"),
    ("extra_fn", "body-less prototype queued as follow-up"),
];

fn have_ctags() -> bool {
    std::process::Command::new("ctags")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn ctags_defs(path: &std::path::Path) -> HashMap<String, i64> {
    let out = std::process::Command::new("ctags")
        // `+K` requests long kind names. Mapping kinds by their first letter
        // (`field` -> `f`, `member` -> `m`) silently counted struct fields as
        // functions, because `f` is also `function`'s initial.
        .args(["-o", "-", "--excmd=number", "--fields=+nK"])
        .arg(path)
        .output()
        .expect("run ctags");
    assert!(out.status.success(), "ctags failed on {}", path.display());
    let text = String::from_utf8_lossy(&out.stdout);
    let mut defs = HashMap::new();
    for line in text.lines() {
        // name<TAB>file<TAB>line;"<TAB>kind...
        // Only definition kinds: the oracle is a def-set comparison, and
        // variable/constant kinds (`const x = 42`) are local bindings we
        // deliberately do not define (V66c). Kind letters: f function,
        // c class, s struct, m member/method, d define/macro, p prototype,
        // t typedef, g enum, u union, n namespace.
        let mut parts = line.split('\t');
        let name = match parts.next() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        let _file = parts.next();
        let addr = parts.next().unwrap_or("");
        let kind = parts.next().unwrap_or("");
        if !is_definition_kind(kind) {
            continue;
        }
        // address looks like `42;"` — strip the `;"` trailer.
        let num: String = addr.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num.parse::<i64>() {
            defs.insert(name, n);
        }
    }
    defs
}

/// Which ctags kinds are definitions for this comparison.
///
/// Matched on the long kind word, never on its first letter. `member` is
/// kept because ctags reports Python methods as members; `field` is dropped
/// because a struct field is a slot, not a definition, and V86/V87 spent real
/// effort removing exactly that false positive from the index.
fn is_definition_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function"
            | "method"
            | "member"
            | "class"
            | "struct"
            | "enumerator"
            | "enum"
            | "union"
            | "interface"
            | "implementation"
            | "namespace"
            | "typedef"
            | "define"
            | "macro"
            | "prototype"
    )
}

#[test]
fn ctags_kind_mapping_uses_the_word_not_its_first_letter() {
    // Regression: `field` collapsed to `f` and matched `function`, so every
    // struct field entered the oracle as a definition we had "missed".
    assert!(!is_definition_kind("field"));
    assert!(!is_definition_kind("constant"));
    assert!(!is_definition_kind("variable"));
    assert!(!is_definition_kind("getter"));
    assert!(!is_definition_kind("setter"));
    assert!(is_definition_kind("function"));
    assert!(is_definition_kind("method"));
    assert!(is_definition_kind("member")); // Python methods
    assert!(is_definition_kind("struct"));
}

fn our_defs(dir: &std::path::Path, file: &str) -> HashSet<(String, i64)> {
    let db = rusqlite::Connection::open(dir.join("idx.sqlite")).unwrap();
    create_new_db(&db).unwrap();
    // Index the non-hidden corpus subdir (see caller).
    index_directory(&db, dir.join("corpus").to_str().unwrap()).unwrap();
    // Build the lazy occurrence table (fresh trust leaves it empty).
    let _ = build_all_occurrence(&db);
    // Collect is_def rows for this file.
    let mut out = HashSet::new();
    let mut stmt = db
        .prepare(
             "SELECT o.line, p.phrase FROM occurrence o
             JOIN phrases p ON p.id = o.phrase_id
             JOIN file_map f ON f.id = o.file_id
             WHERE o.is_def = 1 AND f.file_path LIKE '%' || ?1",
        )
        .unwrap();
    let rows = stmt
        .query_map([file], |r| {
            let line: i64 = r.get(0)?;
            let phrase: String = r.get(1)?;
            Ok((phrase, line))
        })
        .unwrap();
    for r in rows.flatten() {
        out.insert(r);
    }
    out
}

#[test]
fn ctags_oracle_recall() {
    if !have_ctags() {
        eprintln!("SKIP: ctags binary absent (CI installs universal-ctags)");
        return;
    }
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_dir = manifest.join("tests/fixtures/oracle");
    if !fixture_dir.exists() {
        eprintln!("SKIP: no oracle fixtures yet");
        return;
    }
    let mut misses: Vec<String> = Vec::new();
    let mut oracle_dump: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&fixture_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let fname = path.file_name().unwrap().to_string_lossy().into_owned();
        let work = tempfile::tempdir().unwrap();
        // The walker skips dot-prefixed directories and tempfile names its
        // dir `.tmpXXXX`, so the corpus must live in a non-hidden subdir
        // (same trap as symptom_checks documents).
        let corpus = work.path().join("corpus");
        std::fs::create_dir_all(&corpus).unwrap();
        std::fs::copy(&path, corpus.join(&fname)).unwrap();
        let the_copy = corpus.join(&fname);
        std::fs::copy(&path, &the_copy).unwrap();
        let ours = our_defs(work.path(), &fname);
        let theirs = ctags_defs(&the_copy);
        let our_names: HashSet<&str> = ours.iter().map(|(n, _)| n.as_str()).collect();
        // Full ctags def-set for adjudication output on failure.
        let mut all_theirs: Vec<String> = theirs.iter().map(|(n, l)| format!("{n}:{l}")).collect();
        all_theirs.sort();
        oracle_dump.push(format!("{fname}: {}", all_theirs.join(" ")));
        for (name, line) in &theirs {
            if KNOWN_GAPS.iter().any(|(g, _)| name.contains(g)) {
                continue;
            }
            // Our phrases are stemmed/lowered (`annotated`→`annot`,
            // `unioned`→`union`, CamelCase preserved-lower). Compare on
            // stemmed forms via the same stemmer the index uses.
            let stemmed = reliary_search::stem_identifier(name).to_ascii_lowercase();
            let low = name.to_ascii_lowercase();
            let hit = our_names.contains(name.as_str())
                || our_names.contains(low.as_str())
                || our_names.contains(stemmed.as_str())
                || ours.iter().any(|(n, l)| {
                    (*l == *line || (*l - *line).abs() <= 1) && n.to_ascii_lowercase() == low
                });
            if !hit {
                misses.push(format!("{fname}:{line} ctags def `{name}` missing from our def-set"));
            }
        }
    }
    assert!(
        misses.is_empty(),
        "oracle recall gaps (adjudicate: source truth wins, then shape-rule + unit test):\n{}\nctags said:\n{}",
        misses.join("\n"),
        oracle_dump.join("\n")
    );
}
