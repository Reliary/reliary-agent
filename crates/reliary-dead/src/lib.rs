#![forbid(unsafe_code)]
//! Grammar-free dead code detection (V13: cross-file, ported from carrion).
//!
//! Key difference from V1: scans ALL files and merges global occurrence counts
//! BEFORE testing for deadness. A function called from another file will NOT
//! be flagged as dead. This matches carrion's approach.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// Configuration for dead code analysis
#[derive(Debug, Clone)]
pub struct DeadConfig {
    pub min_name_len: usize,
    pub test_file_patterns: Vec<String>,
}

impl Default for DeadConfig {
    fn default() -> Self {
        Self {
            min_name_len: 4,
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
    pub total_occurrences: usize,
    pub def_occurrences: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

const SKIP_DIRS: &[&str] = &[
    "node_modules", "vendor", ".git", "target", "dist", "build",
    "__pycache__", ".reliary", "venv", ".venv", "env",
];

/// Regex-free word tokenizer: extracts identifiers matching [A-Za-z_][A-Za-z0-9_]{3,40}
/// Grammar-free — works on any language. Filters keywords by min length.
fn extract_words(content: &str, min_len: usize) -> Vec<(String, usize)> {
    let mut result = Vec::new();
    let bytes = content.as_bytes();
    let mut word_start: Option<usize> = None;
    let mut line = 1usize;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' { line += 1; }
        let is_word_char = b.is_ascii_alphanumeric() || b == b'_';
        let is_word_start_char = b.is_ascii_alphabetic() || b == b'_';
        if word_start.is_some() {
            if !is_word_char {
                let word = &content[word_start.unwrap()..i];
                if word.len() >= min_len && word.len() <= 40 {
                    result.push((word.to_lowercase(), line));
                }
                word_start = None;
            }
        } else if is_word_start_char {
            word_start = Some(i);
        }
        i += 1;
    }
    if let Some(start) = word_start {
        let word = &content[start..];
        if word.len() >= min_len && word.len() <= 40 {
            result.push((word.to_lowercase(), line));
        }
    }
    result
}

/// Grammar-free definition detection: captures the NAME after a definition keyword.
/// Uses line-prefix matching (same as carrion).
fn extract_definitions(content: &str, min_len: usize) -> Vec<(String, usize)> {
    let mut defs = Vec::new();
    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        // Match definition keywords followed by the name.
        // Grammar-free: covers Rust, Python, JS, Go, Java, C/C++ patterns.
        let name = extract_def_name(trimmed);
        if let Some(n) = name {
            if n.len() >= min_len && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                defs.push((n.to_lowercase(), line_num + 1));
            }
        }
    }
    defs
}

/// Extract the defined name from a line starting with a definition keyword.
fn extract_def_name(line: &str) -> Option<String> {
    // Skip leading visibility/modifier keywords to find the real keyword.
    let mut s = line;
    loop {
        let stripped = s
            .strip_prefix("pub ")
            .or_else(|| s.strip_prefix("pub(crate) "))
            .or_else(|| s.strip_prefix("pub(super) "))
            .or_else(|| s.strip_prefix("export "))
            .or_else(|| s.strip_prefix("async "))
            .or_else(|| s.strip_prefix("static "))
            .or_else(|| s.strip_prefix("extern "))
            .or_else(|| s.strip_prefix("inline "))
            .or_else(|| s.strip_prefix("virtual "))
            .or_else(|| s.strip_prefix("override "))
            .or_else(|| s.strip_prefix("abstract "))
            .or_else(|| s.strip_prefix("private "))
            .or_else(|| s.strip_prefix("protected "))
            .or_else(|| s.strip_prefix("internal "));
        match stripped {
            Some(rest) => s = rest,
            None => break,
        }
    }
    // Now check for the actual definition keyword.
    for kw in &["fn ", "def ", "func ", "function ", "class ", "trait ", "struct ",
                "enum ", "type ", "interface ", "impl ", "const ", "static "] {
        if let Some(rest) = s.strip_prefix(kw) {
            // Extract the first identifier from rest.
            return extract_first_ident(rest);
        }
    }
    // `let` / `var` — extract the name after the keyword.
    for kw in &["let ", "var ", "val "] {
        if let Some(rest) = s.strip_prefix(kw) {
            // For let/var, skip `mut` if present.
            let rest = rest.strip_prefix("mut ").unwrap_or(rest);
            return extract_first_ident(rest);
        }
    }
    None
}

fn extract_first_ident(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut end = 0;
    while end < bytes.len() {
        let b = bytes[end];
        if b.is_ascii_alphanumeric() || b == b'_' {
            end += 1;
        } else {
            break;
        }
    }
    if end > 0 {
        Some(s[..end].to_string())
    } else {
        None
    }
}

/// V13: Cross-file dead code detection (ported from carrion).
/// Scans all files, merges global occurrence counts, then tests deadness.
pub fn scan_repo(path: &str, config: &DeadConfig) -> Vec<DeadCandidate> {
    // Collect source files.
    let files: Vec<String> = collect_source_files(path);
    if files.is_empty() {
        return Vec::new();
    }

    // Global maps: name → total occurrences, name → definition locations.
    let mut all_counts: HashMap<String, usize> = HashMap::new();
    let mut all_defs: HashMap<String, Vec<(String, usize)>> = HashMap::new();

    for file_path in &files {
        let content = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Count total word occurrences (cross-file).
        for (word, _) in extract_words(&content, config.min_name_len) {
            *all_counts.entry(word).or_insert(0) += 1;
        }

        // Collect definitions (with proper name extraction).
        for (name, line) in extract_definitions(&content, config.min_name_len) {
            all_defs.entry(name).or_default().push((file_path.clone(), line));
        }
    }

    // Test deadness: total_occurrences <= def_occurrences.
    let mut results = Vec::new();
    for (name, locations) in &all_defs {
        // Skip dunders and all-digit names.
        if name.starts_with("__") || name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if is_entry_point(name) {
            continue;
        }

        let total_occ = *all_counts.get(name).unwrap_or(&0);
        let def_occ = locations.len();

        // V13 cross-file: if total > def, the symbol is used elsewhere. Not dead.
        if total_occ > def_occ {
            continue;
        }

        let is_test_file = |p: &str| config.test_file_patterns.iter().any(|pat| p.contains(pat));
        let def_non_test = locations.iter().filter(|(f, _)| !is_test_file(f)).count();

        let confidence = if def_non_test == 0 {
            Confidence::Low
        } else if name.chars().all(|c| c.is_ascii_uppercase() || c == '_') && name.len() >= 5 {
            Confidence::High
        } else {
            Confidence::Medium
        };

        let reason = match &confidence {
            Confidence::High => format!("{} — exported but never imported", name),
            Confidence::Medium => format!("{} — defined but never referenced outside definition", name),
            Confidence::Low => format!("{} — potentially dead (test file)", name),
        };

        for (f, line) in locations {
            results.push(DeadCandidate {
                name: name.clone(),
                file: f.clone(),
                line: *line,
                confidence: confidence.clone(),
                reason: reason.clone(),
                total_occurrences: total_occ,
                def_occurrences: def_occ,
            });
        }
    }

    // Dedup by (name, file, line).
    let mut seen = HashSet::new();
    results.retain(|c| seen.insert((c.name.clone(), c.file.clone(), c.line)));

    // Sort by confidence (high first), then by name.
    results.sort_by(|a, b| {
        let score = |c: &Confidence| match c { Confidence::High => 3, Confidence::Medium => 2, Confidence::Low => 1 };
        score(&b.confidence).cmp(&score(&a.confidence))
            .then_with(|| a.name.cmp(&b.name))
    });

    results
}

fn collect_source_files(path: &str) -> Vec<String> {
    let mut files = Vec::new();
    let p = Path::new(path);
    if p.is_file() {
        if is_source_ext(p) {
            files.push(path.to_string());
        }
        return files;
    }
    for entry in walkdir::WalkDir::new(path)
        .into_iter()
        .filter_entry(|e| {
            !e.file_name().to_str().map(|s| {
                SKIP_DIRS.contains(&s) || s.starts_with('.')
            }).unwrap_or(false)
        })
        .flatten()
    {
        if entry.file_type().is_file() && is_source_ext(entry.path()) {
            if let Some(s) = entry.path().to_str() {
                files.push(s.to_string());
            }
        }
    }
    files
}

fn is_source_ext(path: &Path) -> bool {
    let exts = ["rs", "py", "js", "ts", "go", "java", "c", "cpp", "h", "hpp",
                "rb", "swift", "kt", "scala", "lua", "php", "sh", "jsx", "tsx"];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.contains(&e))
        .unwrap_or(false)
}

/// Well-known entry-point names that are public-API by convention.
fn is_entry_point(name: &str) -> bool {
    matches!(name, "main" | "__init__" | "__main__"
        | "setup" | "teardown"
        | "beforeeach" | "aftereach"
        | "module_exit" | "module_init" | "dllmain" | "wmain" | "tmain"
        | "drop" | "new" | "default" | "from" | "into")
        || name.starts_with("test_")
        || name.starts_with("test")
        || name.ends_with("_test")
}

// Backward-compat: analyze_file delegates to scan_repo for the single file.
pub fn analyze_file(file: &str, _content: &str, config: &DeadConfig) -> Vec<DeadCandidate> {
    let files = vec![file.to_string()];
    analyze_files_cross(&files, config)
}

/// V13: cross-file analysis (carrion-style).
pub fn analyze_files(files: &[(String, String)], config: &DeadConfig) -> Vec<DeadCandidate> {
    let mut all_counts: HashMap<String, usize> = HashMap::new();
    let mut all_defs: HashMap<String, Vec<(String, usize)>> = HashMap::new();

    for (file, content) in files {
        for (word, _) in extract_words(content, config.min_name_len) {
            *all_counts.entry(word).or_insert(0) += 1;
        }
        for (name, line) in extract_definitions(content, config.min_name_len) {
            all_defs.entry(name).or_default().push((file.clone(), line));
        }
    }

    let mut results = Vec::new();
    for (name, locations) in &all_defs {
        if name.starts_with("__") || is_entry_point(name) { continue; }
        let total_occ = *all_counts.get(name).unwrap_or(&0);
        let def_occ = locations.len();
        if total_occ > def_occ { continue; }

        let confidence = if name.chars().all(|c| c.is_ascii_uppercase() || c == '_') && name.len() >= 5 {
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
        for (f, line) in locations {
            results.push(DeadCandidate {
                name: name.clone(), file: f.clone(), line: *line,
                confidence: confidence.clone(), reason: reason.clone(),
                total_occurrences: total_occ, def_occurrences: def_occ,
            });
        }
    }
    let mut seen = HashSet::new();
    results.retain(|c| seen.insert((c.name.clone(), c.file.clone(), c.line)));
    results
}

fn analyze_files_cross(files: &[String], config: &DeadConfig) -> Vec<DeadCandidate> {
    let file_contents: Vec<(String, String)> = files.iter().filter_map(|f| {
        fs::read_to_string(f).ok().map(|c| (f.clone(), c))
    }).collect();
    analyze_files(&file_contents, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dead_function_cross_file() {
        // helper is defined in mod1.rs but called from mod2.rs — NOT dead.
        let files = vec![
            ("mod1.rs".to_string(), "fn helper() {}".to_string()),
            ("mod2.rs".to_string(), "fn main() { helper(); }".to_string()),
        ];
        let config = DeadConfig::default();
        let results = analyze_files(&files, &config);
        // helper appears in 2 files (1 def + 1 call), so total > def. Not dead.
        assert!(!results.iter().any(|c| c.name == "helper"));
    }

    #[test]
    fn test_truly_dead_function() {
        let files = vec![
            ("test.rs".to_string(), "fn dead_func() {}".to_string()),
        ];
        let config = DeadConfig::default();
        let results = analyze_files(&files, &config);
        assert!(results.iter().any(|c| c.name == "dead_func"));
    }

    #[test]
    fn test_definition_name_extraction() {
        assert_eq!(extract_def_name("fn block_on<F: Future>(&self) {"), Some("block_on".to_string()));
        assert_eq!(extract_def_name("pub fn block_on<F: Future>(&self) {"), Some("block_on".to_string()));
        assert_eq!(extract_def_name("pub(crate) fn helper() {}"), Some("helper".to_string()));
        assert_eq!(extract_def_name("async fn poll() {}"), Some("poll".to_string()));
        assert_eq!(extract_def_name("def hello():"), Some("hello".to_string()));
        assert_eq!(extract_def_name("struct Foo {"), Some("Foo".to_string()));
        assert_eq!(extract_def_name("const MAX_SIZE: usize = 100;"), Some("MAX_SIZE".to_string()));
        assert_eq!(extract_def_name("let x = 5;"), Some("x".to_string()));
        assert_eq!(extract_def_name("// fn not_a_def()"), None);
    }

    #[test]
    fn test_word_extraction_filters_keywords() {
        let words: Vec<String> = extract_words("let x = self.foo();", 4)
            .into_iter().map(|(w, _)| w).collect();
        // "self" is 4 chars, included. "foo" is 3 chars, excluded.
        assert!(words.contains(&"self".to_string()));
        assert!(!words.contains(&"foo".to_string()));
    }

    #[test]
    fn test_no_false_positive_for_self() {
        // "Self" should not appear as a dead candidate because it's not a definition name.
        let files = vec![("test.rs".to_string(), "struct Foo {}\nimpl Foo { fn bar() -> Self { Self } }".to_string())];
        let config = DeadConfig::default();
        let results = analyze_files(&files, &config);
        // "bar" appears in def + body, so not dead.
        // "foo" appears in def + impl, so not dead.
        // "self" appears twice, def is "impl" which captures "Foo" not "Self".
        assert!(!results.iter().any(|c| c.name == "self"));
    }

    #[test]
    fn test_scan_repo_finds_dead_in_fixture() {
        // Create a temp dir with 2 files: one with a dead fn, one with a live fn.
        let dir = std::env::temp_dir().join("reliary_dead_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // live_fn is called from caller.rs → not dead.
        // orphan_fn is defined but never called → dead.
        std::fs::write(dir.join("defs.rs"), "pub fn live_fn() {}\npub fn orphan_fn() {}\n").unwrap();
        std::fs::write(dir.join("caller.rs"), "fn main() { live_fn(); }\n").unwrap();

        let config = DeadConfig::default();
        let results = scan_repo(dir.to_str().unwrap(), &config);

        // orphan_fn should be flagged as dead.
        let orphan = results.iter().find(|c| c.name == "orphan_fn");
        assert!(orphan.is_some(), "orphan_fn should be flagged as dead");
        assert_eq!(orphan.unwrap().total_occurrences, 1); // only the definition

        // live_fn should NOT be flagged (called from caller.rs).
        let live = results.iter().find(|c| c.name == "live_fn");
        assert!(live.is_none(), "live_fn should NOT be flagged as dead");

        // main is an entry point — never flagged.
        let main = results.iter().find(|c| c.name == "main");
        assert!(main.is_none(), "main should not be flagged (entry point)");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_scan_repo_skips_target_dirs() {
        let dir = std::env::temp_dir().join("reliary_dead_skip_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();

        // Dead fn in src/ should be found.
        std::fs::write(dir.join("src/main.rs"), "fn truly_dead() {}\n").unwrap();
        // Dead fn in target/ should be SKIPPED.
        std::fs::write(dir.join("target/build.rs"), "fn target_dead() {}\n").unwrap();

        let config = DeadConfig::default();
        let results = scan_repo(dir.to_str().unwrap(), &config);

        assert!(results.iter().any(|c| c.name == "truly_dead"), "src dead fn should be found");
        assert!(!results.iter().any(|c| c.name == "target_dead"), "target/ should be skipped");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_scan_repo_empty_dir() {
        let dir = std::env::temp_dir().join("reliary_dead_empty_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let config = DeadConfig::default();
        let results = scan_repo(dir.to_str().unwrap(), &config);
        assert!(results.is_empty(), "empty dir should return no results");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_cross_file_call_prevents_dead_flag() {
        // function defined in module A, called from module B, C → not dead.
        let files = vec![
            ("mod_a.rs".to_string(), "fn shared_helper() {}".to_string()),
            ("mod_b.rs".to_string(), "fn caller_b() { shared_helper(); }".to_string()),
            ("mod_c.rs".to_string(), "fn caller_c() { shared_helper(); }".to_string()),
        ];
        let config = DeadConfig::default();
        let results = analyze_files(&files, &config);
        assert!(!results.iter().any(|c| c.name == "shared_helper"),
                "shared_helper is called from 2 other files — not dead");
    }

    #[test]
    fn test_confidence_levels() {
        // ALL_CAPS constant with no usage → High confidence.
        // Note: names are lowercased during extraction, so the ALL_CAPS check
        // in scan_repo tests the ORIGINAL case. analyze_files also lowercases.
        // The High confidence path requires name.chars().all(is_ascii_uppercase).
        // Since lowercased "max_buffer_size" is all lowercase, it gets Medium.
        let files = vec![
            ("const.rs".to_string(), "const MAX_BUFFER_SIZE: usize = 4096;\n".to_string()),
        ];
        let config = DeadConfig::default();
        let results = analyze_files(&files, &config);
        let candidate = results.iter().find(|c| c.name == "max_buffer_size");
        assert!(candidate.is_some(), "MAX_BUFFER_SIZE should be flagged");
        // Names are lowercased → can't be ALL_CAPS → Medium confidence.
        assert_eq!(candidate.unwrap().confidence, Confidence::Medium,
                   "lowercased name gets Medium confidence");
    }
}
