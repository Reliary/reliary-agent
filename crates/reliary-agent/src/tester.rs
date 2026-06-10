/// Grammar-free test runner detection + structured output.
/// No language detection, no source code parsing.
/// Uses: which(1) for binary discovery, config file existence, chronicle caching.

use std::path::Path;
use std::process::Command;

const FALLBACK_ORDER: &[(&str, &str, &str)] = &[
    ("cargo",   "Cargo.toml",    "cargo test 2>&1"),
    ("python3", "pyproject.toml","python3 -m pytest 2>&1"),
    ("python",  "pyproject.toml","python -m pytest 2>&1"),
    ("node",    "package.json",  "npm test 2>&1"),
    ("npx",     "package.json",  "npx jest 2>&1"),
    ("go",      "go.mod",        "go test ./... 2>&1"),
    ("make",    "Makefile",      "make test 2>&1"),
    ("rake",    "Gemfile",       "rake test 2>&1"),
];

fn bin_in_path(binary: &str) -> bool {
    Command::new("which")
        .arg(binary)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_or(false, |s| s.success())
}

/// Detect test runner: chronicle cache → which + config → fallback list.
/// Returns (shell_command_string, binary_name).
pub fn detect(workdir: &str) -> Option<(String, String)> {
    let root = Path::new(workdir);
    let cache_path = root.join(".reliary").join("test_cache");

    // 1. Chronicled cache
    if let Ok(cached) = std::fs::read_to_string(&cache_path) {
        let t = cached.trim();
        if !t.is_empty() {
            let bin = t.split_whitespace().next().unwrap_or("");
            if bin_in_path(bin) {
                return Some((t.to_string(), bin.to_string()));
            }
        }
    }

    // 2. which(1) + config file existence
    for (binary, config, cmd) in FALLBACK_ORDER {
        if bin_in_path(binary) && root.join(config).exists() {
            // Cache for next time
            if let Ok(dir) = std::fs::create_dir_all(root.join(".reliary")) {
                let _ = std::fs::write(&cache_path, cmd);
            }
            return Some((cmd.to_string(), binary.to_string()));
        }
    }

    None
}

/// Run tests, return structured output.
/// Format: "PASS N | TOTAL M" or "FAIL N | FIRST: <error line>"
pub fn run(workdir: &str) -> String {
    match detect(workdir) {
        Some((cmd, _)) => {
            let output = match Command::new("sh")
                .args(["-c", &cmd])
                .current_dir(workdir)
                .output()
            {
                Ok(o) => o,
                Err(e) => return format!("ERROR: test execution failed — {}", e),
            };
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{}{}", stdout, stderr);

            let passed = combined.lines()
                .filter(|l| l.contains("PASS") || l.contains("ok")
                    || (l.contains("test result") && l.contains("passed")))
                .count();

            let failed_lines: Vec<&str> = combined.lines()
                .filter(|l| (l.contains("FAIL") || l.contains("FAILED") || l.contains("failed"))
                    && !l.contains("test result"))
                .collect();

            if failed_lines.is_empty() {
                format!("PASS {} | {} tests", passed,
                    combined.lines().filter(|l| l.contains("test") || l.contains("Test")).count())
            } else {
                let first = failed_lines[0].chars().take(120).collect::<String>();
                format!("FAIL {} | FIRST: {}", failed_lines.len(), first)
            }
        }
        None => {
            let available: Vec<&str> = FALLBACK_ORDER.iter()
                .filter(|(b,_,_)| bin_in_path(b))
                .map(|(b,_,_)| *b)
                .collect();
            format!("ERROR: no test runner found. Available: {}",
                if available.is_empty() { "none".to_string() } else { available.join(", ") })
        }
    }
}
