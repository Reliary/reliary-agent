//! Shared test helpers for the ctags-based oracle tests.
//!
//! Only `tests/*.rs` are compiled as test targets; a subdirectory module like
//! this one is not, so both `ctags_oracle.rs` and `segment_spike.rs` can
//! `mod common;` without producing a third empty test binary.
//!
//! `mod common;` compiles a **separate copy** of this file into each test
//! binary, and each binary uses a different subset: `ctags_oracle` uses
//! `ctags_defs`, `segment_spike` uses `ctags_entries`. Since the module is
//! private to each binary, the unused half trips `dead_code` under
//! `-D warnings`. Allowing it here is the honest fix; the alternative is
//! splitting the file, which would recreate the duplication this exists to
//! remove.
#![allow(dead_code)]
//!
//! Why this exists: two test files independently parsed ctags output and both
//! took the first letter of the kind field. With short kinds (`--fields=+n`),
//! `field` becomes `f`, which is also `function`'s initial, so every struct
//! field entered the oracle as a definition we had "missed". CI caught it; a
//! dev host without ctags skips the test and never sees it. One parser, one
//! place to get it right.

use std::collections::HashMap;

/// Kinds that count as definitions for the oracle comparison.
///
/// Matched on the long kind word, never on its first letter. `member` is kept
/// because ctags reports Python methods as members. `field` and `constant` are
/// dropped: a struct field is a slot, not a definition, and V86/V87 spent real
/// effort removing that same false positive from the index.
pub fn is_definition_kind(kind: &str) -> bool {
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

/// ctags' language name for a fixture extension.
///
/// Needed because auto-detection returns **nothing** for `macros.c` in the
/// Ubuntu universal-ctags build, so the C fixture contributed zero oracle
/// lines and its assertions were vacuous. Forcing the language by extension
/// makes the fixture visible. Extension is used only to select a parser for
/// the *oracle*, never to decide what our indexer does.
fn language_for(path: &std::path::Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("c") | Some("h") => Some("C"),
        Some("py") => Some("Python"),
        Some("js") | Some("mjs") => Some("JavaScript"),
        Some("ts") => Some("TypeScript"),
        Some("rs") => Some("Rust"),
        Some("hpp") | Some("cpp") | Some("cc") => Some("C++"),
        _ => None,
    }
}

/// `(name, line, kind)` triples from ctags, definitions only.
pub fn ctags_entries(path: &std::path::Path) -> Vec<(String, i64, String)> {
    let mut cmd = std::process::Command::new("ctags");
    // `+K` requests long kind names. Without it ctags emits short kinds and
    // `field`/`function` both start with `f`.
    cmd.args(["-o", "-", "--excmd=number", "--fields=+nK"]);
    if let Some(lang) = language_for(path) {
        cmd.arg(format!("--language-force={lang}"));
    }
    let out = cmd
        .arg(path)
        .output()
        .expect("run ctags");
    assert!(out.status.success(), "ctags failed on {}", path.display());
    let text = String::from_utf8_lossy(&out.stdout);
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        let name = match parts.next() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        let _file = parts.next();
        let addr = parts.next().unwrap_or("");
        let kind = parts.next().unwrap_or("").to_string();
        if !is_definition_kind(&kind) {
            continue;
        }
        let num: String = addr.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num.parse::<i64>() {
            rows.push((name, n, kind));
        }
    }
    rows
}

/// `name -> line` for definition kinds. Later entries win, matching ctags
/// ordering (a struct and its `impl` share a name; the `impl` is later).
pub fn ctags_defs(path: &std::path::Path) -> HashMap<String, i64> {
    ctags_entries(path)
        .into_iter()
        .map(|(name, line, _kind)| (name, line))
        .collect()
}

pub fn have_ctags() -> bool {
    std::process::Command::new("ctags")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn kind_mapping_uses_the_word_not_its_first_letter() {
    assert!(!is_definition_kind("field"));
    assert!(!is_definition_kind("constant"));
    assert!(!is_definition_kind("variable"));
    assert!(!is_definition_kind("getter"));
    assert!(!is_definition_kind("setter"));
    assert!(is_definition_kind("function"));
    assert!(is_definition_kind("method"));
    assert!(is_definition_kind("member"));
    assert!(is_definition_kind("struct"));
}
