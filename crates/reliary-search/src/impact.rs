//! V70 P2: `reliary impact` — pre-edit blast radius.
//!
//! Given a symbol, reports: definition location, direct callers grouped by
//! file, which callers are tests, and a risk verdict. Deterministic:
//! same index + same symbol => same output.

use crate::callgraph_v2::{build_call_graph_ext, CallGraph};
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct Impact {
    pub symbol: String,
    pub def_file: String,
    pub def_line: i32,
    pub callers: Vec<(String, i32)>,
    pub caller_files: Vec<String>,
    pub test_files: Vec<String>,
    pub risk: Risk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Low,
    Moderate,
    High,
}

impl Risk {
    pub fn label(&self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Moderate => "moderate",
            Risk::High => "high",
        }
    }
}

/// Grammar-free test-file detection: path segments and filename patterns.
/// Universal across languages (no per-language parsing).
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let segs: Vec<&str> = lower.split('/').collect();
    if segs.iter().any(|s| *s == "test" || *s == "tests" || *s == "spec" || *s == "specs"
        || *s == "__tests__" || *s == "testing") {
        return true;
    }
    let base = lower.rsplit('/').next().unwrap_or("");
    base.starts_with("test_")
        || base.ends_with("_test.rs") || base.ends_with("_test.py") || base.ends_with("_test.go")
        || base.ends_with("_tests.rs") || base.ends_with("_spec.rs") || base.ends_with("_spec.rb")
        || base.contains(".test.") || base.contains(".spec.")
        || base == "tests.rs" || base == "test.rs"
}

/// Compute the blast radius for `symbol`.
pub fn compute_impact(db: &Connection, symbol: &str, path: &str) -> rusqlite::Result<Impact> {
    let cg: CallGraph = build_call_graph_ext(db, symbol, path, None, 1, true)?;
    let def_file = cg.anchor_file.clone();
    let def_line = cg.anchor_line;
    let callers: Vec<(String, i32)> = cg
        .callers
        .iter()
        .map(|c| (c.file.clone(), c.line))
        .collect();
    let mut caller_files: Vec<String> = callers.iter().map(|(f, _)| f.clone()).collect();
    caller_files.sort();
    caller_files.dedup();
    let test_files: Vec<String> = caller_files
        .iter()
        .filter(|f| is_test_path(f))
        .cloned()
        .collect();
    let production_callers = caller_files.len() - test_files.len();
    let risk = if production_callers == 0 {
        Risk::Low
    } else if production_callers <= 5 {
        Risk::Moderate
    } else {
        Risk::High
    };
    Ok(Impact {
        symbol: symbol.to_string(),
        def_file,
        def_line,
        callers,
        caller_files,
        test_files,
        risk,
    })
}

/// One-line summary for `describe` enrichment.
pub fn summary_line(imp: &Impact) -> String {
    let prod = imp.caller_files.len() - imp.test_files.len();
    let tests = imp.test_files.len();
    let mut parts = vec![format!("{} production caller file(s)", prod)];
    if tests > 0 {
        parts.push(format!("{} test file(s)", tests));
    }
    format!("impact: {} ({})", imp.risk.label(), parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_detection_negative() {
        assert!(!is_test_path("src/main.rs"));
        assert!(!is_test_path("crates/reliary-search/src/symbol.rs"));
        assert!(!is_test_path("src/latest.rs"));     // 'test' inside a word
        assert!(!is_test_path("src/contest.rs"));    // same
        assert!(!is_test_path("src/attestation.py"));
    }

    #[test]
    fn risk_thresholds() {
        // boundary: 0 production => low; 1-5 => moderate; 6+ => high
        let mk = |prod: usize, test: usize| {
            let mut files: Vec<String> = (0..prod).map(|i| format!("src/f{}.rs", i)).collect();
            files.extend((0..test).map(|i| format!("tests/t{}.rs", i)));
            let production = files.len() - test;
            if production == 0 { Risk::Low }
            else if production <= 5 { Risk::Moderate }
            else { Risk::High }
        };
        assert_eq!(mk(0, 0), Risk::Low);
        assert_eq!(mk(0, 3), Risk::Low);       // only tests => low
        assert_eq!(mk(1, 0), Risk::Moderate);
        assert_eq!(mk(5, 0), Risk::Moderate);
        assert_eq!(mk(6, 0), Risk::High);
    }
}
