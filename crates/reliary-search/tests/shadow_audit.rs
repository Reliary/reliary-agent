//! Step 4 shadow audit: island classifier vs classify_structural,
//! side-by-side on the fixture corpus. READ-ONLY measurement: the
//! project learns one island set from this crate's Rust source (the
//! trust-time path in miniature), then both classifiers run per file.
//! Tallies def-agreement and the two disagreement directions.
//! No asserts on which is right — adjudication is manual (step 4 log).

use reliary_search::brace_graph::build_brace_graph;
use reliary_search::islands::{
    classify_island_line_raw, generalize, header_pattern, select_islands, Cat,
};
use reliary_search::segment::{brace_events, segment_lines};
use reliary_search::structural::{classify_structural, tokenizable_code};
use std::collections::{BTreeMap, HashSet};

fn learn_islands() -> Vec<reliary_search::islands::Island> {
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut entries: Vec<_> = std::fs::read_dir(&src_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "rs").unwrap_or(false))
        .collect();
    entries.sort();
    let mut counts: BTreeMap<Vec<Cat>, usize> = BTreeMap::new();
    for path in entries {
        let text = std::fs::read_to_string(&path).unwrap();
        for l in text.lines() {
            let g = generalize(&header_pattern(&tokenizable_code(l)));
            if !g.is_empty() {
                *counts.entry(g).or_insert(0) += 1;
            }
        }
    }
    let mut ranked: Vec<(Vec<Cat>, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let distinct = ranked.len();
    select_islands(&ranked, distinct)
}

/// Track brace depth crudely for the tag rule (depth>0 -> method).
fn depths(lines: &[String]) -> Vec<i32> {
    let mut out = Vec::with_capacity(lines.len());
    let mut d = 0i32;
    for l in lines {
        out.push(d);
        let t = tokenizable_code(l);
        d += (t.bytes().filter(|&b| b == b'{').count() as i32)
            - (t.bytes().filter(|&b| b == b'}').count() as i32);
        if d < 0 {
            d = 0;
        }
    }
    out
}

#[test]
fn shadow_audit() {
    let islands = learn_islands();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    names.sort();
    let mut out = String::new();
    let mut agree = 0usize;
    let mut only_island = 0usize;
    let mut only_struct = 0usize;
    let mut both_none = 0usize;
    for path in names {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        let graph = build_brace_graph(&lines);
        let events = brace_events(&graph);
        let segs = segment_lines(&lines, &events);
        let starts: HashSet<usize> = segs.iter().map(|s| s.start_line).collect();
        let ds = depths(&lines);
        let mut backward: Vec<HashSet<String>> = vec![HashSet::new()];
        out.push_str(&format!("FILE {}\n", name));
        for (idx, line) in lines.iter().enumerate() {
            let ln = idx + 1;
            let masked = tokenizable_code(line);
            let s = classify_structural(line, ds[idx], false, false);
            let s_def = s.is_def.then(|| s.defined_name.unwrap_or("?").to_string());
            let i_def = classify_island_line_raw(
                line,
                &masked,
                starts.contains(&ln),
                ds[idx],
                &backward,
                &islands,
                None,
            );
            // Feed ALL words (any length) into the backward vocabulary:
            // reuse detection must see every introduction. (An earlier
            // revision filtered len<2 and blinded novelty to x/T/i.)
            for w in masked.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                if !w.is_empty() && !w.bytes().next().unwrap().is_ascii_digit() {
                    backward[0].insert(w.to_string());
                }
            }
            match (&s_def, &i_def) {
                (Some(a), Some((b, _))) if a == b => agree += 1,
                (None, None) => both_none += 1,
                _ => {
                    if i_def.is_some() {
                        only_island += 1;
                    } else {
                        only_struct += 1;
                    }
                    out.push_str(&format!(
                        "  L{:>2} struct={:?} island={:?} :: {}\n",
                        ln, s_def, i_def, line.trim()
                    ));
                }
            }
        }
    }
    out.push_str(&format!(
        "AGREE {} BOTH_NONE {} ONLY_ISLAND {} ONLY_STRUCT {}\n",
        agree, both_none, only_island, only_struct
    ));
    std::fs::write("/tmp/shadow_audit.txt", &out).unwrap();
}
