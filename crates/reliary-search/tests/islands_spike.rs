//! Islands step 2 measurement (SEGMENTER_SPIKE.md).
//! The CCM/naturalness claim, tested without labels: definition-line
//! shapes must rank among the frequent interior patterns of the corpus.
//! Source truth for def lines is the recorded table (mirror of
//! segment_spike.rs); the measurement is comparative, not absolute.

use reliary_search::islands::{
    classify_island_line, header_pattern, island_def, select_islands, Cat, Island,
};
use reliary_search::structural::tokenizable_code;
use std::collections::HashMap;

fn corpus_lines() -> Vec<(String, Vec<String>)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle");
    let mut out = Vec::new();
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    names.sort();
    for path in names {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        out.push((name, text.lines().map(|l| l.to_string()).collect()));
    }
    out
}

fn recorded_defs(name: &str) -> Vec<usize> {
    match name {
        "macros.c" => vec![2, 3, 4, 5, 6, 8, 11, 14, 15, 16, 17, 18, 19, 20, 21],
        "annotated.py" => vec![5, 9, 13, 17, 21, 22, 26, 31],
        "shapes.js" => vec![2, 3, 7, 10, 11, 14, 17],
        "shapes.rs" => vec![2, 6, 10, 17, 18],
        _ => Vec::new(),
    }
}

#[test]
fn def_shapes_are_frequent_shapes() {
    let corpus = corpus_lines();
    // Count header-span patterns over production-masked text: this is
    // the unsupervised ranking (no labels enter the counts).
    let all: Vec<String> = corpus
        .iter()
        .flat_map(|(_, lines)| {
            lines.iter().map(|l| reliary_search::structural::tokenizable_code(l))
        })
        .collect();
    let all_refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    let ranked = {
        let mut counts: std::collections::BTreeMap<Vec<Cat>, usize> = Default::default();
        for s in &all_refs {
            let pat = header_pattern(s);
            if pat.is_empty() {
                continue;
            }
            *counts.entry(pat).or_insert(0) += 1;
        }
        let mut out: Vec<(Vec<Cat>, usize)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    };
    let rank_of: HashMap<Vec<Cat>, usize> =
        ranked.iter().enumerate().map(|(i, (p, _))| (p.clone(), i + 1)).collect();
    let top_n = 12;

    let mut covered = 0usize;
    let mut total = 0usize;
    let mut distinct_def_pats = std::collections::HashSet::new();
    for (name, lines) in &corpus {
        for &ln in &recorded_defs(name) {
            let masked = reliary_search::structural::tokenizable_code(&lines[ln - 1]);
            let pat = header_pattern(&masked);
            distinct_def_pats.insert(pat.clone());
            total += 1;
            match rank_of.get(&pat) {
                Some(r) if *r <= top_n => covered += 1,
                other => eprintln!("{}:{} pattern {:?} rank {:?}", name, ln, pat, other),
            }
        }
    }
    eprintln!(
        "def-line patterns covered in top-{}: {}/{} ({} distinct def patterns)",
        top_n,
        covered,
        total,
        distinct_def_pats.len()
    );
    // Comparative bar: a majority of def lines must use shapes ranked in
    // the unsupervised top-N. If this fails, the CCM premise (def shapes
    // are the frequent shapes) does not hold on real code and step 3
    // must not proceed on frequency alone.
    assert!(
        covered * 2 >= total,
        "CCM premise FAILED: only {}/{} def lines use top-{} shapes",
        covered,
        total,
        top_n
    );
}

/// Step 3 cross-corpus measurement: learn islands on a real corpus
/// (this crate's own Rust source, ~100 files) and type the fixture def
/// lines with them. This is the architectural claim: islands learned
/// per-project at trust time generalize across shapes AND languages.
/// Fixture-only learning starves MDL (everything is count-1); the
/// kill criterion is measured here, not there.
#[test]
fn island_typing_cross_corpus() {
    use reliary_search::brace_graph::build_brace_graph;
    use reliary_search::islands::generalize;
    use reliary_search::segment::{brace_events, segment_lines};
    // Learn: all Rust source of this crate (production-masked).
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut all: Vec<String> = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&src_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "rs").unwrap_or(false))
        .collect();
    entries.sort();
    assert!(!entries.is_empty());
    for path in entries {
        let text = std::fs::read_to_string(&path).unwrap();
        for l in text.lines() {
            all.push(reliary_search::structural::tokenizable_code(l));
        }
    }
    let mut counts: std::collections::BTreeMap<Vec<Cat>, usize> = Default::default();
    for s in &all {
        let g = generalize(&header_pattern(s));
        if g.is_empty() {
            continue;
        }
        *counts.entry(g).or_insert(0) += 1;
    }
    let mut ranked: Vec<(Vec<Cat>, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let distinct = ranked.len();
    let islands = select_islands(&ranked, distinct);
    eprintln!("cross-corpus: {} lines -> {} islands", all.len(), islands.len());
    for isl in islands.iter().take(12) {
        eprintln!("  island {:?} x{}", isl.pattern, isl.count);
    }

    let corpus = corpus_lines();
    let rare: &[(&str, usize)] = &[
        ("macros.c", 2),
        ("macros.c", 21),
        ("annotated.py", 21),
        ("shapes.js", 2),
        ("shapes.rs", 17),
    ];
    let mut rare_hit = 0usize;
    let mut cov = 0usize;
    let mut cov_tot = 0usize;
    for (name, lines) in &corpus {
        let text_lines: Vec<String> = lines.clone();
        let graph = build_brace_graph(&text_lines);
        let events = brace_events(&graph);
        let segs = segment_lines(&text_lines, &events);
        let starts: std::collections::HashSet<usize> =
            segs.iter().map(|s| s.start_line).collect();
        for &ln in &recorded_defs(name) {
            let masked = reliary_search::structural::tokenizable_code(&lines[ln - 1]);
            let got = island_def(&masked, starts.contains(&ln), &islands);
            let is_rare = rare.contains(&(name.as_str(), ln));
            if got.is_some() {
                if is_rare {
                    rare_hit += 1;
                } else {
                    cov += 1;
                }
            } else {
                eprintln!("X MISS {}:{} masked=[{}]", name, ln, masked);
            }
            if !is_rare {
                cov_tot += 1;
            }
        }
    }
    eprintln!("X RARE {}/5 COV {}/{}", rare_hit, cov, cov_tot);
    assert!(
        rare_hit >= 4,
        "STEP 3 KILL: cross-corpus typing recovered {}/5 rare shapes",
        rare_hit
    );
    assert!(
        cov >= 27,
        "STEP 3 REGRESSION: cross-corpus covered {}/{} (floor 27)",
        cov,
        cov_tot
    );
}

#[test]
fn b1_terminator_rules_at_island_level() {
    // B1 verify-only (plan v2): terminator exclusion needs no new code —
    // the island path already types `;`-declarations and waters `=`-bindings.
    // This test pins both so future edits can't silently flip them.
    let islands = vec![
        Island { pattern: vec![Cat::Word, Cat::OpenParen], count: 99 },
        Island { pattern: vec![Cat::Word, Cat::Word, Cat::Semi], count: 5 },
        // Operator-def shape (selected on real corpora; cf. wired_oracle).
        Island {
            pattern: vec![Cat::Word, Cat::Other, Cat::Other, Cat::OpenParen],
            count: 9,
        },
    ];
    let back: Vec<std::collections::HashSet<String>> = vec![std::collections::HashSet::new()];
    // Prototype: declaration IS the site (ctags agrees).
    let m1 = tokenizable_code("int use_proto(void);");
    assert_eq!(
        classify_island_line(&m1, true, 0, &back, &islands, None),
        Some(("use_proto".to_string(), 1))
    );
    // Binding: top-level `=` waters even with a call-shaped RHS.
    let m2 = tokenizable_code("let x = foo();");
    assert_eq!(
        classify_island_line(&m2, true, 0, &back, &islands, None),
        None
    );
    // Comparison is not assignment: `==` must not water a def shape.
    let m3 = tokenizable_code("bool operator==(const int a);");
    assert!(classify_island_line(&m3, true, 0, &back, &islands, None).is_some());
}

#[test]
fn import_group_opener_detection() {
    // Blind-gate find: `::{` opens a re-export list, not a body.
    // `) {` (qualified defs) and word-`{` (enums) are bodies.
    use reliary_search::islands::opens_import_group;
    assert!(opens_import_group("use std::sync::{"));
    assert!(opens_import_group("use std::{Arc,"));
    assert!(!opens_import_group("int Foo::bar(int x) {"));
    assert!(!opens_import_group("pub enum Color {"));
    assert!(!opens_import_group("impl Foo {"));
    assert!(!opens_import_group("fn foo() {"));
}

#[test]
fn qualifier_group_skip() {
    // Blind-gate find (tokio): `pub(crate) struct BlockingPool {`
    // must name `BlockingPool`, not `pub` (the `(` of the visibility
    // qualifier is not the signature anchor). But `method() {` and
    // `if (x) {` must NOT skip (the group IS their header).
    use reliary_search::islands::name_before_anchor;
    assert_eq!(
        name_before_anchor("pub(crate) struct BlockingPool {"),
        Some("BlockingPool".to_string())
    );
    assert_eq!(
        name_before_anchor("pub(in crate::foo) struct Bar {"),
        Some("Bar".to_string())
    );
    assert_eq!(
        name_before_anchor("method() {"),
        Some("method".to_string())
    );
    assert_eq!(name_before_anchor("if (x) {"), Some("if".to_string()));
    assert_eq!(name_before_anchor("foo(x);"), Some("foo".to_string()));
}

#[test]
fn b4_receiver_never_introduces() {
    // B4 (plan v2): a name reached through `.`/`::` is a use, never an
    // introduction — it must not enter the backward vocabulary.
    // Negative control: remove the receiver and the same name introduces.
    use reliary_search::islands::is_receiver;
    // `x.get()` — `get` at offset 2, preceded by `.`.
    assert!(is_receiver("x.get()", 2));
    // `Foo::bar` — `bar` at offset 5, preceded by `:`.
    assert!(is_receiver("Foo::bar", 5));
    // `get()` at line start — no receiver, introduces.
    assert!(!is_receiver("get()", 0));
    // `  get()` indented — spaces walked back, line start reached.
    assert!(!is_receiver("  get()", 2));
    // `a -> get()` — `>` is not a receiver discriminator (the `->`
    // tail is covered by the `:`/`::` handling upstream; `>` alone
    // must not water, or `a > b` comparisons break vocabulary).
    assert!(!is_receiver("a -> get()", 5));
}

#[test]
fn impl_for_argument_never_introduces() {
    // Blind-gate find (tokio): `impl Drop for BlockingPool` names the
    // trait-impl ARGUMENT, not an introduction. The real defs are the
    // `pub struct BlockingPool` line and the `impl BlockingPool` line
    // (bare `impl Name`, no `for`). Negative controls: both type.
    use reliary_search::islands::name_before_anchor;
    assert_eq!(name_before_anchor("impl Drop for BlockingPool {"), None);
    assert_eq!(
        name_before_anchor("impl BlockingPool {"),
        Some("BlockingPool".to_string())
    );
    assert_eq!(
        name_before_anchor("pub struct BlockingPool {"),
        Some("BlockingPool".to_string())
    );
}
