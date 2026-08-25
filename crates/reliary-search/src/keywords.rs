//! Language keyword filter — at ingest time, skip keywords that bloat the
//! occurrence table without carrying discriminative information.
//!
//! ArC 33 Phase B Trick #1: ~30% of occurrence rows are noise keywords.
//! Excluding them cuts DB size by ~30% and indexing time by ~40%.

use std::collections::HashSet;
use std::sync::OnceLock;

static KEYWORDS: OnceLock<HashSet<String>> = OnceLock::new();

fn keywords() -> &'static HashSet<String> {
    KEYWORDS.get_or_init(|| {
        let words = [
            // C/C++ keywords (after porter stem pass: lowercase)
            "int", "void", "char", "long", "short", "float", "double",
            "unsigned", "signed", "const", "static", "extern", "volatile",
            "register", "inline", "auto", "restrict",
            "if", "else", "switch", "case", "default", "for", "while", "do",
            "break", "continue", "goto", "return",
            "typedef", "struct", "union", "enum",
            "sizeof", "alignof",
            "true", "false",
            // Rust keywords (post-porter)
            "let", "mut", "pub", "fn", "impl", "trait", "use", "mod",
            "match", "self", "self_", "super", "crate", "async", "await",
            "where", "move", "ref", "box", "dyn", "in",
            // Python keywords (post-porter)
            "def", "class", "import", "from", "lambda", "yield", "with",
            "assert", "raise", "pass", "none", "true", "false",
            // JS/TS keywords (post-porter)
            "var", "let", "const", "function", "return",
            "this", "new", "typeof", "instanceof",
            "try", "catch", "finally", "throw",
            // Common types that produce noise
            "string", "size_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t",
            "int8_t", "int16_t", "int32_t", "int64_t",
            "bool", "true", "false", "null", "nil", "none",
        ];
        words.iter().map(|s| s.to_string()).collect()
    })
}

/// Returns true if the porter-stemmed token is a known noise keyword.
pub fn is_keyword(stemmed: &str) -> bool {
    keywords().contains(stemmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_keywords() {
        assert!(is_keyword("int"));
        assert!(is_keyword("let"));
        assert!(is_keyword("struct"));
        assert!(is_keyword("pub"));
    }

    #[test]
    fn test_real_words() {
        assert!(!is_keyword("hello"));
        assert!(!is_keyword("compute"));
        assert!(!is_keyword("runner"));
    }
}
