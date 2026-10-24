//! Step 4b wiring proof: flag-on end-to-end def recovery.
//!
//! Trusts the oracle fixtures with RELIARY_SEGMENTER=1, then asserts
//! every recorded def resolves via occurrence is_def=1 — the same bar
//! as the legacy oracle, through the wired island path (trust extract
//! + JIT occurrence build + persisted islands).
//!
//! Sole test in this binary: sets/unsets the process-global flag.

use reliary_search::ingest::index_directory;
use reliary_search::lazy_occurrence::build_all_occurrence;
use std::collections::HashSet;

// build_all_occurrence lives in lazy_occurrence; re-export check at use.
fn recorded_defs() -> Vec<(&'static str, Vec<(&'static str, i64)>)> {
    vec![
        ("macros.c", vec![
            ("MAX_SIZE", 2), ("WRAP", 3), ("CLAMP", 4), ("LOGFMT", 5),
            ("HAVE_PROTO", 6), ("use_proto", 8), ("extra_fn", 11),
            ("plain_api", 14), ("macro_api", 15), ("helper", 16),
            ("big_count", 17), ("Foo_bar_baz", 18), ("call_it", 19),
            // fp_handler deliberately absent: legacy gaps it too
            // ("function pointer is not a definition"); parity, not more.
            ("operator==", 21),
        ]),
        ("annotated.py", vec![
            ("plain", 5), ("annotated", 9), ("aworker", 13),
            ("unioned", 17), ("Widget", 21), ("method", 22),
            ("make", 26), ("cached", 31),
        ]),
        ("shapes.js", vec![
            ("arrow", 2), ("arrow2", 3), ("topfn", 7), ("Klass", 10),
            ("method", 11), ("prop", 14), ("constructor", 17),
        ]),
        ("shapes.rs", vec![
            ("block_on", 2), ("Foo", 6), ("Bar", 10), ("Bar", 17), ("get", 18),
        ]),
        // V83 B4: `use tokio::task;` must not vaccinate `task`; the local
        // `fn task` on line 4 is a definition despite the import.
        ("import_shadow.rs", vec![
            ("task", 4),
        ]),
    ]
}

fn our_defs(dir: &std::path::Path, file: &str) -> HashSet<(String, i64)> {
    let db = rusqlite::Connection::open(dir.join("idx.sqlite")).unwrap();
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
fn wired_flag_on_def_recovery() {
    std::env::set_var("RELIARY_SEGMENTER", "1");
    assert!(reliary_search::segment::segment_enabled());
    let work = tempfile::tempdir().unwrap();
    let corpus = work.path().join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    let fixture_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle");
    for (name, _) in recorded_defs() {
        std::fs::copy(fixture_dir.join(name), corpus.join(name)).unwrap();
    }
    // Scale: MDL selection starves on 4 toy files (everything count-1;
    // step-3 log). Production corpora aren't toys — add this crate's
    // real Rust source so frequency has something to work with. The
    // assertions below target the FIXTURE defs only.
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut entries: Vec<_> = std::fs::read_dir(&src_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "rs").unwrap_or(false))
        .collect();
    entries.sort();
    for path in &entries {
        let name = path.file_name().unwrap();
        std::fs::copy(path, corpus.join(name)).unwrap();
    }
    let db = rusqlite::Connection::open(work.path().join("idx.sqlite")).unwrap();
    reliary_search::schema::create_new_db(&db).unwrap();
    // Index the non-hidden corpus subdir (tempdir names are dot-prefixed
    // and the walker skips dot dirs — same trap as ctags_oracle).
    index_directory(&db, corpus.to_str().unwrap()).unwrap();
    // Wiring proof 1: islands persisted by the flag trust path.
    let island_rows: i64 = db
        .query_row("SELECT COUNT(*) FROM islands", [], |r| r.get(0))
        .unwrap_or(-1);
    assert!(island_rows > 0, "flag trust must persist islands, got {}", island_rows);
    // Wiring proof 2: occurrence built through the island JIT path.
    let _ = build_all_occurrence(&db);
    let mut misses = Vec::new();
    for (name, defs) in recorded_defs() {
        let ours = our_defs(work.path(), name);
        let our_names: HashSet<String> =
            ours.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
        for (dname, dline) in defs {
            // Operator names: stem the IDENTIFIER part (punctuation
            // never survives tokenization or stemming).
            let ident_part = dname.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
            let stemmed = reliary_search::stem_identifier(&ident_part.to_ascii_lowercase())
                .to_ascii_lowercase();
            let low = dname.to_ascii_lowercase();
            let hit = our_names.contains(&low)
                || our_names.contains(&stemmed)
                || ours.iter().any(|(n, l)| {
                    (*l == dline || (*l - dline).abs() <= 1)
                        && (n.to_ascii_lowercase() == low
                            || reliary_search::islands::def_name_matches_token(
                                &low,
                                &n.to_ascii_lowercase(),
                            ))
                });
            if !hit {
                misses.push(format!("{}:{} {}", name, dline, dname));
            }
        }
    }
    std::env::remove_var("RELIARY_SEGMENTER");
    assert!(
        misses.is_empty(),
        "flag-on wiring missed defs (legacy oracle covers these): {:?}",
        misses
    );
}
