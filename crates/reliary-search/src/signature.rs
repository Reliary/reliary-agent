//! Phase V+S+T+W: Signature-based call resolution.
//!
//! V: Call-site arity matching — extract arg count from `.stem(args)`.
//!    If candidate arity != anchor arity, DEMOTE (different definition).
//!
//! S: Pattern match variable resolution — `Enum::Variant(v) => v.park()`
//!    resolves `v` to `Enum` type.
//!
//! T: Import-assisted field type — `self.driver` → look up `Driver` in imports.
//!
//! W: Return type consistency — check if def return type matches call usage.

/// Extract the arity (number of comma-separated args) from a call site.
/// `park.park()` → 0. `v.park(handle)` → 1. `self.park(a, b)` → 2.
pub fn call_arity(line_text: &str, stem: &str) -> Option<usize> {
    let needle = format!(".{}", stem);
    if let Some(pos) = line_text.find(&needle) {
        let after = &line_text[pos + needle.len()..];
        if let Some(open) = after.find('(') {
            // Walk to matching close paren.
            let depth_start = open;
            let bytes = after.as_bytes();
            let mut depth: i32 = 0;
            let mut args = 0;
            let mut string_mode = false;
            let mut escape_next = false;

            for (i, &b) in bytes.iter().enumerate().skip(open) {
                if escape_next { escape_next = false; continue; }
                if b == b'\\' && string_mode { escape_next = true; continue; }
                if b == b'"' { string_mode = !string_mode; continue; }
                if string_mode { continue; }

                if b == b'(' {
                    depth += 1;
                    if depth == 1 {
                        // Start of args.
                        // Check if next non-whitespace is ')' (empty args).
                        let rest = &after[i+1..];
                        let trimmed = rest.trim_start();
                        if trimmed.starts_with(')') { return Some(0); }
                        args += 1;
                    }
                } else if b == b')' {
                    depth -= 1;
                    if depth == 0 { return Some(args); }
                } else if b == b',' && depth == 1 {
                    // Start of a new arg (top-level comma).
                    args += 1;
                }
            }
            let _ = depth_start;
            return Some(args);
        }
    }
    // Maybe `Type::stem(args)` form.
    let needle2 = format!("::{}", stem);
    if let Some(pos) = line_text.find(&needle2) {
        let after = &line_text[pos + needle2.len()..];
        if let Some(open) = after.find('(') {
            let bytes = after.as_bytes();
            let mut depth: i32 = 0;
            let mut args = 0;
            let mut string_mode = false;
            for (i, &b) in bytes.iter().enumerate().skip(open) {
                if b == b'"' { string_mode = !string_mode; continue; }
                if string_mode { continue; }
                if b == b'(' {
                    depth += 1;
                    if depth == 1 {
                        let rest = &after[i+1..].trim_start();
                        if rest.starts_with(')') { return Some(0); }
                        args += 1;
                    }
                } else if b == b')' { depth -= 1; if depth == 0 { return Some(args); } }
                else if b == b',' && depth == 1 { args += 1; }
            }
            return Some(args);
        }
    }
    // Bare call: `stem(args)` at start of line or after `=`.
    let bare_needle = format!("{}(", stem);
    if let Some(pos) = line_text.find(&bare_needle) {
        // Make sure stem is not a substring of a larger identifier.
        if pos == 0 || !line_text.as_bytes()[pos - 1].is_ascii_alphanumeric() {
            let after = &line_text[pos + bare_needle.len()..];
            let bytes = after.as_bytes();
            if bytes.is_empty() || bytes[0] == b')' { return Some(0); }
            let mut args = 1;
            let mut depth: i32 = 0;
            for &b in bytes {
                if b == b'(' { depth += 1; }
                else if b == b')' { if depth == 0 { return Some(args); } depth -= 1; }
                else if b == b',' && depth == 0 { args += 1; }
            }
            return Some(args);
        }
    }
    None
}

/// Resolve pattern match variable binding: `Enum::Variant(binding) => binding.method()`.
/// Returns (variable_name, type_name) if found.
pub fn resolve_pattern_match_binding(
    file_lines: &[String],
    line: i32,
    var: &str,
) -> Option<String> {
    // Check the current line and walk up to find the match arm.
    let start = (line as usize).saturating_sub(1);
    for i in (0..=start).rev() {
        if start.saturating_sub(i) > 50 { break; }
        let l = file_lines[i].trim_start();
        // Pattern: Enum::Variant(var) => or Enum::Variant { var, .. } =>
        // Also: Enum::Variant(var) at end of line.
        let pat1 = format!("::{}(", var);  // ::Variant(var)
        let pat2 = format!("{}:", var);   // { var: ... }
        let pat3 = format!("{},", var);   // { var, .. }

        if l.contains(&pat1) || l.contains(&pat2) || l.contains(&pat3) {
            // Find the Enum:: part before this.
            if let Some(colon_pos) = l.rfind("::") {
                let before = &l[..colon_pos];
                // Walk backward to find the Enum name.
                let bytes = before.as_bytes();
                let mut s = bytes.len();
                while s > 0 && (bytes[s-1].is_ascii_alphanumeric() || bytes[s-1] == b'_') {
                    s -= 1;
                }
                let enum_name = &before[s..];
                if !enum_name.is_empty() && enum_name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                    return Some(enum_name.to_string());
                }
            }
        }
    }
    None
}

/// Resolve field type via imports: look for `use ...TypeName` in the file.
/// DEPRECATED: uses Rust-specific keywords. Kept for backward compat only.
#[deprecated(note = "Use brace-graph or DB-based field resolution instead")]
pub fn resolve_field_type_via_imports(
    file_lines: &[String],
    field_name: &str,
) -> Option<String> {
    let capitalized = capitalize(field_name);
    // Walk through use statements.
    for line in file_lines {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("use ") { continue; }
        // Check if the capitalized version appears in the use statement.
        if trimmed.contains(&capitalized) {
            return Some(capitalized);
        }
        // Also check for partial matches: "driver" → "Driver" or "TimeDriver".
        let after_use = trimmed.strip_prefix("use ").unwrap_or(trimmed);
        let cleaned = after_use.trim_end_matches(';').trim();
        // Split by :: and check each segment.
        for segment in cleaned.split("::") {
            let seg = segment.trim().trim_matches('{').trim_matches('}');
            if seg == capitalized || seg.ends_with(&capitalized) {
                return Some(seg.to_string());
            }
            // Pattern: "driver" → "TimeDriver" contains "Driver".
            if seg.to_lowercase().contains(&field_name.to_lowercase())
                && seg.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                return Some(seg.to_string());
            }
        }
    }
    None
}

/// Extract the return type from a function definition line.
/// `fn park(&self, handle: &Handle) -> Box<Core> {` → "Box<Core>"
pub fn extract_return_type(signature_line: &str) -> Option<String> {
    if let Some(pos) = signature_line.find("->") {
        let after = &signature_line[pos + 2..];
        let cut = after.find('{').or_else(|| after.find(" where"))
            .unwrap_or(after.len());
        let rt = after[..cut].trim();
        if !rt.is_empty() && rt.len() < 100 {
            return Some(rt.to_string());
        }
    }
    None
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => {
            let mut result = c.to_uppercase().next().unwrap_or(c).to_string();
            result.push_str(chars.as_str());
            result
        }
        None => String::new(),
    }
}
/// Stem-aware role classifier: classifies the role of a SPECIFIC stem occurrence in a line.
/// This replaces the Python autolabeler's line-wide heuristic with stem-specific detection.
/// (NOT grammar-free — uses Rust keywords.)
///
/// DEPRECATED: Use the DB-based classifier (queries occurrence.tag). This function uses
/// Rust-specific keywords and is not language-agnostic. Kept for backward compat only.
#[deprecated(note = "Use DB-based classifier (grammar-free) instead")]
pub fn classify_occurrence_role(line_text: &str, stem: &str) -> &'static str {
    let line = line_text.trim();
    let needle = format!(".{}", stem);
    let fn_needle = format!("fn {}", stem);
    let struct_needle = format!("struct {}", stem);
    let enum_needle = format!("enum {}", stem);
    let trait_needle = format!("trait {}", stem);
    let type_needle = format!("type {}", stem);

    // 1. Import / use statement (highest priority — uses are unambiguous).
    if line.starts_with("use ") || line.starts_with("pub use ") || line.starts_with("extern crate ") {
        // Check if stem appears as the imported name (last segment of use path).
        // The line is an import — the role is import_or_use regardless of stem position.
        return "import_or_use";
    }

    // 2. Function/method definition: fn NAME
    if line.contains(&fn_needle) || line.contains(&format!("fn {}(", stem)) {
        return "function_def";
    }

    // 3. Struct/enum/trait/type definitions.
    if line.contains(&struct_needle) || line.contains(&enum_needle)
       || line.contains(&trait_needle) || line.contains(&type_needle) {
        return "function_def";  // Definitions group under function_def.
    }

    // 4. impl block (not a function def, but if the stem is the impl target, it's function_def).
    if line.starts_with("impl") && line.contains(stem) {
        return "function_def";
    }

    // 5. Method call: .stem( — the stem is the method being called.
    //    Must be standalone (preceded by `.` and followed by `(`).
    if line.contains(&needle) && has_call_after(line, stem) {
        return "method_call";
    }

    // 6. Method call (bare): stem( at start of expression.
    if bare_call(line, stem) {
        return "method_call";
    }

    // 7. Local variable: let stem = or let mut stem =
    if let_binding(line, stem) {
        return "local_var";
    }

    // 8. Function parameter: (stem: Type) or (self, stem: Type)
    if is_param(line, stem) {
        return "param";
    }

    // 9. Self.stem (field access) — but only if no `(` follows the stem.
    if line.contains(&format!("self.{}", stem)) && !line.contains(&format!(".{} (", stem))
        && !line.contains(&format!(".{}(", stem)) {
        return "field_access";
    }

    // 10. Type annotation: : Type or ::Type
    if line.contains(&format!(": {}", stem)) || line.contains(&format!("::{}", stem)) {
        return "type_name";
    }

    // 11. Module path: crate::, super::, self::
    if line.contains("::") {
        return "module_name";
    }

    // 12. Field-like: stem: Type,
    if is_field_decl(line, stem) {
        return "field_access";
    }

    // Fallback: type_name (conservative — many false positives are type refs).
    "type_name"
}

fn has_call_after(line: &str, stem: &str) -> bool {
    // Find `.stem` and check if `(` follows.
    if let Some(pos) = line.find(&format!(".{}", stem)) {
        let after = &line[pos + 1 + stem.len()..];
        return after.trim_start().starts_with('(') || after.starts_with('(');
    }
    false
}

fn bare_call(line: &str, stem: &str) -> bool {
    // Check for `stem(` at start of expression (not preceded by `.` or `:`).
    let needle = format!("{}(", stem);
    if let Some(pos) = line.find(&needle) {
        // Ensure stem isn't part of a larger identifier.
        if pos > 0 {
            let prev = line.as_bytes()[pos - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' {
                return false;
            }
        }
        // Ensure it's not after a `.` (that's a method call handled above).
        if pos > 0 && line.as_bytes()[pos - 1] == b'.' {
            return false;
        }
        return true;
    }
    false
}

fn let_binding(line: &str, stem: &str) -> bool {
    // let stem = or let mut stem =
    let patterns = [
        format!("let {}[^a-zA-Z0-9_]", stem),  // let stem =, let stem:, etc.
        format!("let mut {}[^a-zA-Z0-9_]", stem),
    ];
    for p in &patterns {
        if line.find(p.as_str()).is_some() {
            return true;
        }
    }
    false
}

fn is_param(line: &str, stem: &str) -> bool {
    // (stem: Type) — stem followed by colon.
    let patterns = [
        format!("{}: ", stem),
        format!("{},", stem),
        format!("{}, ", stem),
        format!("{}:&", stem),
        format!("{}: &", stem),
        format!("{}: &mut ", stem),
    ];
    for p in &patterns {
        if line.contains(p.as_str()) {
            // Confirm it's inside parens (function arg list).
            let pos = line.find(p.as_str()).unwrap();
            let before = &line[..pos];
            if before.contains('(') {
                return true;
            }
        }
    }
    false
}

fn is_field_decl(line: &str, stem: &str) -> bool {
    // `name: Type,` or `pub name: Type,`
    let needle = format!("{}:", stem);
    if line.find(&needle).is_some() {
        // Ensure it's not part of a function signature.
        if line.contains("fn ") { return false; }
        return true;
    }
    false
}

/// Stem-line-text retrieval score (Phase 1).
/// DEPRECATED: Use `is_def` from the DB instead (set by the grammar-free structural
/// detector in `crate::structural`). This function uses Rust-specific keywords and
/// is not language-agnostic. Kept for backward compat only.
#[deprecated(note = "Use is_def from DB (grammar-free) instead")]
///
/// Returns a score in [0.0, 1.0] based on whether the line text contains a
/// DEFINITIVE pattern for the stem. This is information-theoretically optimal
/// for function/type definitions: if a line is `fn poll_write(`, it IS a function
/// definition with certainty.
///
/// Score breakdown:
///   - 1.0: line text contains `fn {stem}(` with word boundary (function_def)
///   - 0.95: line text contains `pub fn {stem}(`, `async fn {stem}(`, etc.
///   - 0.9: line text contains `struct {stem}` or `enum {stem}` (type_def)
///   - 0.85: line text contains `trait {stem}` or `impl.* {stem}` (trait/impl)
///   - 0.7: line text contains `let {stem} =` or `let mut {stem} =` (local_var)
///   - 0.0: no match — fall back to block similarity
pub fn stem_line_text_score(line_text: &str, stem: &str) -> f32 {
    let line = line_text.trim_start();
    // Build word-boundary patterns for the stem.
    let fn_patterns = [
        format!("fn {}(", stem),
        format!("fn {}<", stem),
        format!("fn {} (", stem),
    ];
    let pub_fn_patterns = [
        format!("pub fn {}(", stem),
        format!("pub fn {}<", stem),
        format!("pub async fn {}(", stem),
        format!("pub async fn {}<", stem),
        format!("async fn {}(", stem),
        format!("async fn {}<", stem),
    ];
    let type_patterns = [
        format!("struct {}", stem),
        format!("enum {}", stem),
        format!("pub struct {}", stem),
        format!("pub enum {}", stem),
    ];
    let trait_patterns = [
        format!("trait {}", stem),
        format!("pub trait {}", stem),
        format!("impl {}", stem),  // impl<T> Foo or impl Trait
    ];
    let let_patterns = [
        format!("let {} =", stem),
        format!("let mut {} =", stem),
        format!("let {}:", stem),
        format!("let mut {}:", stem),
    ];

    // Verify the stem is at word boundary (not part of larger identifier).
    let has_word_boundary = |line: &str, pos: usize, _stem: &str| -> bool {
        // Check character before the stem at this position.
        if pos == 0 { return true; }
        let before = line.as_bytes()[pos - 1];
        !before.is_ascii_alphanumeric() && before != b'_'
    };

    // Check function definitions first (highest score).
    for pat in &pub_fn_patterns {
        if let Some(pos) = line.find(pat.as_str()) {
            if has_word_boundary(line, pos, stem) { return 0.95; }
        }
    }
    for pat in &fn_patterns {
        if let Some(pos) = line.find(pat.as_str()) {
            if has_word_boundary(line, pos, stem) { return 1.0; }
        }
    }
    // Type definitions.
    for pat in &type_patterns {
        if let Some(pos) = line.find(pat.as_str()) {
            if has_word_boundary(line, pos, stem) { return 0.90; }
        }
    }
    // Trait / impl patterns.
    for pat in &trait_patterns {
        if let Some(pos) = line.find(pat.as_str()) {
            if has_word_boundary(line, pos, stem) { return 0.85; }
        }
    }
    // Local bindings.
    for pat in &let_patterns {
        if let Some(pos) = line.find(pat.as_str()) {
            if has_word_boundary(line, pos, stem) { return 0.70; }
        }
    }
    0.0
}

/// Extract the receiver token from a `.stem(` call site. Returns empty string
/// if not a method call.
pub fn extract_call_receiver(line_text: &str, stem: &str) -> String {
    let needle = format!(".{}", stem);
    if let Some(pos) = line_text.find(&needle) {
        // Walk backward from pos to find the receiver token.
        let before = &line_text[..pos];
        // Trim trailing whitespace.
        let before_trimmed = before.trim_end();
        // Find last identifier before this position.
        let bytes = before_trimmed.as_bytes();
        let mut start = bytes.len();
        while start > 0 {
            let c = bytes[start - 1];
            if c.is_ascii_alphanumeric() || c == b'_' {
                start -= 1;
            } else {
                break;
            }
        }
        if start < bytes.len() {
            return before_trimmed[start..].to_string();
        }
    }
    String::new()
}
