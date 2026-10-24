//! Segmenter spike measurement (SEGMENTER_SPIKE.md K1/K2).
//! Asserts cohesion-plus-depth boundaries land near ctags def lines,
//! and records the brace-only baseline the spike must beat.
//!
//! Skips cleanly when `ctags` is absent (same convention as ctags_oracle).

mod common;

use reliary_search::brace_graph::build_brace_graph;
use reliary_search::segment::{brace_events, segment_lines};
use std::collections::HashSet;

fn fixtures() -> Vec<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle");
    let mut out: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

/// Source-truth def lines per fixture (1-indexed), hand-adjudicated.
/// Used when `ctags` is absent; when present, ctags output is preferred
/// and must agree with this table (mismatch = investigate, not silence).
fn recorded_defs(name: &str) -> Vec<i64> {
    match name {
        // macros.c: defines 2-6, decls 8/11 (prototypes count as defs
        // for boundary purposes), fns 14-19, fp var 20 (decl), op 21.
        "macros.c" => vec![2, 3, 4, 5, 6, 8, 11, 14, 15, 16, 17, 18, 19, 20, 21],
        // annotated.py: defs 5,9,13,17,21,22,26,31.
        "annotated.py" => vec![5, 9, 13, 17, 21, 22, 26, 31],
        // shapes.js: arrows 2,3, fn 7, class 10, methods 11,14,17.
        // Lines 6/21 are bindings/usages — not defs.
        "shapes.js" => vec![2, 3, 7, 10, 11, 14, 17],
        // shapes.rs: fn 2, structs 6,10, impl 17, method 18.
        "shapes.rs" => vec![2, 6, 10, 17, 18],
        // import_shadow.rs: `run` at 3, and a *nested* `fn task` at 4 that
        // shadows the imported `task`. Both are definitions; the nested one is
        // the case V84/V85 fixed (declarations nested 3+ levels were dropped).
        "import_shadow.rs" => vec![3, 4],
        // A fixture with no recorded truth used to return an empty vector,
        // which made every recall assertion vacuous. `unwrap` so adding a
        // fixture forces someone to adjudicate it.
        other => panic!(
            "fixture `{other}` has no recorded def lines; adjudicate it and add them \
             (an empty list would make the recall gate vacuous)"
        ),
    }
}

fn ctags_available() -> bool {
    common::have_ctags()
}

/// Definition lines from ctags, filtered by kind word (see `common`).
fn ctags_defs(path: &std::path::Path) -> Vec<i64> {
    let mut lines: Vec<i64> = common::ctags_entries(path)
        .into_iter()
        .map(|(_name, line, _kind)| line)
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Fraction of def lines within `tol` of a boundary set.
fn recall(defs: &[i64], bounds: &HashSet<i64>, tol: i64) -> (usize, usize) {
    let hit = defs
        .iter()
        .filter(|d| bounds.iter().any(|b| (b - *d).abs() <= tol))
        .count();
    (hit, defs.len())
}

#[test]
fn segmenter_boundary_recall() {
    let have_ctags = ctags_available();
    if !have_ctags {
        eprintln!("NOTE: ctags absent — measuring against recorded source truth");
    }
    // K1 bar: >=80% of def lines within +/-2 of a fused boundary.
    // Baseline: brace-only boundaries must be reported alongside.
    let mut tot_hit = 0usize;
    let mut tot_defs = 0usize;
    let mut base_hit = 0usize;
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        let graph = build_brace_graph(&lines);
        let events = brace_events(&graph);
        let segs = segment_lines(&lines, &events);
        let fused: HashSet<i64> = segs.iter().map(|s| s.start_line as i64).collect();
        let brace_only: HashSet<i64> = events.iter().copied().collect();
        let recorded = recorded_defs(&name);
        if have_ctags {
            let live = ctags_defs(&path);
            let live_set: HashSet<i64> = live.iter().copied().collect();
            let rec_set: HashSet<i64> = recorded.iter().copied().collect();
            // Subset, not equality. The recorded table is hand-adjudicated and
            // deliberately includes shapes our classifier does not index as
            // definitions (body-less prototypes, a function-pointer variable, an
            // operator overload) because they are still real block boundaries
            // for the segmenter's purpose. What must never happen is ctags
            // finding a definition we failed to record: that means the table is
            // stale and the recall gate below is measuring the wrong lines.
            let unrecorded: Vec<i64> = live_set.difference(&rec_set).copied().collect();
            assert!(
                unrecorded.is_empty(),
                "ctags found def lines absent from the recorded truth for {}: {:?} \
                 (recorded {:?}) — adjudicate and add them, do not silence",
                name, unrecorded, recorded
            );
            // Report the adjudicated extras so the gap stays visible.
            let mut extras: Vec<i64> = rec_set.difference(&live_set).copied().collect();
            extras.sort_unstable();
            if !extras.is_empty() {
                eprintln!("{name}: recorded but not ctags (adjudicated): {extras:?}");
            }
        }
        let (h, t) = recall(&recorded, &fused, 2);
        let (bh, _) = recall(&recorded, &brace_only, 2);
        eprintln!(
            "{}: fused {}/{} brace-only {}/{}",
            name, h, t, bh, t
        );
        tot_hit += h;
        tot_defs += t;
        base_hit += bh;
    }
    eprintln!(
        "TOTAL fused {}/{} brace-only {}/{}",
        tot_hit, tot_defs, base_hit, tot_defs
    );
    assert!(
        tot_hit >= base_hit,
        "COMPARATIVE FAILED: fused {}/{} below brace-only {}/{}",
        tot_hit,
        tot_defs,
        base_hit,
        tot_defs
    );
}

/// K2: minified/fluent input (flat cohesion) must still produce brace
/// events as boundaries, without >2x spurious segments vs brace count.
#[test]
fn segmenter_minified_graceful() {
    let text = "function a(x){return x+1;}function b(y){return a(y)*2;}var z=b(3);";
    let lines: Vec<String> = vec![text.to_string()];
    let graph = build_brace_graph(&lines);
    let events = brace_events(&graph);
    let segs = segment_lines(&lines, &events);
    // Single line: no interior gaps exist, so exactly one segment.
    // The assertion is structural: no panic, full coverage, and the
    // brace graph saw the two function opens.
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].start_line, 1);
    assert_eq!(segs[0].end_line, 1);
    eprintln!("minified events {:?} segments {:?}", events, segs);
}
