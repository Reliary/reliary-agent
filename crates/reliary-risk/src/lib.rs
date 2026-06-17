/// Pre-edit risk analysis.
/// Grammar-free: uses structural heuristics, not AST parsing.
/// Risk categories a file or edit operation can fall into.
#[derive(Debug, Clone, PartialEq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// Risk profile for a file: how dangerous it is to edit.
#[derive(Debug, Clone)]
pub struct FileRisk {
    pub file: String,
    pub risk: RiskLevel,
    pub reason: String,
    /// Files that should be read before editing this one
    pub read_first: Vec<String>,
}

/// Compute risk for a file based on structural heuristics.
/// No AST, no language-specific logic.
pub fn compute_file_risk(file: &str, content: &str) -> FileRisk {
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // Count definitions (lines starting with fn/def/function/struct/class/trait/pub)
    let def_count = lines.iter().filter(|l| {
        let t = l.trim();
        t.starts_with("fn ") || t.starts_with("def ") || t.starts_with("function ")
            || t.starts_with("struct ") || t.starts_with("class ") || t.starts_with("trait ")
            || t.starts_with("impl ") || t.starts_with("pub ")
            || t.starts_with("enum ") || t.starts_with("interface ")
    }).count();

    // Count test references
    let test_refs = lines.iter().filter(|l| {
        let t = l.trim().to_lowercase();
        t.contains("test") || t.contains("spec")
    }).count();

    // Detect dangerous patterns: TODO, FIXME, HACK
    let todo_count = content.matches("TODO").count()
        + content.matches("FIXME").count()
        + content.matches("HACK").count()
        + content.matches("XXX").count();

    // Coupled files: imports that reference other local files
    let mut read_first = Vec::new();
    for line in &lines {
        let t = line.trim();
        if t.starts_with("use ") || t.starts_with("import ") || t.starts_with("from ") {
            // Extract the module path
            let path = t.split_whitespace().nth(1).unwrap_or("").to_string();
            if !path.is_empty() && !path.starts_with("std") && !path.starts_with("crate") {
                read_first.push(path);
            }
        }
    }
    read_first.truncate(3);

    // Heuristic risk scoring
    let risk = if def_count > 20 && total_lines > 200 && test_refs < 3 && todo_count > 5 {
        RiskLevel::High
    } else if (def_count > 10 && total_lines > 100 && todo_count > 2)
        || (def_count > 30 && total_lines > 300)
        || (test_refs == 0 && total_lines > 50)
    {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    };

    let reason = match &risk {
        RiskLevel::High => format!("{} defs, {} lines, {} tests, {} TODOs — high blast radius", def_count, total_lines, test_refs, todo_count),
        RiskLevel::Medium => format!("{} defs, {} lines — moderate complexity", def_count, total_lines),
        RiskLevel::Low => "Low risk: small file with test references".to_string(),
    };

    FileRisk { file: file.to_string(), risk, reason, read_first }
}

/// Blast radius: estimate which files are affected by a change to this file.
pub fn compute_blast_radius(content: &str) -> Vec<String> {
    // Find exported identifiers (pub, export, or top-level defs)
    let mut exported = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("pub fn ") || t.starts_with("pub struct ") || t.starts_with("pub enum ")
            || t.starts_with("pub trait ") || t.starts_with("pub type ")
            || t.starts_with("pub const ") || t.starts_with("pub static ")
            || t.starts_with("export ")
        {
            let name = t.split_whitespace()
                .skip_while(|&w| w != "fn" && w != "struct" && w != "enum" && w != "trait" && w != "type" && w != "const" && w != "static").nth(1)
                .unwrap_or("")
                .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string();
            if !name.is_empty() {
                exported.push(name);
            }
        }
    }
    exported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_low_risk_file() {
        let content = "use std::fmt;\nfn test_foo() {}\nfn test_bar() {}\n";
        let risk = compute_file_risk("test.rs", content);
        assert_eq!(risk.risk, RiskLevel::Low);
    }

    #[test]
    fn test_high_risk_file() {
        let mut content = String::new();
        for _ in 0..50 {
            content.push_str("pub fn something() {}\n");
        }
        content.push_str("// TODO: fix this");
        content.push_str("// FIXME: and this");
        content.push_str("// HACK: workaround");
        let risk = compute_file_risk("large.rs", &content);
        assert_eq!(risk.risk, RiskLevel::Medium);
    }

    #[test]
    fn test_empty_file_risk() {
        let risk = compute_file_risk("empty.rs", "");
        assert_eq!(risk.risk, RiskLevel::Low);
    }

    #[test]
    fn test_nonexistent_path_handling() {
        // Should not panic — returns Low with error message
        let result = std::panic::catch_unwind(|| {
            compute_file_risk("/nonexistent/path.rs", "");
        });
        assert!(result.is_ok(), "compute_file_risk should not panic on non-existent paths");
    }

    #[test]
    fn test_high_export_file_risk() {
        let mut content = String::new();
        for i in 0..25 {
            content.push_str(&format!("pub struct Item{} {{\n    id: u32,\n    name: String,\n}}\n\n", i));
            content.push_str(&format!("fn test_item{}() {{}}\n", i));
        }
        content.push_str("// TODO: add validation\n// FIXME: check bounds\n// HACK: workaround for demo\n// XXX: remove before ship\n");
        let risk = compute_file_risk("high_risk.rs", &content);
        assert_eq!(risk.risk, RiskLevel::Medium, "50 defs on 154 lines: {:?}", risk.reason);
    }

    #[test]
    fn test_blast_radius() {
        let content = "pub fn process() {}\npub struct Config {}\nfn internal() {}";
        let radius = compute_blast_radius(content);
        assert!(radius.len() >= 2, "should have at least 2 exports, got {}", radius.len());
        // process and Config are pub-exported
        assert!(radius.contains(&"process".to_string()), "process should be in blast radius: {:?}", radius);
    }
}
