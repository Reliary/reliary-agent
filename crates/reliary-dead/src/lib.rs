/// Grammar-free dead code detection.
/// Uses occurrence counting: an identifier is dead if total_occurrences == definition_occurrences.

use std::collections::{HashMap, HashSet};

/// Configuration for dead code analysis
#[derive(Debug, Clone)]
pub struct DeadConfig {
    pub min_name_len: usize,
    pub test_file_patterns: Vec<String>,
}

impl Default for DeadConfig {
    fn default() -> Self {
        Self {
            min_name_len: 3,
            test_file_patterns: vec!["test".to_string(), "spec".to_string(), "mock".to_string()],
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeadCandidate {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub confidence: Confidence,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// Analyze a single file for dead identifiers
pub fn analyze_file(file: &str, content: &str, config: &DeadConfig) -> Vec<DeadCandidate> {
    let lines: Vec<&str> = content.lines().collect();
    let mut identifiers: HashMap<String, (Vec<usize>, Vec<usize>)> = HashMap::new();
    // Maps: identifier → (definition_lines, total_occurrence_lines)

    for (i, line) in lines.iter().enumerate() {
        // Find all identifiers in this line
        for token in line.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if token.len() < config.min_name_len { continue; }
            if !token.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') { continue; }
            if !token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') { continue; }

            // Check if this line is a definition site
            let t = line.trim();
            let is_def = t.starts_with("fn ") || t.starts_with("def ")
                || t.starts_with("class ") || t.starts_with("struct ")
                || t.starts_with("enum ") || t.starts_with("trait ")
                || t.starts_with("const ") || t.starts_with("let ")
                || t.starts_with("var ") || t.starts_with("function ")
                || t.starts_with("public ") || t.starts_with("private ")
                || line.contains(&format!("fn {}", token))
                || line.contains(&format!("def {}", token));

            let entry = identifiers.entry(token.to_string()).or_insert((Vec::new(), Vec::new()));
            if is_def {
                entry.0.push(i);
            }
            entry.1.push(i);
        }
    }

    let mut candidates = Vec::new();
    let is_test_file = config.test_file_patterns.iter().any(|p| file.contains(p));

    for (name, (def_lines, total_lines)) in identifiers {
        // Dead: defined but never used outside its own definition line
        if def_lines.is_empty() { continue; }
        let def_only = def_lines.iter().all(|def_line| {
            total_lines.iter().all(|&tl| tl == *def_line)
        });
        if !def_only { continue; }

        let confidence = if is_test_file {
            Confidence::Low
        } else if name.chars().all(|c| c.is_ascii_uppercase() || c == '_') && name.len() >= 5 {
            Confidence::High
        } else if name.len() >= 5 {
            Confidence::Medium
        } else {
            Confidence::Low
        };

        let reason = match &confidence {
            Confidence::High => format!("{} — exported but never imported", name),
            Confidence::Medium => format!("{} — defined but never referenced outside definition", name),
            Confidence::Low => format!("{} — potentially dead (test file)", name),
        };

        for &dl in &def_lines {
            candidates.push(DeadCandidate {
                name: name.clone(),
                file: file.to_string(),
                line: dl + 1,
                confidence: confidence.clone(),
                reason: reason.clone(),
            });
        }
    }

    candidates
}

/// Analyze multiple files and return aggregated dead code results
pub fn analyze_files(files: &[(String, String)], config: &DeadConfig) -> Vec<DeadCandidate> {
    let mut all = Vec::new();
    for (file, content) in files {
        all.extend(analyze_file(file, content, config));
    }
    // Dedup by name
    let mut seen = HashSet::new();
    all.into_iter().filter(|c| seen.insert(c.name.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dead_function() {
        let content = "fn helper() {}\nfn main() { helper(); }";
        let config = DeadConfig::default();
        let results = analyze_file("test.rs", content, &config);
        // helper is used, main is used
        let dead: Vec<_> = results.iter().filter(|c| c.name == "helper").collect();
        assert!(dead.is_empty()); // helper is called
    }

    #[test]
    fn test_unused_function() {
        let content = "fn dead_func() {}\nfn main() {}";
        let config = DeadConfig::default();
        let results = analyze_file("test.rs", content, &config);
        assert!(results.iter().any(|c| c.name == "dead_func"));
    }

    #[test]
    fn test_test_file_low_confidence() {
        let content = "fn helper() {}\n";
        let config = DeadConfig::default();
        let results = analyze_file("test_helper.rs", content, &config);
        assert!(!results.is_empty());
        assert_eq!(results[0].confidence, Confidence::Low);
    }

    #[test]
    fn test_high_confidence_constant() {
        let content = "const API_KEY = 123;\n";
        let config = DeadConfig::default();
        let results = analyze_file("config.rs", content, &config);
        // constants with all-caps names get high confidence
        let high: Vec<_> = results.iter().filter(|c| c.confidence == Confidence::High).collect();
        assert!(!high.is_empty());
    }
}
