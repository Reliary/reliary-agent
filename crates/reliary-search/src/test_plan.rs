//! V70 P3: `reliary test-plan` — which tests exercise the changed code.
//!
//! Grammar-free mapping from changed files/symbols to test files:
//!   1. callers of changed symbols that live in test paths
//!   2. mirror paths (src/foo.rs -> tests/foo.rs, src/foo/ -> tests/foo/)
//!   3. vocabulary overlap (rare identifiers shared between change and test)
//!
//! Deterministic: sorted output, same index => same plan.

use crate::callgraph_v2::build_call_graph_ext;
use crate::impact::is_test_path;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct TestPlan {
    /// Ordered test files with the reasons they matched.
    pub tests: Vec<(String, Vec<String>)>,
    /// Suggested commands (deduped, sorted).
    pub commands: Vec<String>,
    /// Test functions that live inside the changed files themselves. Rust,
    /// Python and JS put tests in the same file as the code, so a path-only
    /// plan reports nothing for exactly the files people ask about.
    pub inline: Vec<InlineTest>,
    /// Set when an inline scan was requested but the file's metadata could not
    /// be read. Empty `inline` is then "not checked", not "no tests".
    pub inline_skipped: Vec<String>,
}

/// Test-function markers, as prefixes of a trimmed line.
///
/// These are *attribute* shapes, not language grammars: every entry is a
/// bracketed or at-sign annotation whose presence on the preceding line marks
/// the next definition as a test. That is the same class as `#[cfg(test)]` and
/// it generalises across Rust, C#, Java, Kotlin and Scala without a parser.
const TEST_ATTRIBUTES: &[&str] = &[
    "#[test]", "#[tokio::test", "#[async_std::test", "#[test_case",
    "[Fact]", "[Theory]", "[Test]", "[TestMethod]", "[TestCase",
    "@Test", "@ParameterizedTest",
];

/// Does this trimmed line carry a test annotation?
pub fn is_test_attribute(t: &str) -> bool {
    TEST_ATTRIBUTES.iter().any(|a| t.starts_with(a))
}

/// Python and Ruby have no attributes: the marker is the function name itself.
/// `def test_x(` and `def test_x:` are the convention in both, and it is a
/// name-shape rule, not a keyword list.
fn is_python_style_test_fn(t: &str, name: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    if !(lower.starts_with("def ") || lower.starts_with("async def ")) {
        return false;
    }
    name.starts_with("test_") || name == "test" || name.ends_with("_test")
}

/// Inline tests, by enclosing function rather than by file path.
///
/// Rust, Python and JS put tests *inside* the same file as the code. The old
/// plan only looked at file paths, so a file with 83 `#[test]` functions
/// reported "no tests found" — 494 inline tests across this repository were
/// invisible.
///
/// The unit reported is the test function and its line, which is what a person
/// actually wants to run, not the file that contains it.
fn inline_tests(meta: &crate::file_meta::FileMeta, rel: &str) -> Vec<InlineTest> {
    let lines = &meta.lines;
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        // A definition may share its line with a body opener and even with
        // code: `fn test_x() {    let home = ...`. Classify the declaration
        // slice, not the whole line, or the trailing code changes the shape
        // and the definition is missed.
        let decl = match t.find('{') {
            Some(b) if t[..b].contains('(') => &t[..b],
            _ => t,
        };
        let def = crate::structural::classify_structural(decl.trim_end(), 0, false, false);
        if !def.is_def {
            continue;
        }
        let Some(name) = def.defined_name else { continue };
        // A test annotation sits on the line(s) before the definition, and
        // definitions routinely carry MORE than one attribute:
        //     #[test]
        //     #[allow(unused_imports)]
        //     fn test_atomic_write_creates_file() {
        // So walk back over the whole contiguous attribute run rather than
        // looking at one line. An attribute is bracket-shaped (`#[...]`) or
        // at-sign-shaped (`@Test`); anything else ends the run.
        let mut marked = false;
        for line in lines[..i].iter().rev() {
            let t2 = line.trim();
            if t2.is_empty() {
                continue;
            }
            // A doc comment is itself an attribute (`#[doc]`), and real code
            // interleaves them:
            //     #[test]
            //     /// V87: skip comments.
            //     #[test]
            //     fn trait_impl_scan_skips_comments() {
            // Treating `///` as an end-of-run made that test invisible.
            // Three attribute shapes: Rust `#[...]`, C#/Java `[Fact]`/`[Test]`,
            // and Java/Kotlin/Python `@Test`. Doc comments are attributes too.
            let is_attr = (t2.starts_with('#') && t2.ends_with(']'))
                || (t2.starts_with('[') && t2.ends_with(']'))
                || t2.starts_with("///") || t2.starts_with("//!")
                || (t2.starts_with('@') && !t2.contains(char::is_whitespace));
            if !is_attr {
                break;
            }
            if is_test_attribute(t2) {
                marked = true;
            }
        }
        let marked = marked || is_python_style_test_fn(t, name);
        if !marked {
            continue;
        }
        out.push(InlineTest {
            file: rel.to_string(),
            line: (i + 1) as i32,
            name: name.to_string(),
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InlineTest {
    pub file: String,
    /// 1-indexed display line.
    pub line: i32,
    pub name: String,
}

/// Map a changed file to candidate test files using mirror-path conventions.
fn mirror_candidates(changed: &str) -> Vec<String> {
    let mut out = Vec::new();
    let path = changed.replace('\\', "/");
    let base = path.rsplit('/').next().unwrap_or(&path);
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    let ext = base.rsplit_once('.').map(|(_, e)| e).unwrap_or("");

    // Normalize for segment search: prefix "/" so "src/..." matches "/src/".
    let anchored = if path.starts_with('/') { path.clone() } else { format!("/{}", path) };

    // src/foo.rs -> tests/foo.rs ; src/foo/bar.rs -> tests/bar.rs
    if let Some(idx) = anchored.find("/src/") {
        let after = &anchored[idx + 5..];
        let tail_base = after.rsplit('/').next().unwrap_or(after);
        out.push(format!("tests/{}", tail_base));
        out.push(format!("test/{}", tail_base));
        if !stem.is_empty() {
            out.push(format!("tests/{}_test.{}", stem, ext));
            out.push(format!("tests/test_{}.{}", stem, ext));
            out.push(format!("tests/{}.test.{}", stem, ext));
        }
    }
    // crates/*/src/foo.rs -> crates/*/tests/foo.rs
    if let Some(idx) = anchored.find("/crates/") {
        let rest = &anchored[idx + 1..];
        if let Some((head, _)) = rest.split_once("/src/") {
            out.push(format!("{}/tests/{}", head, base));
            if !stem.is_empty() {
                out.push(format!("{}/tests/{}.rs", head, stem));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Extract rare identifiers (length >= 5) from a file's changed symbols for
/// vocabulary-overlap matching. Grammar-free token scan.
#[allow(dead_code)]
fn rare_identifiers(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            if cur.len() >= 5 && cur.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
                out.insert(cur.clone());
            }
            cur.clear();
        }
    }
    if cur.len() >= 5 && cur.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
        out.insert(cur);
    }
    out
}

/// Compute the test plan for changed files (and optional changed symbols).
pub fn compute_plan(
    db: &Connection,
    changed_files: &[String],
    changed_symbols: &[String],
    path: &str,
) -> rusqlite::Result<TestPlan> {
    let mut matches: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    // All test files in the index (for mirror/vocabulary matching).
    let mut test_files: Vec<String> = Vec::new();
    {
        let mut stmt = db.prepare_cached(
            "SELECT DISTINCT file_path FROM file_map WHERE is_source = 1 ORDER BY file_path",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let fp: String = r.get(0)?;
            if is_test_path(&fp) {
                test_files.push(fp);
            }
        }
    }

    // 0. Inline tests in the changed files themselves.
    let mut inline: Vec<InlineTest> = Vec::new();
    let mut inline_skipped: Vec<String> = Vec::new();
    for changed in changed_files {
        // The index stores absolute paths; the CLI may pass either.
        let abs = if std::path::Path::new(changed).is_absolute() {
            changed.clone()
        } else {
            format!("{}/{}", path.trim_end_matches('/'), changed)
        };
        match crate::file_meta::get(&abs) {
            Some(meta) => inline.extend(inline_tests(&meta, changed)),
            None => inline_skipped.push(changed.clone()),
        }
    }
    inline.sort();
    inline.dedup();
    for t in &inline {
        matches
            .entry(t.file.clone())
            .or_default()
            .insert(format!("inline test {}", t.name));
    }

    // 1. Callers of changed symbols that are tests.
    for sym in changed_symbols {
        if let Ok(cg) = build_call_graph_ext(db, sym, path, None, 1, true) {
            for c in &cg.callers {
                if is_test_path(&c.file) {
                    matches
                        .entry(c.file.clone())
                        .or_default()
                        .insert(format!("calls {}", sym));
                }
            }
        }
    }

    // 2. Mirror paths: match by basename, but require the test path's tail
    //    to align with the candidate (V74): basename alone matched same-named
    //    files in ANY crate and the changed file itself.
    for changed in changed_files {
        let base = changed.rsplit('/').next().unwrap_or(changed);
        for cand in mirror_candidates(changed) {
            let cand_base = cand.rsplit('/').next().unwrap_or(&cand);
            let cand_tail: Vec<&str> = cand.split('/').filter(|x| !x.is_empty()).collect();
            for tf in &test_files {
                if tf == changed { continue; }
                let tf_base = tf.rsplit('/').next().unwrap_or(tf);
                let tf_tail: Vec<&str> = tf.split('/').filter(|x| !x.is_empty()).collect();
                // Match when the filenames agree AND at least the last two
                // path segments overlap (or both are single-segment names).
                let name_match = tf_base == cand_base || tf_base == base;
                let tail_match = if cand_tail.len() >= 2 && tf_tail.len() >= 2 {
                    cand_tail[cand_tail.len() - 2..] == tf_tail[tf_tail.len() - 2..]
                } else {
                    true
                };
                if name_match && tail_match {
                    matches
                        .entry(tf.clone())
                        .or_default()
                        .insert(format!("mirrors {}", base));
                }
            }
        }
        // 2b. Files in a tests/ dir whose path shares the changed file's
        //     parent module name (e.g. .../symbol.rs -> .../tests/...symbol...)
        let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
        if stem.len() >= 5 {
            for tf in &test_files {
                let tf_base = tf.rsplit('/').next().unwrap_or(tf);
                if tf_base.contains(stem) && tf_base != base {
                    matches
                        .entry(tf.clone())
                        .or_default()
                        .insert(format!("name overlaps {}", stem));
                }
            }
        }
    }

    // 3. Vocabulary overlap: rare identifiers from changed symbols matched
    //    against test file paths is weak — instead check that the test file
    //    references any changed symbol via the occurrence table.
    for sym in changed_symbols {
        let stemmed = crate::stem_identifier(sym);
        if let Ok(Some(pid)) = crate::symbol::phrase_id_for(db, &stemmed) {
            let mut stmt = db.prepare_cached(
                "SELECT DISTINCT f.file_path FROM occurrence o
                 JOIN file_map f ON f.id = o.file_id
                 WHERE o.phrase_id = ?1",
            )?;
            let mut rows = stmt.query(rusqlite::params![pid])?;
            while let Some(r) = rows.next()? {
                let fp: String = r.get(0)?;
                if is_test_path(&fp) {
                    matches
                        .entry(fp)
                        .or_default()
                        .insert(format!("references {}", sym));
                }
            }
        }
    }

    // Order: by match count desc, then path asc (deterministic).
    let mut tests: Vec<(String, Vec<String>)> = matches
        .into_iter()
        .map(|(f, reasons)| (f, reasons.into_iter().collect()))
        .collect();
    tests.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));

    // Commands: language-agnostic — cargo if Cargo.toml in repo root, pytest if
    // any .py test, go test if any _test.go.
    let mut commands: BTreeSet<String> = BTreeSet::new();
    let _has_cargo = std::path::Path::new(&format!("{}/Cargo.toml", path.trim_end_matches('/'))).exists();
    // V74: choose the runner from THE FILE'S extension, not global flags.
    // The old code emitted `pytest x.js` for any non-Rust test when a single
    // .py existed anywhere, and `go test .//abs/path` for absolute paths.
    // Paths are made relative to the nearest project marker for runnability.
    let rel = |f: &str| -> String {
        for marker in ["/crates/", "/src/", "/tests/", "/test/"] {
            if let Some(idx) = f.find(marker) {
                return f[idx + 1..].to_string();
            }
        }
        // Absolute-looking path with no marker: use the last two segments.
        let segs: Vec<&str> = f.split('/').filter(|x| !x.is_empty()).collect();
        if segs.len() >= 2 {
            format!("{}/{}", segs[segs.len() - 2], segs[segs.len() - 1])
        } else {
            segs.last().copied().unwrap_or(f).to_string()
        }
    };
    for (f, _) in &tests {
        if f.ends_with(".rs") {
            // Try to infer crate from the path (crates/<name>/...).
            if let Some(idx) = f.find("/crates/") {
                let rest = &f[idx + 8..];
                let crate_name = rest.split('/').next().unwrap_or("");
                if !crate_name.is_empty() {
                    commands.insert(format!("cargo test -p {}", crate_name));
                }
            } else {
                commands.insert("cargo test".to_string());
            }
        } else if f.ends_with(".py") {
            commands.insert(format!("pytest {}", rel(f)));
        } else if f.ends_with("_test.go") {
            let dir = f.rsplit_once('/').map(|(d, _)| d).unwrap_or(f);
            commands.insert(format!("go test ./{}", rel(dir)));
        } else if f.ends_with(".js") || f.ends_with(".ts") || f.ends_with(".jsx") || f.ends_with(".tsx") {
            commands.insert(format!("npx jest {}", rel(f)));
        }
    }

    Ok(TestPlan {
        tests,
        commands: commands.into_iter().collect(),
        inline,
        inline_skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `compute_from_content` caches by path, so each fixture needs its own
    /// path or every test reads the first fixture's metadata (which is exactly
    /// what happened: all of them returned the last test's function).
    fn meta_of(lines: &[&str]) -> std::sync::Arc<crate::file_meta::FileMeta> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let body = lines.join("\n");
        let path = format!("/tmp/inline_probe_{}.rs", N.fetch_add(1, Ordering::Relaxed));
        crate::file_meta::compute_from_content(&path, &body).unwrap()
    }

    fn names(rows: Vec<InlineTest>) -> Vec<String> {
        rows.into_iter().map(|r| r.name).collect()
    }

    #[test]
    fn inline_rust_tests_are_found_with_their_lines() {
        let m = meta_of(&[
            "fn production() {}",
            "",
            "#[cfg(test)]",
            "mod tests {",
            "    #[test]",
            "    fn does_a_thing() {",
            "        assert!(true);",
            "    }",
            "",
            "    #[tokio::test]",
            "    async fn does_an_async_thing() {",
            "    }",
            "}",
        ]);
        let found = inline_tests(&m, "src/lib.rs");
        assert_eq!(names(found.clone()), vec!["does_a_thing", "does_an_async_thing"]);
        assert_eq!(found[0].line, 6);
        assert_eq!(found[1].line, 11);
        assert_eq!(found[0].file, "src/lib.rs");
    }

    /// Negative control for the attribute gate: without a test attribute, a
    /// function inside `mod tests` is NOT a test. Removing the attribute gate
    /// makes this test fail.
    #[test]
    fn production_functions_are_not_inline_tests() {
        let m = meta_of(&[
            "#[cfg(test)]",
            "mod tests {",
            "    fn helper_not_a_test() {}",
            "    #[test]",
            "    fn real_test() {}",
            "}",
        ]);
        assert_eq!(names(inline_tests(&m, "src/lib.rs")), vec!["real_test"]);
    }

    /// Stacked attributes: a test often carries `#[allow(..)]` or a doc
    /// comment between the marker and the definition. A single-line lookback
    /// misses these, which is how 4 tests were lost on the first attempt.
    #[test]
    fn stacked_attributes_do_not_hide_a_test() {
        let m = meta_of(&[
            "    #[test]",
            "    #[allow(unused_imports)]",
            "    fn with_a_trailing_attr() {}",
            "",
            "    #[test]",
            "    /// a doc comment between the marker and the fn",
            "    fn with_a_doc_comment() {}",
        ]);
        assert_eq!(
            names(inline_tests(&m, "src/lib.rs")),
            vec!["with_a_trailing_attr", "with_a_doc_comment"]
        );
    }

    /// A definition may share its line with code. The declaration slice is
    /// classified instead of the raw line, or the `{`-and-body changes the
    /// shape and the test disappears.
    #[test]
    fn one_line_test_body_is_still_found() {
        let m = meta_of(&[
            "#[test]",
            "fn terse() {    let x = 1; assert_eq!(x, 1);",
            "}",
        ]);
        assert_eq!(names(inline_tests(&m, "src/lib.rs")), vec!["terse"]);
    }

    /// Python and Ruby have no attributes: the convention is the name.
    #[test]
    fn python_style_test_names_are_found() {
        let m = meta_of(&[
            "def production():",
            "    pass",
            "",
            "def test_production():",
            "    assert True",
            "",
            "async def test_async_production():",
            "    pass",
            "",
            "class TestThing:",
            "    def test_method(self):",
            "        pass",
        ]);
        assert_eq!(
            names(inline_tests(&m, "tests/test_x.py")),
            vec!["test_production", "test_async_production", "test_method"]
        );
    }

    /// Attribute-shaped markers from other ecosystems.
    #[test]
    fn csharp_and_java_test_attributes_are_found() {
        let m = meta_of(&[
            "    [Fact]",
            "    public void AddsNumbers() {",
            "    }",
            "",
            "    [Test]",
            "    public void SubtractsNumbers() {",
            "    }",
            "",
            "    @Test",
            "    public void multipliesNumbers() {",
            "    }",
        ]);
        assert_eq!(
            names(inline_tests(&m, "src/CalcTests.cs")),
            vec!["AddsNumbers", "SubtractsNumbers", "multipliesNumbers"]
        );
    }

    #[test]
    fn mirror_candidates_src_layout() {
        let c = mirror_candidates("src/foo.rs");
        assert!(c.contains(&"tests/foo.rs".to_string()));
        assert!(c.contains(&"tests/foo_test.rs".to_string()));
    }

    #[test]
    fn mirror_candidates_crate_layout() {
        let c = mirror_candidates("crates/reliary-search/src/symbol.rs");
        assert!(c.iter().any(|x| x == "crates/reliary-search/tests/symbol.rs"));
    }

    #[test]
    fn rare_identifiers_filters_short() {
        let ids = rare_identifiers("let a = classify_structural(b, 42);");
        assert!(ids.contains("classify_structural"));
        assert!(!ids.contains("let"));
        assert!(!ids.contains("a"));
    }

    #[test]
    fn mirror_handles_no_extension() {
        let c = mirror_candidates("src/module");
        assert!(c.is_empty() || c.iter().all(|x| x.starts_with("test")));
    }
}
