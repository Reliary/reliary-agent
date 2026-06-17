//! Command-output line classification + skeleton normalization.
use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub enum OutputLineType {
    Blank,
    Separator,
    Progress,
    Error,
    Warning,
    Summary,
    PrefixLine { prefix: String, body: String },
    Code,
}

#[derive(Debug, Clone)]
pub struct OutputLine {
    pub text: String,
    pub line_type: OutputLineType,
    pub skeleton: String,
}

struct Patterns {
    ansi: Regex,
    uuid: Regex,
    hex40: Regex,
    version: Regex,
    numbers: Regex,
    timestamp: Regex,
    progress: Regex,
    error_starts: [&'static str; 5],
    warning_starts: [&'static str; 2],
    test_header: Regex,
    separator_re: Regex,
}

static PATTERNS: std::sync::LazyLock<Patterns> = std::sync::LazyLock::new(|| Patterns {
    ansi: Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~]|\])").unwrap(),
    uuid: Regex::new(r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap(),
    hex40: Regex::new(r"(?i)\b[0-9a-f]{7,40}\b").unwrap(),
    version: Regex::new(r"\b\d+\.\d+(?:\.\d+(?:-\w+(?:\.\d+)?)?)?\b").unwrap(),
    numbers: Regex::new(r"\b\d+\b").unwrap(),
    timestamp: Regex::new(r"\d{2}:\d{2}:\d{2}(?:[.,]\d{3,})?").unwrap(),
    progress: Regex::new(r"^\s*(?:Compiling|Checking|Building|Linking|Running|Processing|Generating)\s").unwrap(),
    error_starts: ["error:", "error[", "Error:", "FAILED", "thread '"],
    warning_starts: ["warning:", "Warning:"],
    test_header: Regex::new(r"^(?:running|test|tests? result)").unwrap(),
    separator_re: Regex::new(r"^\s*[-=*_.~]{3,}\s*$").unwrap(),
});

/// Strip ANSI escape sequences from text.
pub fn strip_ansi(text: &str) -> String {
    PATTERNS.ansi.replace_all(text, "").to_string()
}

/// Normalize concrete values to structural skeletons.
pub fn skeleton(text: &str) -> String {
    let cleaned = strip_ansi(text);
    let s = PATTERNS.uuid.replace_all(&cleaned, "{uuid}");
    let s = PATTERNS.hex40.replace_all(&s, "{hash}");
    let s = PATTERNS.version.replace_all(&s, "{ver}");
    let s = PATTERNS.timestamp.replace_all(&s, "{time}");
    let s = PATTERNS.progress.replace_all(&s, "{progress}");
    let s = PATTERNS.numbers.replace_all(&s, "{n}");
    s.to_string()
}

/// Classify a single line of command output.
pub fn classify_output_line(_line: &str, trimmed: &str) -> OutputLineType {
    if trimmed.is_empty() {
        return OutputLineType::Blank;
    }

    if PATTERNS.separator_re.is_match(trimmed) {
        return OutputLineType::Separator;
    }

    for pat in &PATTERNS.error_starts {
        if trimmed.starts_with(pat) {
            return OutputLineType::Error;
        }
    }

    for pat in &PATTERNS.warning_starts {
        if trimmed.starts_with(pat) {
            return OutputLineType::Warning;
        }
    }

    let lower = trimmed.to_lowercase();
    if (lower.contains("passed") && lower.contains("failed"))
        || trimmed.starts_with("test result:")
        || trimmed.starts_with("Finished ")
        || trimmed.starts_with("  --> ")
        || trimmed.starts_with("error[")
    {
        return OutputLineType::Summary;
    }

    if PATTERNS.test_header.is_match(trimmed) {
        return OutputLineType::Summary;
    }

    if PATTERNS.progress.is_match(trimmed) {
        return OutputLineType::Progress;
    }

    // Prefix detection: leading word followed by separator
    if let Some(pos) = trimmed.find(['>', ' ']) {
        let prefix = &trimmed[..pos];
        if (5..=20).contains(&prefix.len()) {
            let after = &trimmed[pos..].trim_start();
            if after.len() > 5 {
                return OutputLineType::PrefixLine {
                    prefix: prefix.to_string(),
                    body: after.to_string(),
                };
            }
        }
    }

    OutputLineType::Code
}

/// Classify all lines in command output.
pub fn classify_output(text: &str) -> Vec<OutputLine> {
    text.lines().map(|line| {
        let cleaned = strip_ansi(line);
        let trimmed = cleaned.trim();
        let lt = classify_output_line(&cleaned, trimmed);
        let skel = skeleton(&cleaned);
        OutputLine {
            text: cleaned,
            line_type: lt,
            skeleton: skel,
        }
    }).collect()
}
