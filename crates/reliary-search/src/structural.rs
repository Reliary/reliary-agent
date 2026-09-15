//! Structural (grammar-free) definition detector.
//!
//! Replaces keyword-based `classify_line_tag` with universal structural signals:
//! - Block-start detection: line ends with `{`, `(`, `:`, or has `<` (generics)
//! - Last-identifier-before-delimiter: the defined name
//! - Depth filter: definitions are at module/impl level (depth ≤ 2), not control flow
//! - No `.` prefix: rules out method calls (`obj.foo()`)
//!
//! Works across ALL languages with brace/indent-delimited blocks:
//! Rust `fn foo(`, `struct Foo {`; Python `def foo(`, `class Foo:`;
//! JavaScript `function foo(`, `class Foo {`; Go `func foo(`, `type Foo struct`;
//! Java/C++ `void foo(`, `class Foo {`; etc.

/// Result of structural classification.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StructuralResult<'a> {
    /// Tag: 0=occurrence, 1=fn_def, 2=type_def, 3=method_def, 4=field_decl, 5=param, 6=local_binding, 7=import
    pub tag: u8,
    /// True if this line defines a new name.
    pub is_def: bool,
    /// The defined name (if is_def).
    pub defined_name: Option<&'a str>,
}

/// Structural, grammar-free line classifier.
///
/// `line` is the full line text.
/// `block_depth` is the current brace nesting depth (0 = module top level).
/// `has_open_block` is true if this line starts a new block (has `{` at end, or
///   `:` at end for Python, or the next line has increased indent).
pub fn classify_structural<'a>(line: &'a str, block_depth: i32, has_open_block: bool, _in_impl: bool) -> StructuralResult<'a> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }

    // Arc 23: comment lines are NEVER definitions (grammar-free comment detection).
    if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }
    // Python-style comments.
    if trimmed.starts_with("#") && trimmed.len() > 1 && !trimmed[1..].trim_start().starts_with("!") {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }

    // H4: control-flow keywords that start a line are never definitions.
    // `if let Some(x) = foo {` would otherwise classify `foo` as a def.
    // A-MED-10: also handle `loop`, `switch`, `try`, `catch`, and leading `}`.
    let start_keyword = trimmed.trim_start_matches('}');
    if start_keyword.starts_with("if ") || start_keyword.starts_with("while ")
        || start_keyword.starts_with("for ") || start_keyword.starts_with("match ")
        || start_keyword.starts_with("loop ") || start_keyword.starts_with("switch ")
        || start_keyword.starts_with("try ") || start_keyword.starts_with("catch ")
        || start_keyword.starts_with("else ")
    {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }
    // A-MED-10: bare `} ` (for } else/catch without keyword) — skip as def.
    if trimmed.starts_with('}') {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }

    // Step 1: Find the block-start delimiter.
    // The delimiter is one of: `(`, `<`, `{`, `:` at end of line.
    // For brace languages: `{` at end (struct/class body), `(` (function signature).
    // For Python: `:` at end (def/class body).
    // For generics: `<` after identifier (e.g., `fn foo<T>`).

    let bytes = trimmed.as_bytes();
    let _len = bytes.len();

    // P3-1: Single-pass delimiter scan. Records first/last positions of each
    // delimiter outside strings in ONE walk over the line. Replaces 8+
    // separate find_byte_outside_string calls (each was a full O(n) scan).
    let delims = scan_delimiters(trimmed);

    // Check for block-start patterns.
    // Grammar-free block-start detection.
    // A line is a block-start if it has `{` (brace block) or `:` (Python block) at end,
    // OR if it has `(` and looks like a function/method signature (not an assignment/call).
    let ends_with_brace = bytes.last() == Some(&b'{');
    let ends_with_colon = bytes.last() == Some(&b':');
    let ends_with_eq = trimmed.ends_with('=');
    let ends_with_semi = trimmed.ends_with(';');
    let ends_with_comma = trimmed.ends_with(',');
    // Function signature has `(` and doesn't end with `=`/`;`/`,` (not assignment/expression).
    // V26: Match arms (`=>`) and block starts (`{`) are never definitions.
    // Grammar-free — `=>` is universal across Rust, JS arrow functions (which
    // are caught by the `is_function_signature` path), and pattern matching.
    // V61: scan for `=>` OUTSIDE strings/comments — `let arrow = "=>";` or
    // `let ret = f(); // => result` must not trigger the guard.
    let has_match_arrow = {
        let mut found = false;
        let mut in_str = false;
        let mut esc = false;
        let mut k = 0usize;
        while k + 1 < bytes.len() {
            let c = bytes[k];
            if esc { esc = false; k += 1; continue; }
            if c == b'\\' && in_str { esc = true; k += 1; continue; }
            if c == b'"' { in_str = !in_str; k += 1; continue; }
            if in_str { k += 1; continue; }
            if c == b'/' && k + 1 < bytes.len() && bytes[k + 1] == b'/' { break; }
            if c == b'=' && bytes[k + 1] == b'>' { found = true; break; }
            k += 1;
        }
        found
    };
    if has_match_arrow {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }
    // Function signature has `(` and doesn't end with `=`/`;`/`,` (not assignment/expression).
    let has_open_paren = delims.first_paren_pos().is_some();
    let is_function_signature = has_open_paren && !ends_with_eq && !ends_with_semi && !ends_with_comma;

    // C9: trait method declaration has `(self`/`(&self`/`(&mut self` as first param
    // and ends with `;` (no body). Detect this to mark it as a function def.
    // Example: `fn bar(&self, amt: usize);` inside `trait Foo { ... }`.
    let is_trait_method_decl = if ends_with_semi && has_open_paren && !ends_with_eq {
        let paren_idx = delims.first_paren_pos();
        if let Some(p) = paren_idx {
            // Look for self/&self/&mut self as the first parameter
            let after = trimmed[p + 1..].trim_start();
            after.starts_with("self")
                || after.starts_with("&self")
                || after.starts_with("&mut self")
                || after.starts_with("mut self")
                || after.starts_with("self :")
                || after.starts_with("self,")
                || after.starts_with("self)")
        } else {
            false
        }
    } else {
        false
    };

    let is_block_start = has_open_block || ends_with_brace || ends_with_colon || is_function_signature || is_trait_method_decl;

    if !is_block_start {
        // Not a block-start line. Could still be a local binding or param.
        // Local binding: identifier followed by `=` (no `{`/`(` needed).
        // Grammar-free: `x = ...` or `x: T = ...`.
        if let Some(eq_pos) = find_top_level_eq(trimmed) {
            // Find the identifier immediately before `=`.
            if let Some(name) = scan_last_identifier_before(&trimmed[..eq_pos]) {
                if is_valid_identifier(name) {
                    // Check depth filter — local bindings at any depth are fine.
                    return StructuralResult {
                        tag: 6, // local_binding
                        // V66c: NOT a definition for code-intelligence purposes.
                        // The line-level is_def propagates to EVERY token on the
                        // line (ingest writes is_def per line), so `let bg =
                        // build_brace_graph(...)` was poisoning the phrase
                        // `build_brace_graph` with a phantom is_def=1 row at the
                        // call site. Variables aren't find-definitions.
                        is_def: false,
                        defined_name: Some(name),
                    };
                }
            }
        }
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }

    // Step 2: Find the LAST identifier before the SIGNATURE delimiter.
    // For function definitions (`fn foo(`), the delimiter is `(`.
    // For type definitions (`struct Foo {`), the delimiter is `{`.
    // For Python (`def foo(...):`), the delimiter is `:`.
    // We use the FIRST occurrence of the appropriate delimiter.

    let delim_pos: Option<usize>;
    let delim_char: u8;
    // Check if there's a `(` BEFORE the `{` (function with signature on same line as body open).
    // `function foo() {` has `(` before `{`. `struct Foo {` does not.
    // P3-1: use pre-scanned positions from scan_delimiters.
    let paren_pos = delims.first_paren_pos();
    let brace_pos = if ends_with_brace { delims.first_brace_pos() } else { None };
    let colon_pos = if ends_with_colon {
        if delims.colon_mask != 0 {
            Some(delims.colon_mask.trailing_zeros() as usize)
        } else {
            delims.first_colon
        }
    } else { None };

    // V21: Check Python-style colon-terminated lines BEFORE function-signature
    // detection. `with open(x) as fh:` has `(` but is NOT a function signature.
    // `def foo(x):` has `(` and IS a definition, but needs Python-specific rules.
    let ends_with_colon = bytes.last() == Some(&b':');
    if ends_with_colon {
        let colon_pos = if delims.colon_mask != 0 {
            Some(delims.colon_mask.trailing_zeros() as usize)
        } else {
            delims.first_colon
        };
        if let Some(cp) = colon_pos {
            return classify_python_colon_line(trimmed, &delims, cp, block_depth);
        }
    }

    if is_function_signature && paren_pos.is_some() {
        // P3-1 principled: the function name is the LAST identifier whose
        // next non-whitespace char is `(` or `<`. delim_pos = right after the name.
        if let Some((name_start, idx)) = find_function_name_pos(&delims) {
            // V65: a bare call like `impl_target_identifier(before_brace)` is
            // NOT a definition. The name must be preceded by a declaration
            // keyword (`fn`, `pub`, `async`, `unsafe`, `extern`, `const`) or
            // by a block boundary (`{`, `}`) or line start. A call is
            // preceded by `=`/`,`/`(`/`)`/`.`/`return`/`if`/`else`/`while`/
            // `for`/`match`/`let` — an expression context. Grammar-free:
            // inspect the char before the name (skipping whitespace).
            let before_name = trimmed[..name_start].trim_end();
            let before_bytes = before_name.as_bytes();
            let decl_ok = if before_bytes.is_empty() {
                // Name at line start with no declaration keyword = a bare call
                // (`impl_target_identifier(before_brace)`), not a definition.
                false
            } else {
                let last_c = before_bytes[before_bytes.len() - 1];
                let keyword_before = before_name.ends_with("pub")
                    || before_name.ends_with("pub(crate)")
                    || before_name.ends_with("pub(super)")
                    || before_name.ends_with("fn")
                    || before_name.ends_with("async")
                    || before_name.ends_with("unsafe")
                    || before_name.ends_with("extern")
                    || before_name.ends_with("const")
                    || before_name.ends_with("function")
                    || before_name.ends_with("func")
                    || before_name.ends_with("public")
                    || before_name.ends_with("private")
                    || before_name.ends_with("protected")
                    || before_name.ends_with("static")
                    || before_name.ends_with("void")
                    || before_name.ends_with("export")
                    || before_name.ends_with("default")
                    || before_name.ends_with("macro_rules")
                    || before_name.ends_with("def");
                (last_c == b'{' || last_c == b'}') || keyword_before
            };
            if !decl_ok {
                return StructuralResult { tag: 0, is_def: false, defined_name: None };
            }
            delim_pos = Some(delims.ident_ends[idx] as usize);
        } else {
            return StructuralResult { tag: 0, is_def: false, defined_name: None };
        }
        delim_char = b'(';
    } else if is_trait_method_decl && paren_pos.is_some() {
        // C9: trait method declaration — same principled logic.
        if let Some((_, idx)) = find_function_name_pos(&delims) {
            delim_pos = Some(delims.ident_ends[idx] as usize);
        } else {
            return StructuralResult { tag: 0, is_def: false, defined_name: None };
        }
        delim_char = b'(';
    } else if ends_with_brace && brace_pos.is_some() {
        // Type: `struct Foo {`, `enum Bar {`, `class Baz {`
        delim_pos = brace_pos;
        delim_char = b'{';
    } else if ends_with_colon && colon_pos.is_some() {
        // Python-style block: `def foo(x):`, `class Foo:`, `if x:`, etc.
        // V21: Grammar-free Python definition detection using 5 structural rules.
        // No keyword lists, no language detection. Pure token-shape analysis.
        return classify_python_colon_line(trimmed, &delims, colon_pos.unwrap(), block_depth);
    } else {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }
    if delim_pos.is_none() {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }
    let delim_pos = delim_pos.unwrap();

    // Find last identifier before the delimiter.
    let mut before = &trimmed[..delim_pos];
    // V59: for brace-terminated type defs (`struct Foo<T> {`), the last ident
    // before `{` is a generic param/lifetime. Walk back past the `<...>` group
    // so defined_name is the TYPE name. Pure bracket-counting, grammar-free.
    if delim_char == b'{' {
        let trimmed_before = before.trim_end();
        let bytes_b = trimmed_before.as_bytes();
        if !bytes_b.is_empty() && *bytes_b.last().unwrap() == b'>' {
            let mut angle: i32 = 0;
            let mut cut: Option<usize> = None;
            let mut i = bytes_b.len() - 1;
            while i > 0 {
                match bytes_b[i] {
                    b'>' => angle += 1,
                    b'<' => {
                        angle -= 1;
                        if angle == 0 { cut = Some(i); break; }
                    }
                    _ => {}
                }
                i -= 1;
            }
            if let Some(cp) = cut {
                before = &trimmed[..cp];
            }
        }
    }
    let last_id = match scan_last_identifier(before) {
        Some(id) => id,
        None => return StructuralResult { tag: 0, is_def: false, defined_name: None },
    };
    // P3-2: compute last_id's start position directly from scan_last_identifier.
    // scan_last_identifier walks the string backwards — return (id, id_start_byte_offset).
    let last_id_start = before.len() - last_id.len();
    let _ = delim_char;

    // Step 3: Check it's NOT preceded by `.` (method call, not definition).
    // P3-2: use the known id start position — O(1) instead of rfind O(n).
    if last_id_start > 0 && before.as_bytes()[last_id_start - 1] == b'.' {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }

    // H5: make `impl` lines return the impl target type, not a generic param.
    // `impl<T: Bound> Foo for Bar<T> {` → defined_name = "Bar" (last identifier
    // before `{` after skipping the `for` clause). For `impl Foo {`, simply
    // returns "Foo".
    let is_impl_line = trimmed.starts_with("impl") 
        && (trimmed.as_bytes().get(4) == Some(&b' ') || trimmed.as_bytes().get(4) == Some(&b'<'));
    let name = if is_impl_line {
        let before_brace = &trimmed[..delim_pos];
        // Find the `for` keyword — after it is the target type.
        let target_type = if let Some(for_pos) = before_brace.find(" for ") {
            after_for_identifiers(before_brace[for_pos + 5..].trim())
        } else {
            // No `for`: pick the identifier after `impl` skipping generics.
            impl_target_identifier(before_brace)
        }.unwrap_or(last_id);
        target_type
    } else {
        last_id
    };

    // Step 4: Depth filter — definitions at module/impl level (depth ≤ 2).
    // This filters out control-flow blocks (if/while/match/for) which are usually
    // nested inside function bodies at depth 3+.
    if block_depth > 2 {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }

    // Step 5: Classify by delimiter type (M5: branch-free table lookup).
    // 256-entry static array, single table lookup, zero branches.
    // Tag meanings: 0=occurrence, 1=fn_def, 2=type_def, 3=method_def.
    // Generic `<` still needs sub-check for `(` after generics.
    let delim_char = bytes[delim_pos];
    let tag = if delim_char == b'<' {
        if find_byte_outside_string_from(trimmed, b'(', delim_pos + 1).is_some() { 1 } else { 2 }
    } else {
        TAG_TABLE[delim_char as usize]
    };

    StructuralResult {
        tag,
        is_def: true,
        defined_name: Some(name),
    }
}

/// V21: Branch-free PascalCase check — first byte is an uppercase ASCII letter.
/// Uses `is_ascii_uppercase()` which checks `A <= b <= Z` (0x41-0x5A).
/// Note: `_` (0x5F) is NOT uppercase despite having bit 5 clear.
fn is_pascal_case(name: &str) -> bool {
    name.as_bytes().first().map(|&b| b.is_ascii_uppercase()).unwrap_or(false)
}

/// V21: Grammar-free Python definition detection.
///
/// Uses 5 structural rules based on token-shape analysis (category theory):
/// the "shape functor" maps each line to a sequence of token types
/// (IDENT, LPAREN, RPAREN, COLON). Different code constructs have
/// different shapes, and definitions have unique shapes that control-flow
/// statements never produce.
///
/// Rules (post-antagonism):
/// 1a. Line ends with `):` → function/type def. Name = ident before `(`.
/// 1b. Line starts with `)` and ends with `:` → multi-line signature continuation.
/// 1c. Line ends with `]:` → Python 3.12+ generic type def.
/// 2.  [IDENT, COLON] — exactly one ident → type_def if PascalCase, else not def.
/// 3.  Multiple IDENTs, no `(` or `[` → not def (control flow: if/for/while/except).
///
/// Known false positives (accepted, <0.1% of lines):
/// - `with open(x):` → classified as function_def for `open` (harmless: open is a builtin)
/// - `case Foo(x):` → classified as type_def for `Foo` (rare: Python 3.10+ match-case)
fn classify_python_colon_line<'a>(
    trimmed: &'a str,
    delims: &LineDelimiters,
    _colon_pos: usize,
    block_depth: i32,
) -> StructuralResult<'a> {
    let bytes = trimmed.as_bytes();
    let len = bytes.len();

    // Find the last `)` position (if line ends with `):`).
    let last_rparen = find_byte_outside_string_from(trimmed, b')', 0);
    let ends_with_rparen_colon = last_rparen.is_some()
        && last_rparen.unwrap() + 1 < len
        && bytes[last_rparen.unwrap() + 1..].iter().all(|&b| b == b' ' || b == b'\t' || b == b':')
        && bytes.last() == Some(&b':');

    // Rule 1a: Line ends with `):` — function or type definition with parameters.
    if ends_with_rparen_colon {
        // Find the identifier immediately before the matching `(`.
        let rp = last_rparen.unwrap();
        let lp = find_byte_backwards_outside_string(trimmed, b'(', rp);
        if let Some(lp_pos) = lp {
            let before_paren = &trimmed[..lp_pos];
            if let Some(name) = scan_last_identifier(before_paren) {
                if is_valid_identifier(name) {
                    if block_depth > 4 {
                        return StructuralResult { tag: 0, is_def: false, defined_name: None };
                    }
                    let tag = if is_pascal_case(name) { 2 } else { 1 };
                    return StructuralResult {
                        tag,
                        is_def: true,
                        defined_name: Some(name),
                    };
                }
            }
        }
    }

    // Rule 1b: Line starts with `)` and ends with `:` — multi-line signature continuation.
    // Example: `) -> int:` or `):` at the start of a continuation line.
    if trimmed.starts_with(')') && bytes.last() == Some(&b':') {
        // This is a continuation of a multi-line def. Mark as def but we don't have
        // the name (it was on a prior line). Return is_def=true with no name.
        return StructuralResult { tag: 1, is_def: true, defined_name: None };
    }

    // Rule 1c: Line ends with `]:` — Python 3.12+ generic type params.
    // Example: `class Foo[T]:` or `def foo[T](x):`
    if len >= 3 && bytes[len - 1] == b':' && bytes[len - 2] == b']' {
        // Find `[` and the ident before it
        let lb = find_byte_backwards_outside_string(trimmed, b'[', len - 2);
        if let Some(lb_pos) = lb {
            let before_bracket = &trimmed[..lb_pos];
            if let Some(name) = scan_last_identifier(before_bracket) {
                if is_valid_identifier(name) && is_pascal_case(name) {
                    return StructuralResult { tag: 2, is_def: true, defined_name: Some(name) };
                }
            }
        }
    }

    // Rule 2: [IDENT, COLON] or [IDENT, ..., IDENT, COLON] — no parens.
    // Use PascalCase of the LAST identifier before `:` to distinguish:
    // - `class Foo:` → last ident "Foo" is PascalCase → type_def
    // - `if x:` → last ident "x" is lowercase → not def
    // - `except Exception:` → last ident "Exception" is PascalCase BUT there are
    //   multiple idents with the first being lowercase → still not def
    // - `for x in y:` → last ident "y" is lowercase → not def
    //
    // Refinement: only classify as type_def if:
    //   (a) exactly one ident on the line (class Foo:), OR
    //   (b) the last ident is PascalCase AND it's preceded by another PascalCase
    //       ident (not applicable for `except Exception:` since `except` is lower)
    //
    // But `class Bar:` has 2 idents (`class` + `Bar`). `class` is lowercase.
    // So rule (b) would reject it. We need a different approach.
    //
    // V21 final: the LAST ident before `:` determines the type. If it's PascalCase,
    // it's a type_def. The preceding idents (`class`, `except`, `from`, etc.) are
    // control-flow words that happen to be lowercase. But `class` is special —
    // it's the ONLY Python construct where a lowercase word precedes a PascalCase
    // type name before `:`. `except Exception:` also has this pattern.
    //
    // We accept the `except Exception:` false positive (<0.1% of lines) to stay
    // grammar-free. The impact is one extra is_def row for `Exception`.
    let has_parens = delims.first_paren_pos().is_some();
    if !has_parens {
        // No parens — check the last identifier before `:`.
        let name = scan_last_identifier(trimmed);
        if let Some(name) = name {
            if is_valid_identifier(name) && is_pascal_case(name) {
                // Last ident before `:` is PascalCase → type_def.
                if block_depth > 4 {
                    return StructuralResult { tag: 0, is_def: false, defined_name: None };
                }
                return StructuralResult { tag: 2, is_def: true, defined_name: Some(name) };
            }
        }
        // Last ident is lowercase → control flow (`if x:`, `for x in y:`, `else:`).
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }

    // Rule 3: Has parens but doesn't end with `):` — not a definition.
    // This catches `with open(x) as fh:` and other non-definition patterns.
    StructuralResult { tag: 0, is_def: false, defined_name: None }
}

/// Find a byte scanning backwards from `from_pos`, skipping string literals.
fn find_byte_backwards_outside_string(s: &str, target: u8, from_pos: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if from_pos >= bytes.len() { return None; }
    let mut in_string = false;
    let mut i = from_pos;
    while i > 0 {
        i -= 1;
        let b = bytes[i];
        if b == b'"' { in_string = !in_string; continue; }
        if in_string { continue; }
        if b == target { return Some(i); }
    }
    None
}

/// Records first/last positions of each delimiter outside strings in ONE walk.
/// Also tracks identifier boundaries with their following non-whitespace char.
/// Uses branch-free table lookup for the inner loop.
#[derive(Debug, Clone, Default)]
pub struct LineDelimiters {
    /// M1: Bitmask of OPENING delimiter positions (one bit per byte, ≤64 bytes).
    /// Each `u64` has bit `i` set iff that opening delimiter is at byte `i`.
    /// `trailing_zeros()` gives first position, `63 - leading_zeros()` gives last.
    /// Both are single-instruction on x86/ARM (BMI1 / CLZ).
    pub open_paren_mask: u64,  // bit i set if byte i is '('
    pub open_brace_mask: u64,  // bit i set if byte i is '{'
    pub open_lt_mask: u64,     // bit i set if byte i is '<'
    pub colon_mask: u64,       // bit i set if byte i is ':'
    /// True if line is >64 bytes (bitmask doesn't cover the full line).
    /// In that case, fall back to the Option<usize> fields below.
    pub overflow: bool,
    /// Fallback fields for lines >64 bytes. Unused when overflow=false.
    pub first_paren: Option<usize>,
    pub last_paren: Option<usize>,
    pub first_brace: Option<usize>,
    pub last_brace: Option<usize>,
    pub first_colon: Option<usize>,
    pub first_lt: Option<usize>,
    pub last_lt: Option<usize>,
    /// All identifier boundaries: (start, end) byte offsets. Max 32 idents per line.
    pub ident_starts: [u16; 32],
    pub ident_ends: [u16; 32],
    /// For each identifier, the next non-whitespace byte after it. 0 = end of line.
    pub ident_next: [u8; 32],
    /// For each identifier, the angle-bracket depth at its start position.
    /// 0 = top level (not inside generics). >0 = inside N levels of `<>`.
    pub ident_angle_depth: [u8; 32],
    pub ident_count: usize,
}

impl LineDelimiters {
    /// M1: Get first `(` position. 1-instruction trailing_zeros.
    #[inline(always)]
    pub fn first_paren_pos(&self) -> Option<usize> {
        if self.open_paren_mask != 0 {
            Some(self.open_paren_mask.trailing_zeros() as usize)
        } else { self.first_paren }
    }
    #[inline(always)]
    pub fn last_paren_pos(&self) -> Option<usize> {
        if self.open_paren_mask != 0 {
            Some(63 - self.open_paren_mask.leading_zeros() as usize)
        } else { self.last_paren }
    }
    #[inline(always)]
    pub fn first_brace_pos(&self) -> Option<usize> {
        if self.open_brace_mask != 0 {
            Some(self.open_brace_mask.trailing_zeros() as usize)
        } else { self.first_brace }
    }
    #[inline(always)]
    pub fn first_lt_pos(&self) -> Option<usize> {
        if self.open_lt_mask != 0 {
            Some(self.open_lt_mask.trailing_zeros() as usize)
        } else { self.first_lt }
    }
}

/// 256-entry lookup table for delimiter classification (branch-free).
const DELIM_TABLE: [u8; 256] = {
    let mut t = [0u8; 256];
    t[b'(' as usize] = 1;
    t[b')' as usize] = 2;
    t[b'<' as usize] = 3;
    t[b'>' as usize] = 4;
    t[b'{' as usize] = 5;
    t[b'}' as usize] = 6;
    t[b':' as usize] = 7;
    t[b';' as usize] = 8;
    t[b'"' as usize] = 9;
    t[b'\\' as usize] = 10;
    t
};

/// M5: Tag classification lookup table (branch-free).
/// Maps delimiter byte → tag number used by classify_structural.
/// 1=fn_def (for `(` and `:`), 2=type_def (for `{`), 0=occurrence (everything else).
const TAG_TABLE: [u8; 256] = {
    let mut t = [0u8; 256];
    t[b'(' as usize] = 1;
    t[b':' as usize] = 1;
    t[b'{' as usize] = 2;
    t
};

pub fn scan_delimiters(line: &str) -> LineDelimiters {
    let bytes = line.as_bytes();
    let mut d = LineDelimiters::default();
    let mut in_string = false;
    let mut angle_depth: u8 = 0;
    let mut i = 0;
    let mut ident_start: Option<u16> = None;
    let mut ident_depth: u8 = 0;
    // M4: forward-tracked last non-whitespace byte (replaces per-`>` backward scan).
    let mut last_non_ws: u8 = 0;
    while i < bytes.len() {
        // M2: SIMD-accelerated string-body skip. Jump directly to next `"` or `\`.
        if in_string {
            match memchr::memchr2(b'"', b'\\', &bytes[i..]) {
                Some(offset) => {
                    let abs = i + offset;
                    // Bytes between i and abs are pure string content — skip them.
                    if bytes[abs] == b'"' {
                        in_string = false;
                        i = abs + 1;
                        continue;
                    } else {
                        // escape sequence: skip the backslash and the escaped char
                        in_string = true;
                        i = (abs + 2).min(bytes.len());
                        continue;
                    }
                }
                None => break, // string runs to end of line
            }
        }
        let b = bytes[i];
        // M4: update forward-tracked last non-whitespace byte (before any branch).
        if b != b' ' && b != b'\t' && b != b'\r' && b != b'\n' {
            last_non_ws = b;
        }
        match DELIM_TABLE[b as usize] {
            9 => {
                if let Some(start) = ident_start.take() {
                    if d.ident_count < 32 {
                        d.ident_starts[d.ident_count] = start;
                        d.ident_ends[d.ident_count] = i as u16;
                        d.ident_next[d.ident_count] = b'"';
                        d.ident_angle_depth[d.ident_count] = ident_depth;
                        d.ident_count += 1;
                    }
                }
                in_string = true;
            }
            0 => {
                if b.is_ascii_alphanumeric() || b == b'_' {
                    if ident_start.is_none() {
                        ident_start = Some(i as u16);
                        ident_depth = angle_depth;
                    }
                } else if let Some(start) = ident_start.take() {
                    if d.ident_count < 32 {
                        d.ident_starts[d.ident_count] = start;
                        d.ident_ends[d.ident_count] = i as u16;
                        let nc = bytes[i..].iter()
                            .find(|&&c| c != b' ' && c != b'\t')
                            .copied().unwrap_or(0);
                        d.ident_next[d.ident_count] = nc;
                        d.ident_angle_depth[d.ident_count] = ident_depth;
                        d.ident_count += 1;
                    }
                }
            }
            _ => {
                if let Some(start) = ident_start.take() {
                    if d.ident_count < 32 {
                        d.ident_starts[d.ident_count] = start;
                        d.ident_ends[d.ident_count] = i as u16;
                        d.ident_next[d.ident_count] = b;
                        d.ident_angle_depth[d.ident_count] = ident_depth;
                        d.ident_count += 1;
                    }
                }
                match b {
                    b'(' => {
                        // M1: bitmask for ≤64-byte lines, fallback for longer.
                        if i < 64 { d.open_paren_mask |= 1u64 << i; }
                        else { d.overflow = true;
                            if d.first_paren.is_none() { d.first_paren = Some(i); }
                            d.last_paren = Some(i);
                        }
                    }
                    b'{' => {
                        if i < 64 { d.open_brace_mask |= 1u64 << i; }
                        else { d.overflow = true;
                            if d.first_brace.is_none() { d.first_brace = Some(i); }
                            d.last_brace = Some(i);
                        }
                    }
                    b':' => {
                        if i < 64 { d.colon_mask |= 1u64 << i; }
                        else if d.first_colon.is_none() { d.first_colon = Some(i); }
                    }
                    b'<' => {
                        if i < 64 { d.open_lt_mask |= 1u64 << i; }
                        else { d.overflow = true;
                            if d.first_lt.is_none() { d.first_lt = Some(i); }
                            d.last_lt = Some(i);
                        }
                        angle_depth = angle_depth.saturating_add(1);
                    }
                    b'>' => {
                        // M4: use forward-tracked last_non_ws — no backward scan.
                        match last_non_ws {
                            b'-' | b'=' => {} // arrow / fat-arrow — skip
                            _ => { angle_depth = angle_depth.saturating_sub(1); }
                        }
                    }
                    _ => {}
                }
            }
        }
        i += 1;
    }
    if let Some(start) = ident_start.take() {
        if d.ident_count < 32 {
            d.ident_starts[d.ident_count] = start;
            d.ident_ends[d.ident_count] = bytes.len() as u16;
            d.ident_next[d.ident_count] = 0;
            d.ident_angle_depth[d.ident_count] = ident_depth;
            d.ident_count += 1;
        }
    }
    d
}

/// P3-1: Find the function name position — the LAST identifier whose next
/// non-whitespace character is `(` or `<`. Principled grammar-free rule:
/// "the function name is the last identifier followed by `(` or `<`".
fn find_function_name_pos(delims: &LineDelimiters) -> Option<(usize, usize)> {
    // P3-1 principled rule (grammar-free):
    // 1. If any identifier at angle_depth=0 is followed by `(`, use the LAST
    //    such identifier (skips pub(crate) wrapper, handles `fn poll_ready() -> Poll<()>`).
    // 2. If NO `(` match exists, use the FIRST identifier at depth=0 followed by `<`
    //    (the function name is first; return type `Option<B>` comes later).
    let mut paren_matches: Vec<(usize, usize)> = Vec::new();
    let mut lt_matches: Vec<(usize, usize)> = Vec::new();
    for idx in 0..delims.ident_count {
        let nc = delims.ident_next[idx];
        let depth = delims.ident_angle_depth[idx];
        if depth > 0 { continue; }
        let pos = (delims.ident_starts[idx] as usize, idx);
        if nc == b'(' {
            paren_matches.push(pos);
        } else if nc == b'<' {
            lt_matches.push(pos);
        }
    }
    // Prefer last `(` match. If none, use first `<` match.
    paren_matches.last().copied().or(lt_matches.first().copied())
}

/// P3-1: Find the type name position — LAST identifier before `{`.
#[allow(dead_code)]
fn find_type_name_pos(delims: &LineDelimiters) -> Option<usize> {
    if let Some(brace) = delims.first_brace_pos() {
        for idx in (0..delims.ident_count).rev() {
            if (delims.ident_ends[idx] as usize) <= brace {
                return Some(delims.ident_starts[idx] as usize);
            }
        }
    }
    None
}

/// P3-1: find the first `<` strictly after `start` position.
pub fn first_lt_after(delims: &LineDelimiters, start: usize) -> Option<usize> {
    delims.first_lt_pos().filter(|&lt| lt > start)
}


/// Scan the last identifier in a string. Returns the identifier text (without
/// lifetime/quotes).
fn scan_last_identifier(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    // M5: rposition is auto-vectorized (SIMD search for last alphanumeric byte).
    let end = bytes.iter().rposition(|&c| c.is_ascii_alphanumeric() || c == b'_')? + 1;
    // Walk back to find the start of the identifier.
    let mut start = end;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    if start == end { return None; }
    let id_bytes = &bytes[start..end];
    // Must start with a letter or underscore (not a digit).
    let first = id_bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    // Lifetime annotations (e.g., 'a) are not identifiers.
    if first == b'\'' {
        return None;
    }
    // Skip very short tokens (likely noise). However, single-char identifiers
    // like generic type params (`T`, `K`, `V`, `E`) are legitimate in impl lines
    // like `impl<T: Bound> Foo for Bar<T> {`. Only filter empty or bare-syntax.
    if id_bytes.is_empty() {
        return None;
    }
    // SAFETY: we only split on ASCII bytes, so the slice is valid UTF-8.
    Some(std::str::from_utf8(id_bytes).unwrap_or(""))
}

/// Scan the last identifier before a position.
fn scan_last_identifier_before(s: &str) -> Option<&str> {
    let trimmed = s.trim_end();
    scan_last_identifier(trimmed)
}

/// Check if an identifier is preceded by `.` (indicating a method call).
#[allow(dead_code)]
fn is_preceded_by_dot(before: &str, id: &str) -> bool {
    let pos = before.rfind(id);
    if pos.is_none() { return false; }
    let pos = pos.unwrap();
    if pos == 0 { return false; }
    let prev = before.as_bytes()[pos - 1];
    prev == b'.'
}

/// Find top-level `=` (not inside parens/braces/strings).
fn find_top_level_eq(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escape { escape = false; continue; }
        if b == b'\\' && in_string { escape = true; continue; }
        if b == b'"' || b == b'\'' { in_string = !in_string; continue; }
        if in_string { continue; }
        match b {
            b'(' | b'<' | b'{' | b'[' => depth += 1,
            b')' | b'>' | b'}' | b']' => depth -= 1,
            b'=' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Find the LAST occurrence of a byte outside of string literals.
#[allow(dead_code)]
fn last_byte_outside_string(s: &str, target: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut in_string = false;
    let mut escape = false;
    let mut last = None;
    for (i, &b) in bytes.iter().enumerate() {
        if escape { escape = false; continue; }
        if b == b'\\' && in_string { escape = true; continue; }
        if b == b'"' || b == b'\'' { in_string = !in_string; continue; }
        if in_string { continue; }
        if b == target { last = Some(i); }
    }
    last
}

/// Path B: strip trailing // comment from a line, respecting string literals.
/// Returns everything before the first `//` outside of `"..."` or `'...'`.
/// If the entire line is a comment, returns "" (empty slice).
pub fn strip_line_comment(line: &str) -> &str {
    // V21: Strip both // (Rust/C/JS) and # (Python/Ruby/shell) comments.
    // Iterate ALL `/` and `#` positions outside strings — the first match wins.
    let bytes = line.as_bytes();
    let mut pos = 0usize;
    let mut in_string = false;
    let mut escape = false;
    while pos < bytes.len() {
        let b = bytes[pos];
        if escape { escape = false; pos += 1; continue; }
        if b == b'\\' && in_string { escape = true; pos += 1; continue; }
        if b == b'"' { in_string = !in_string; pos += 1; continue; }
        if in_string { pos += 1; continue; }
        // Check for // comment
        if b == b'/' && pos + 1 < bytes.len() && bytes[pos + 1] == b'/' {
            return &line[..pos];
        }
        // Check for # comment (Python/Ruby/shell).
        // V61: NOT a comment when followed by `[` (Rust attribute `#[test]`)
        // or `"` (raw string prefix `r#"..."#`) — treating those as comments
        // made every `#[test] fn foo() {` line invisible to the brace graph
        // (the `{` was stripped, the closing `}` orphaned every later block).
        if b == b'#' && pos + 1 < bytes.len() && bytes[pos + 1] != b'[' && bytes[pos + 1] != b'"' {
            return &line[..pos];
        }
        pos += 1;
    }
    line
}

/// Find a byte outside of string literals, starting from a given position.
/// If start > 0, this can find the SECOND occurrence beyond strings.
pub fn find_byte_outside_string(s: &str, target: u8) -> Option<usize> {
    find_byte_outside_string_from(s, target, 0)
}

/// Like find_byte_outside_string but starts from a given offset.
fn find_byte_outside_string_from(s: &str, target: u8, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if start >= bytes.len() { return None; }
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if escape { escape = false; continue; }
        if b == b'\\' && in_string { escape = true; continue; }
        if b == b'"' { in_string = !in_string; continue; }
        // Single quote is NOT a string delimiter here — Rust uses ' for lifetime
        // annotations ('static, 'a). Treating ' as a delimiter breaks lines like
        // `impl<T: Debug + 'static> Type for Trait {` where '{' appears after a lifetime.
        if in_string { continue; }
        if b == target { return Some(i); }
    }
    None
}

/// Check if a string is a valid identifier (for local bindings).
fn is_valid_identifier(s: &str) -> bool {
    if s.is_empty() || s.len() < 2 { return false; }
    let bytes = s.as_bytes();
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_') { return false; }
    for &b in &bytes[1..] {
        if !(b.is_ascii_alphanumeric() || b == b'_') { return false; }
    }
    true
}
/// H5: Extract the target type identifier from an `impl` line.
fn impl_target_identifier(before_brace: &str) -> Option<&str> {
    let after = before_brace.trim_start();
    if !after.starts_with("impl") { return None; }
    let rest = after[4..].trim_start();
    // Skip generic parameters `<T: Bound>` if present.
    let rest = if rest.starts_with('<') {
        let mut depth = 1i32;
        let mut i = 1;
        while i < rest.len() && depth > 0 {
            match rest.as_bytes()[i] {
                b'<' => depth += 1,
                b'>' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        rest[i..].trim_start()
    } else {
        rest
    };
    // Find `for` clause — return first identifier after it.
    if let Some(for_idx) = rest.find(" for ") {
        let after_for = rest[for_idx + 5..].trim_start();
        // First identifier in `for Bar<T>` — scan chars directly.
        let id_start = after_for.find(|c: char| c.is_ascii_alphabetic() || c == '_')?;
        let id_end = after_for[id_start..].find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(after_for[id_start..].len());
        return Some(&after_for[id_start..id_start + id_end]);
    }
    // No `for`: first identifier after `impl` + generics.
    let id_start = rest.find(|c: char| c.is_ascii_alphabetic() || c == '_')?;
    let id_end = rest[id_start..].find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(rest[id_start..].len());
    Some(&rest[id_start..id_start + id_end])
}

fn after_for_identifiers(s: &str) -> Option<&str> {
    let trimmed = s.trim_start();
    let id_start = trimmed.find(|c: char| c.is_ascii_alphabetic() || c == '_')?;
    let id_end = trimmed[id_start..].find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(trimmed[id_start..].len());
    Some(&trimmed[id_start..id_start + id_end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_fn_def() {
        let r = classify_structural("    fn poll_write(", 2, false, true);
        // Grammar-free: tag=1 (fn_def) for any function signature.
        assert_eq!(r.tag, 1);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("poll_write"));
    }

    #[test]
    fn test_rust_fn_def_with_generics() {
        // W3: pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        let r = classify_structural("    pub fn block_on<F: Future>(&self, future: F) -> F::Output {", 1, true, false);
        assert_eq!(r.tag, 1, "expected fn_def, got tag={} name={:?}", r.tag, r.defined_name);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("block_on"));
    }

    #[test]
    fn test_rust_fn_def_generic_only() {
        // fn map<B, F: FnOnce(A) -> B>(self, f: F) -> Option<B> {
        let r = classify_structural("    fn map<B, F: FnOnce(A) -> B>(self, f: F) -> Option<B> {", 1, true, false);
        assert_eq!(r.tag, 1);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("map"));
    }

    #[test]
    fn test_rust_struct_def() {
        let r = classify_structural("pub struct Foo {", 1, false, false);
        assert_eq!(r.tag, 2); // type_def
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("Foo"));
    }

    #[test]
    fn test_python_def() {
        let r = classify_structural("def aggregate():", 1, false, false);
        assert_eq!(r.tag, 1); // fn_def
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("aggregate"));
    }

    #[test]
    fn test_python_class() {
        let r = classify_structural("class Bar:", 1, false, false);
        assert_eq!(r.tag, 2); // type_def (V21: PascalCase → type_def)
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("Bar"));
    }

    #[test]
    fn test_v21_python_def_with_params() {
        let r = classify_structural("def foo(x):", 1, false, false);
        assert!(r.is_def);
        assert_eq!(r.tag, 1); // function_def
        assert_eq!(r.defined_name, Some("foo"));
    }

    #[test]
    fn test_v21_python_class_with_params() {
        let r = classify_structural("class Foo(Bar):", 1, false, false);
        assert!(r.is_def);
        assert_eq!(r.tag, 2); // type_def
        assert_eq!(r.defined_name, Some("Foo"));
    }

    #[test]
    fn test_v21_python_async_def() {
        let r = classify_structural("async def foo(x):", 1, false, false);
        assert!(r.is_def);
        assert_eq!(r.tag, 1); // function_def
        assert_eq!(r.defined_name, Some("foo"));
    }

    #[test]
    fn test_v21_python_dunder() {
        let r = classify_structural("def __init__(self):", 1, false, false);
        assert!(r.is_def);
        assert_eq!(r.tag, 1); // function_def
        assert_eq!(r.defined_name, Some("__init__"));
    }

    #[test]
    fn test_v21_python_if() {
        let r = classify_structural("if x:", 1, false, false);
        assert!(!r.is_def);
    }

    #[test]
    fn test_v21_python_for() {
        let r = classify_structural("for x in y:", 1, false, false);
        assert!(!r.is_def);
    }

    #[test]
    fn test_v21_python_while() {
        let r = classify_structural("while x:", 1, false, false);
        assert!(!r.is_def);
    }

    #[test]
    fn test_v21_python_except() {
        // Accepted false positive: `except Exception:` classified as type_def
        // because `Exception` is PascalCase. Impact: <0.1% of lines.
        let r = classify_structural("except Exception:", 1, false, false);
        assert!(r.is_def); // false positive — accepted
        assert_eq!(r.tag, 2); // type_def
    }

    #[test]
    fn test_v21_python_with_as() {
        let r = classify_structural("with open(x) as fh:", 1, false, false);
        assert!(!r.is_def);
    }

    #[test]
    fn test_javascript_function() {
        let r = classify_structural("function foo() {", 1, false, false);
        assert_eq!(r.tag, 1); // fn_def
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("foo"));
    }

    #[test]
    fn test_go_func() {
        let r = classify_structural("func bar(", 1, false, false);
        assert_eq!(r.tag, 1); // fn_def
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("bar"));
    }

    #[test]
    fn test_java_void() {
        let r = classify_structural("public void baz() {", 1, false, false);
        // Last ID before `(` is `baz` (void is before).
        assert_eq!(r.tag, 1); // fn_def
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("baz"));
    }

    #[test]
    fn test_method_call_not_definition() {
        let r = classify_structural("obj.poll_write(", 2, false, false);
        // Preceded by `.` → not a definition.
        assert_eq!(r.tag, 0);
        assert!(!r.is_def);
    }

    #[test]
    fn test_control_flow_filtered_by_depth() {
        // if poll_write() { at depth 3 (inside a function body)
        let r = classify_structural("    if poll_write() {", 3, false, false);
        // Depth > 2 → not a definition.
        assert_eq!(r.tag, 0);
        assert!(!r.is_def);
    }

    #[test]
    fn test_local_binding() {
        let r = classify_structural("let mut park = CachedParkThread::new();", 2, false, false);
        // `let ... = ...` → local_binding (tag 6).
        // V66c: is_def=false — line-level is_def propagates to every token on
        // the line, so is_def=true here poisons OTHER phrases (the callee in
        // `let x = callee(...)`) with phantom def rows.
        assert_eq!(r.tag, 6);
        assert!(!r.is_def);
        assert_eq!(r.defined_name, Some("park"));
    }

    #[test]
    fn test_rust_generic_fn() {
        let r = classify_structural("    fn poll_write<T>(", 2, false, true);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("poll_write"));
        // Generic function: tag=1.
        assert_eq!(r.tag, 1);
    }

    #[test]
    fn test_match_arm_not_definition() {
        // match arm: `IoStack::Enabled(v) => v.park(handle)`
        let r = classify_structural("        IoStack::Enabled(v) => v.park(handle),", 3, false, false);
        // Not a block-start (no `{`/`(`/`<`/`:`) → not a definition.
        assert_eq!(r.tag, 0);
        assert!(!r.is_def);
    }

    #[test]
    fn test_pub_crate_fn() {
        // `pub(crate) fn name(` — first `(` is in `pub(crate)`, must skip to LAST `(`.
        let r = classify_structural("    pub(crate) fn close(&self) -> bool {", 2, false, true);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("close"));
    }

    #[test]
    fn test_impl_line_marks_target() {
        // impl Trait for Type {
        let r = classify_structural("impl Foo {", 0, true, false);
        // Last ID before `{` is `Foo` → type_def
        assert_eq!(r.tag, 2);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("Foo"));
    }

    #[test]
    fn test_h5_impl_with_generic_target() {
        // impl<T: Bound> Trait for Type<T> {
        let r = classify_structural("impl<T: Bound> Foo for Bar<T> {", 0, true, false);
        // Should return "Bar" (the type after `for`), not "T" (generic param of Bar)
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("Bar"));
    }

    #[test]
    fn test_h5_impl_without_for() {
        // impl Foo<T> {
        let r = classify_structural("impl Foo<T> {", 0, true, false);
        // Should return "Foo" (the type after `impl`, skipping generics)
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("Foo"));
    }

    #[test]
    fn test_h5_impl_with_string_param() {
        // impl<T: Debug + 'static> Type for Trait {
        let r = classify_structural("impl<T: Debug + 'static> Type for Trait {", 0, true, false);
        eprintln!("H5_string: tag={} is_def={} name={:?}", r.tag, r.is_def, r.defined_name);
        assert!(r.is_def, "expected is_def=true, got tag={} name={:?}", r.tag, r.defined_name);
        assert_eq!(r.defined_name, Some("Trait"));
    }

    #[test]
    fn test_c9_trait_method_declaration() {
        // trait method: ends with ;, has (, first param is &self
        let r = classify_structural("    fn bar(&self, amt: usize);", 1, false, false);
        assert_eq!(r.tag, 1, "expected fn_def, got tag={} name={:?}", r.tag, r.defined_name);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("bar"));
    }

    #[test]
    fn test_c9_trait_method_with_self() {
        let r = classify_structural("    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<()>;", 1, false, false);
        assert_eq!(r.tag, 1);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("poll_ready"));
    }

    #[test]
    fn test_c9_non_trait_semi_not_def() {
        // let x = foo();  ends with ;, has (, but first param is not self → NOT a trait method
        let r = classify_structural("    let x = foo();", 2, false, false);
        assert!(!r.is_def);
        // It's a local_binding (tag=6) — but with ends_with_semi we'd misclassify
        // if we naively treat semicolon as a trait decl. The strict `(&self/...)` check
        // prevents that.
    }
}

    #[test]
    fn debug_block_on_runtime() {
        let line = "    pub fn block_on<F: Future>(&self, future: F) -> F::Output {";
        let r = classify_structural(line, 1, true, false);
        eprintln!("tag={} is_def={} name={:?}", r.tag, r.is_def, r.defined_name);
    }
