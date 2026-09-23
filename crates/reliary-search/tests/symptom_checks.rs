//! Behavioural symptom checks for the code-intelligence layer.
//!
//! These assertions describe observable output of the public API — they do not
//! name the internal function or module that produces it. A failure here means
//! the tool reported a wrong location/visibility/classification, and the cause
//! must be localised in the source.

use reliary_search::ingest::index_directory;
use reliary_search::lazy_occurrence::ensure_occurrence_for_phrase;
use reliary_search::schema::create_new_db;
use reliary_search::symbol::phrase_id_for;
fn index_src(src: &str) -> (tempfile::TempDir, rusqlite::Connection) {
    let dir = tempfile::tempdir().expect("tempdir");
    // The walker skips dot-prefixed directories, and tempfile::tempdir()
    // names its directory `.tmpXXXX` — so the corpus must live in a
    // non-hidden subdirectory.
    let corpus = dir.path().join("fixture");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(corpus.join("fixture.rs"), src).unwrap();
    let db = rusqlite::Connection::open(dir.path().join("idx.sqlite")).unwrap();
    create_new_db(&db).unwrap();
    index_directory(&db, corpus.to_str().unwrap()).unwrap();
    (dir, db)
}

fn warm(db: &rusqlite::Connection, name: &str) {
    if let Ok(Some(pid)) = phrase_id_for(db, name) {
        let _ = ensure_occurrence_for_phrase(db, pid);
    }
}

#[test]
fn test_file_classification_covers_common_conventions() {
    // Every one of these paths is a test file by universal convention.
    for p in [
        "crates/foo/tests/integration.rs",
        "src/test_helpers.py",
        "app/foo.test.js",
        "pkg/foo_spec.rb",
        "src/specs/thing.rs",
        "tests.rs",
        "a/b/_test.go",
    ] {
        assert!(
            reliary_search::impact::is_test_path(p),
            "{p} must be classified as a test path"
        );
    }
    // ...and these are production.
    for p in ["src/main.rs", "src/latest.rs", "src/contest.rs"] {
        assert!(
            !reliary_search::impact::is_test_path(p),
            "{p} must NOT be classified as a test path"
        );
    }
}

#[test]
fn test_symbol_names_round_trip_through_the_index() {
    // A name that is defined must be findable by that exact name, whatever its
    // spelling convention — including type names that collide with common
    // keywords when lowercased (Default).
    for (name, src) in [
        ("snake_case_fn", "fn snake_case_fn() {}\n"),
        ("CamelThing", "struct CamelThing;\n"),
        ("plainword", "fn plainword() {}\n"),
        ("Default", "struct Default;\n"),
    ] {
        let (_dir, db) = index_src(src);
        warm(&db, name);
        let pid = phrase_id_for(&db, name)
            .expect("query ok")
            .unwrap_or_else(|| panic!("{name} must have a phrase row"));
        let n: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM occurrence WHERE phrase_id = ?1",
                rusqlite::params![pid],
                |r| r.get(0),
            )
            .unwrap();
        assert!(n > 0, "{name} must have occurrence rows (got {n})");
    }
}

#[test]
fn test_reported_method_lines_point_at_the_declaration() {
    let src = "// filler\nstruct Widget {\n    x: i32,\n}\n\nimpl Widget {\n    pub fn area(&self) -> i32 {\n        self.x\n    }\n\n    fn secret(&self) -> i32 {\n        0\n    }\n}\n";
    let (_dir, db) = index_src(src);
    warm(&db, "Widget");
    let mr = reliary_search::callgraph_v2::find_methods_on(&db, "Widget").expect("methods ok");

    let area = mr.methods.iter().find(|m| m.name == "area").expect("area");
    assert_eq!(area.line, 7, "area is declared on line 7, got {}", area.line);
    let secret = mr.methods.iter().find(|m| m.name == "secret").expect("secret");
    assert_eq!(secret.line, 11, "secret is declared on line 11, got {}", secret.line);
}

#[test]
fn test_reported_lines_are_one_indexed_into_the_source() {
    // The first struct field sits on source line 2.
    let src = "struct P {\n    a: i32,\n    b: i32,\n}\n";
    let (_dir, db) = index_src(src);
    warm(&db, "P");
    let mr = reliary_search::callgraph_v2::find_methods_on(&db, "P").expect("fields ok");
    let a = mr.methods.iter().find(|m| m.name == "a").expect("field a");
    assert_eq!(a.line, 2, "field a is on source line 2, got {}", a.line);
}

#[test]
fn test_visibility_reflects_all_public_forms() {
    let src = "struct W;\nimpl W {\n    pub fn a(&self) {}\n    pub(crate) fn b(&self) {}\n    pub(super) fn c(&self) {}\n    fn d(&self) {}\n}\n";
    let (_dir, db) = index_src(src);
    warm(&db, "W");
    let mr = reliary_search::callgraph_v2::find_methods_on(&db, "W").expect("methods ok");
    let vis = |n: &str| {
        mr.methods
            .iter()
            .find(|m| m.name == n)
            .unwrap_or_else(|| panic!("{n} missing"))
            .is_pub
    };
    assert!(vis("a"), "pub fn is public");
    assert!(vis("b"), "pub(crate) fn is public within the crate");
    assert!(vis("c"), "pub(super) fn is public to the parent");
    assert!(!vis("d"), "bare fn is private");
}

#[test]
fn test_adjacent_same_typed_fields_both_survive() {
    // Two fields that happen to share a type must both be listed.
    let src = "pub struct Point {\n    pub x: i32,\n    y: i32,\n}\n";
    let (_dir, db) = index_src(src);
    warm(&db, "Point");
    let mr = reliary_search::callgraph_v2::find_methods_on(&db, "Point").expect("fields ok");
    assert!(mr.methods.iter().any(|m| m.name == "x"), "field x present");
    assert!(mr.methods.iter().any(|m| m.name == "y"), "field y present");
}
