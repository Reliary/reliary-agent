//! V24: Qualified name derivation — grammar-free.
//! Two-pass approach:
//! 1. Fast path: file-path basename → capitalize → type name
//! 2. Fallback: brace-graph walk-up to nearest type_def / impl_target
use crate::brace_graph::BraceNode;
use crate::file_meta::FileMeta;

/// Capitalize the first ASCII letter of a string.
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {
            let mut out = String::with_capacity(s.len());
            out.push(c.to_ascii_uppercase());
            out.push_str(chars.as_str());
            out
        }
        _ => s.to_string(),
    }
}

/// Strip snake_case underscores and capitalize each component.
fn pascal_case(s: &str) -> String {
    s.split('_')
        .filter(|c| !c.is_empty())
        .map(capitalize_first)
        .collect()
}

/// V24: Extract the type name from a file path.
pub fn type_name_from_path(file_path: &str) -> String {
    let basename = file_path
        .rsplit('/')
        .next()
        .and_then(|f| f.rsplit('.').next_back())
        .unwrap_or("");

    let candidate = if basename == "mod"
        || basename == "lib"
        || basename == "tests"
        || basename == "index"
    {
        let parts: Vec<&str> = file_path.rsplitn(3, '/').collect();
        if parts.len() >= 3 {
            parts[1]
        } else {
            basename
        }
    } else {
        basename
    };

    let pascal = pascal_case(candidate);
    if pascal.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
        pascal
    } else {
        String::new()
    }
}

/// V24: Extract type name from brace-graph node's first_line_text.
/// For `type_def` nodes, the text is "struct TypeName" or "class TypeName" — take the last word.
/// For `impl_target` nodes, the text is "impl Trait for Type" — take the type.
fn type_name_from_node_text(role: &str, text: &str) -> Option<String> {
    if role != "type_def" && role != "impl_target" {
        return None;
    }
    // Try to extract the LAST identifier from the text.
    let trimmed = text.trim();
    if role == "impl_target" {
        // "impl Trait for Type" → take after "for"
        if let Some(pos) = trimmed.rfind(" for ") {
            let after = &trimmed[pos + 5..];
            // Take first identifier.
            let ident: String = after.chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !ident.is_empty() {
                return Some(ident);
            }
        }
        // "impl Type" → take last word
        if let Some(last) = trimmed.split_whitespace().last() {
            // Strip generics: "Type<T>" → "Type"
            let base: String = last.chars().take_while(|c| *c != '<').collect();
            if !base.is_empty() {
                return Some(base);
            }
        }
    } else {
        // type_def: "struct Type" or "class Type" or "Type ="
        if let Some(last) = trimmed.split_whitespace().last() {
            let base: String = last.chars().take_while(|c| *c != '<' && *c != '{' && *c != '(').collect();
            if !base.is_empty() {
                return Some(base);
            }
        }
    }
    None
}

/// V24: Walk up the brace graph from a line to find the enclosing type.
pub fn enclosing_type_from_brace_graph(root: &BraceNode, line: i32) -> Option<String> {
    // Find the deepest node containing this line.
    let enclosing = root.find_enclosing(line)?;
    // Walk up looking for type_def or impl_target.
    let mut current = enclosing;
    loop {
        if let Some(name) = type_name_from_node_text(&current.role, &current.first_line_text) {
            return Some(name);
        }
        // Use existing API to find parent.
        if let Some(parent) = crate::brace_graph::find_parent(root, current) {
            current = parent;
        } else {
            return None;
        }
    }
}

/// V41: Extract function name from the actual source line.
/// Looks for `fn name(`, `fn name<`, `def name(`, `func name(` patterns.
/// Grammar-free — matches any language's function declaration syntax.
fn fn_name_from_line(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    // Find the keyword marker for a function definition.
    let after_kw = if let Some(pos) = trimmed.find("fn ") {
        &trimmed[pos + 3..]
    } else if let Some(pos) = trimmed.find("def ") {
        &trimmed[pos + 4..]
    } else if let Some(pos) = trimmed.find("func ") {
        &trimmed[pos + 5..]
    } else {
        return None;
    };
    // Strip `pub`, `pub(crate)`, `pub(super)`, `async`, `unsafe`, `const`, `static` qualifiers.
    let after_kw = after_kw.trim_start();
    for prefix in &["pub(crate) ", "pub(super) ", "pub ", "async ", "unsafe ", "const ", "static "] {
        if let Some(stripped) = after_kw.strip_prefix(*prefix) {
            return extract_first_ident(stripped);
        }
    }
    extract_first_ident(after_kw)
}

/// V41: Extract the first identifier from a string.
fn extract_first_ident(s: &str) -> Option<String> {
    let ident: String = s.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() { None } else { Some(ident) }
}

/// V24: Derive the full qualified name for a hit.
pub fn derive_qualified_name(
    file_path: &str,
    line: i32,
    meta: &FileMeta,
) -> String {
    // V51: MCP params are 1-indexed; arrays are 0-indexed. Convert.
    let line_0idx = line.max(0) as usize;

    let enclosing_type = enclosing_type_from_brace_graph(&meta.brace_graph, line);

    let fn_name = meta
        .fn_names
        .get(line_0idx)
        .filter(|n| !n.is_empty())
        .cloned()
        // V41: Fallback — extract function name from the actual source line.
        .or_else(|| {
            meta.lines.get(line_0idx).and_then(|l| fn_name_from_line(l))
        });

    let result = match (enclosing_type, fn_name) {
        (Some(t), Some(f)) => format!("{}::{}", t, f),
        (Some(t), None) => format!("{}::*", t),
        (None, Some(f)) => {
            let path_type = type_name_from_path(file_path);
            if !path_type.is_empty() {
                format!("{}::{}", path_type, f)
            } else {
                f
            }
        }
        (None, None) => {
            let path_type = type_name_from_path(file_path);
            if !path_type.is_empty() {
                format!("{}::*", path_type)
            } else {
                String::new()
            }
        }
    };
    // Sanitize: replace newlines/tabs with single space.
    result.replace(['\n', '\r', '\t'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brace_graph::BraceNode;

    fn empty_meta() -> FileMeta {
        FileMeta {
            lines: Vec::new(),
            fn_names: Vec::new(),
            impl_targets: Vec::new(),
            arities: Vec::new(),
            brace_graph: std::sync::Arc::new(BraceNode {
                start_line: 0,
                end_line: 0,
                role: String::new(),
                first_line_text: String::new(),
                children: Vec::new(),
            }),
        }
    }

    #[test]
    fn test_type_name_simple() {
        assert_eq!(type_name_from_path("runtime/runtime.rs"), "Runtime");
        assert_eq!(type_name_from_path("io/util/take.rs"), "Take");
        assert_eq!(type_name_from_path("task/local.rs"), "Local");
    }

    #[test]
    fn test_type_name_snake_case() {
        assert_eq!(type_name_from_path("io/util/buf_writer.rs"), "BufWriter");
        assert_eq!(type_name_from_path("sync/notify_batch.rs"), "NotifyBatch");
    }

    #[test]
    fn test_type_name_mod_rs() {
        assert_eq!(type_name_from_path("runtime/scheduler/mod.rs"), "Scheduler");
    }

    #[test]
    fn test_qualified_name_with_enclosing() {
        let root = BraceNode {
            start_line: 370,
            end_line: 400,
            role: "type_def".into(),
            first_line_text: "struct Runtime".into(),
            children: Vec::new(),
        };
        let meta = FileMeta {
            brace_graph: std::sync::Arc::new(root),
            fn_names: {
                let mut v = vec![String::new(); 400];
                v[375] = "block_on".into();
                v
            },
            ..empty_meta()
        };
        assert_eq!(
            derive_qualified_name("runtime.rs", 375, &meta),
            "Runtime::block_on"
        );
    }

    #[test]
    fn test_qualified_name_fallback_to_path() {
        let meta = FileMeta {
            fn_names: {
                let mut v = vec![String::new(); 100];
                v[18] = "block_on".into();
                v
            },
            ..empty_meta()
        };
        assert_eq!(
            derive_qualified_name("task/local.rs", 18, &meta),
            "Local::block_on"
        );
    }
}
