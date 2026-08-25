// S3 fix: tests for warmup text generation and function name extraction

fn extract_function_name(task: &str) -> Option<String> {
    // Extract first `\w+` after a backtick
    let bytes = task.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            // find the identifier
            let start = i + 1;
            let mut end = start;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
            {
                end += 1;
            }
            if end > start {
                return Some(task[start..end].to_string());
            }
        }
        i += 1;
    }
    None
}

fn make_warmup_text(func: &str) -> String {
    format!(
        "Read these entries and answer briefly:\n\
         1. What does `{func}` return for empty/blank input?\n\
         2. What's a key surprise or edge case in `{func}`?\n\
         3. Which functions call or interact with `{func}`?\n\
         Reply briefly for each."
    )
}

#[test]
fn s3_warmup_extracts_function_name_basic() {
    let task = "In `skeleton()`, what are the hex hash lengths?";
    assert_eq!(extract_function_name(task), Some("skeleton".to_string()));
}

#[test]
fn s3_warmup_extracts_function_name_snake_case() {
    let task = "What does `classify_line` return for blank input?";
    assert_eq!(extract_function_name(task), Some("classify_line".to_string()));
}

#[test]
fn s3_warmup_extracts_returns_first_identifier_in_namespaced() {
    let task = "How does `MaxwellGate::score()` handle errors?";
    // Extracts just "MaxwellGate" — the first identifier
    assert_eq!(extract_function_name(task), Some("MaxwellGate".to_string()));
}

#[test]
fn s3_warmup_extracts_returns_none_when_no_backticks() {
    assert_eq!(extract_function_name("No backticks here"), None);
}

#[test]
fn s3_warmup_text_includes_three_questions() {
    let text = make_warmup_text("skeleton");
    assert_eq!(text.matches('?').count(), 3);
    assert!(text.contains("Read these entries"));
    assert!(text.contains("empty/blank"));
    assert!(text.contains("key surprise"));
    assert!(text.contains("call or interact"));
    assert!(text.contains("Reply briefly"));
}

#[test]
fn s3_warmup_text_uses_function_name() {
    let text = make_warmup_text("classify_line");
    assert!(text.contains("`classify_line`"));
    assert!(!text.contains("`skeleton`"));
}
