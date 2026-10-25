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
/// Grammar-free declaration-keyword test: does `before_name` (the text that
/// precedes an identifier) end with a keyword that marks a definition?
///
/// This is the same set already used by the function-signature branch; it
/// covers every language the index handles without any per-language dispatch.
/// Does the line contain a declaration modifier that can precede a return
/// type (`public async Task<IActionResult> Name(...)`)? Grammar-free: a word
/// scan for the same qualifier set used by `has_decl_keyword_before`, but
/// anywhere in the prefix (the return type sits between modifier and name).
fn has_any_decl_keyword(before_name: &str) -> bool {
    const KW: &[&str] = &[
        "pub", "fn", "def", "func", "function", "async", "unsafe", "extern",
        "const", "static", "public", "private", "protected", "internal",
        "export", "void", "virtual", "override", "abstract", "sealed",
        "partial", "final", "synchronized", "native",
    ];
    before_name
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|w| KW.contains(&w))
}

pub(crate) fn has_type_keyword_before(before_name: &str) -> bool {
    const KW: &[&str] = &[
        "typedef", "interface", "struct", "enum", "union", "trait", "class",
        "record", "type",
    ];
    for kw in KW {
        if let Some(rest) = before_name.strip_suffix(kw) {
            // Word boundary: the keyword must not be the tail of a larger word
            // (`Datatype`, `subclass`, `prototype` are not declarations).
            let boundary_ok = rest
                .as_bytes()
                .last()
                .map(|&b| !b.is_ascii_alphanumeric() && b != b'_')
                .unwrap_or(true);
            if boundary_ok {
                return true;
            }
        }
    }
    false
}

/// A: return-type shape (no keyword list). `before_name` is the text
/// preceding the candidate name. Strip trailing pointer/reference
/// declarators (`*`, `&`), balanced trailing `<...>` groups (templated
/// return types), and trailing `::Ident` scope segments (definitions
/// like `int Foo::bar(`/qualified `std::string get(`), then count words:
/// two or more words (`EXPORT int`, `unsigned long`) always admit the
/// definition; a single word (`int` in `int foo(`) admits it only with a
/// `{` body after the paren group, which separates `int foo() {` from
/// `return foo(x)`/`emit foo(x);`. Calls fail structurally: `x = foo(`
/// ends with `=`, `obj.foo(` with `.`, bare `foo(` is empty.
fn has_return_type_shape(before_name: &str, has_body: bool) -> bool {
    let mut t = before_name;
    loop {
        // Strip trailing pointer/reference/whitespace.
        let s = t.trim_end_matches(|c: char| c == '*' || c == '&' || c.is_whitespace());
        if s.len() != t.len() {
            t = s;
            continue;
        }
        // Strip a trailing balanced `<...>` group (e.g. `std::vector<int>`).
        if s.as_bytes().last() == Some(&b'>') {
            let b = s.as_bytes();
            let mut depth = 0i32;
            let mut i = b.len();
            let mut ok = false;
            while i > 0 {
                i -= 1;
                if b[i] == b'>' {
                    depth += 1;
                } else if b[i] == b'<' {
                    depth -= 1;
                    if depth == 0 {
                        t = &s[..i];
                        ok = true;
                        break;
                    }
                }
            }
            if ok {
                continue;
            }
            return false;
        }
        // Strip a trailing `::Ident` scope segment (e.g. `int Foo::` before
        // `bar(`). An empty tail means the name directly follows `::`
        // (the normal qualified case) — strip unconditionally.
        if let Some(pos) = s.rfind("::") {
            let after = &s[pos + 2..];
            if after.is_empty()
                || (!after.is_empty()
                    && after.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'))
            {
                t = s[..pos].trim_end();
                continue;
            }
            return false;
        }
        break;
    }
    if !t
        .as_bytes()
        .last()
        .map(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .unwrap_or(false)
    {
        return false;
    }
    let mut words = 0u32;
    let mut in_word = false;
    for b in t.bytes() {
        let w = b.is_ascii_alphanumeric() || b == b'_';
        if w && !in_word {
            words += 1;
            in_word = true;
        } else if !w {
            in_word = false;
        }
    }
    words >= 2 || (words == 1 && has_body)
}

pub(crate) fn has_decl_keyword_before(before_name: &str) -> bool {
    before_name.ends_with("pub")
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
        || before_name.ends_with("def")
        || before_name.ends_with("struct")
        || before_name.ends_with("enum")
        || before_name.ends_with("union")
        || before_name.ends_with("trait")
        || before_name.ends_with("class")
        || before_name.ends_with("interface")
        || before_name.ends_with("mod")
        || before_name.ends_with("module")
}

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
        // B1: preprocessor islands classify before the water reject.
        // `#define NAME` is a definition (tag=1); usage directives
        // (`#ifdef`/`#ifndef`/`#undef`/`#if defined(`) are tokenized for
        // usages downstream but never defined here.
        if let Some(pre) = classify_preprocessor_line(line) {
            match pre {
                Preproc::Def { name, .. } => {
                    return StructuralResult { tag: 1, is_def: true, defined_name: Some(name) };
                }
                Preproc::Uses => {
                    return StructuralResult { tag: 0, is_def: false, defined_name: None };
                }
            }
        }
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }

    // H4: control-flow keywords that start a line are never definitions.
    // `if let Some(x) = foo {` would otherwise classify `foo` as a def.
    // A-MED-10: also handle `loop`, `switch`, `try`, `catch`, and leading `}`.
    // V84: `return` joins the list — `return {` / `return (` is an
    // expression, but a brace/paren at end makes it look like a block
    // start, so `return` was typed as a definition of the word `return`
    // in 123 zod files (374 def rows). A definition can never be named
    // `return` in any language that has the keyword.
    let start_keyword = trimmed.trim_start_matches('}');
    if start_keyword.starts_with("if ") || start_keyword.starts_with("while ")
        || start_keyword.starts_with("for ") || start_keyword.starts_with("match ")
        || start_keyword.starts_with("loop ") || start_keyword.starts_with("switch ")
        || start_keyword.starts_with("try ") || start_keyword.starts_with("catch ")
        || start_keyword.starts_with("else ") || start_keyword.starts_with("return ")
        // V87: Python's `except Exception:` / `elif cond:` are control flow.
        // The 5-rule Python colon classifier deliberately accepts
        // `except Exception:` as a type definition (documented <0.1% of
        // lines), but `Exception` is a common query: it answered with 153
        // `except` clauses presented as definitions. These words join the
        // same leading-word guard the rest of control flow uses.
        || start_keyword.starts_with("except ") || start_keyword.starts_with("except(")
        || start_keyword == "except" || start_keyword.starts_with("except:")
        || start_keyword.starts_with("except*")
        || start_keyword.starts_with("finally ") || start_keyword.starts_with("finally:")
        || start_keyword.starts_with("elif ") || start_keyword.starts_with("elif(")
        || start_keyword == "return" || start_keyword.starts_with("return(")
        || start_keyword.starts_with("return{") || start_keyword.starts_with("return;")
        // V84: `import`/`export` are declarations of bindings, not symbol
        // definitions: `import * as z from "zod"` was typed as a definition
        // of `from` (131 zod sites). `export` is handled by the real
        // declaration path when it names a symbol (`export function f`).
        || start_keyword.starts_with("import ") || start_keyword.starts_with("import{")
        || start_keyword.starts_with("import(")
    {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }
    // V80: a namespace is a scope container, not a symbol. `namespace App {`
    // (C#/C++) was tagged as a type_def named `App`, so every namespace showed
    // up as a bogus symbol and its whole body looked like one type.
    if start_keyword.starts_with("namespace ") {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }
    // A-MED-10: bare `} ` (for } else/catch without keyword) — skip as def.
    if trimmed.starts_with('}') {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }

    // V83 B10: equations must be checked before the block-start decision.
    // `let create () = Queue.create ()` carries parenthesized arguments,
    // so `is_function_signature` fires and the line is treated as a call
    // signature; without this the equation rules below are never reached.
    // Runs AFTER the match-arrow guard (an arrow assignment `const f = (x)
    // => ...` is not an equation: `=>` fails the top-level-assign scan).
    // Runs BEFORE the block-start branch only; the `type X =` / B1-B2
    // declaration rules live below and must keep priority.
    if let Some(name) = equation_name(line, block_depth) {
        return StructuralResult { tag: 1, is_def: true, defined_name: Some(name) };
    }
    // V84 P-D: module-scope point-free/constant definitions
    // (`arrayValues = List.map f`, `MAX_RETRIES = 5`). Runs after the
    // argument-carrying equation rule so `foo x =` keeps priority.
    if let Some(name) = pointfree_name(line, block_depth) {
        return StructuralResult { tag: 1, is_def: true, defined_name: Some(name) };
    }

    // V92: brace-less `impl` headers (multi-line where-clauses). Checked
    // before the delimiter search, which requires a `{` on the same line.
    if let Some(name) = impl_header_without_brace(trimmed) {
        if block_depth <= 4 {
            return StructuralResult { tag: 2, is_def: true, defined_name: Some(name) };
        }
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
    // V92: `{}` is an empty body, not a missing one. `unsafe impl<T> Send for
    // X {}` and `fn f() {}` end with `}` and were falling through every
    // branch, so all 478 `implementation` definitions in tokio that have an
    // empty body were invisible. A match arm ending in `{}` is guarded below
    // by the `=>` check.
    let ends_with_brace = bytes.last() == Some(&b'{')
        || (bytes.len() >= 2 && bytes[bytes.len() - 2] == b'{' && bytes[bytes.len() - 1] == b'}');
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
        // B8: assignment-arrow shape is a definition, not a match arm.
        // `const foo = (x) => x + 1` has a standalone `=` (not part of
        // `=>`/`==`/`>=`/`<=`/`!=`) before the first `(` outside strings,
        // with `=>` after it. Name = last ident before that `=`. Match
        // arms (`case X =>`), bare arrows (`x => x+1`), and calls with
        // arrow args (`foo(a => a+1)`) have no standalone `=` and stay
        // water. The `=` position carries the shape; no words are read.
        let t = trimmed.as_bytes();
        let mut si = 0usize;
        let mut in_s = false;
        let mut es = false;
        let mut eq_pos: Option<usize> = None;
        let mut paren_pos: Option<usize> = None;
        while si < t.len() {
            let c = t[si];
            if es { es = false; si += 1; continue; }
            if c == b'\\' && in_s { es = true; si += 1; continue; }
            if c == b'"' { in_s = !in_s; si += 1; continue; }
            if in_s { si += 1; continue; }
            if c == b'/' && si + 1 < t.len() && t[si + 1] == b'/' { break; }
            if c == b'(' && paren_pos.is_none() {
                paren_pos = Some(si);
            }
            if c == b'=' {
                let nx = if si + 1 < t.len() { t[si + 1] } else { 0 };
                let pv = if si > 0 { t[si - 1] } else { 0 };
                let part_of = nx == b'=' || nx == b'>'
                    || pv == b'=' || pv == b'!' || pv == b'<' || pv == b'>';
                if !part_of && eq_pos.is_none() {
                    eq_pos = Some(si);
                }
            }
            si += 1;
        }
        if let (Some(ep), Some(pp)) = (eq_pos, paren_pos) {
            if ep < pp {
                if let Some(nm) = scan_last_identifier(&trimmed[..ep]) {
                    if is_valid_identifier(nm) {
                        return StructuralResult { tag: 1, is_def: true, defined_name: Some(nm) };
                    }
                }
            }
        }
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }
    // Function signature has `(` and doesn't end with `=`/`;`/`,` (not assignment/expression).
    // V86: a `(` inside an unclosed `<...>` group is a generic type argument,
    // not a parameter list:
    //   `pub symbols_added: Vec<(String, String, i32)>,`
    // That line was a function signature naming `Vec`, so three struct fields
    // produced `find_references("vec", def_only)` answers. Real signatures put
    // their `(` after the generic list closes (`fn foo<T>(x: T)`), so the angle
    // depth at the paren position separates the two by position alone.
    let first_sig_paren = first_signature_paren(trimmed);
    let has_open_paren = first_sig_paren.is_some();
    let is_function_signature = has_open_paren && !ends_with_eq && !ends_with_semi && !ends_with_comma;

    // C9: trait method declaration has `(self`/`(&self`/`(&mut self` as first param
    // and ends with `;` (no body). Detect this to mark it as a function def.
    // Example: `fn bar(&self, amt: usize);` inside `trait Foo { ... }`.
    // V94: a `fn`/`def` declaration line that ends in `;` has no body, so it
    // is a trait/interface method whether or not it takes `self`.
    // `unsafe fn from_raw_handle(h: RawHandle) -> Self;` in `tokio/src/doc/os.rs`
    // was invisible; the C9 rule below only accepted `self` receivers.
    let is_bodyless_decl = ends_with_semi
        && has_open_paren
        && !ends_with_eq
        && {
            let head = trimmed[..first_sig_paren.unwrap_or(0)].trim_end();
            head.split_whitespace().rev().take(3).any(|w| {
                let w = w.trim_end_matches(['<', '>']);
                matches!(w, "fn" | "def" | "func" | "function")
            })
        };
    let is_trait_method_decl = if is_bodyless_decl {
        true
    } else if ends_with_semi && has_open_paren && !ends_with_eq {
        let paren_idx = first_sig_paren;
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
        // V78: type declarations whose line ends in neither `{`, `(`, nor `:`
        // — `class Foo : Base` (C#/Java/C++), `public class Foo` with the brace
        // on the next line, `interface IFoo`, `class Foo<T> where T : X`. These
        // are the dominant .NET/Java style and were never tagged as definitions
        // (every brace-language branch below requires a delimiter on the line),
        // so `impact`/`find_definition` reported "no definition found" for
        // perfectly-indexed types. Grammar-free: the name is a PascalCase
        // identifier immediately preceded by a type-declaration keyword; a base
        // list after it (`ControllerBase`) is not the name.
        // Restricted to depth <= 2 so a `Foo x = new Foo()`-style local inside a
        // method body (depth 3+) is never mistaken for a type declaration.
        if block_depth <= 2 {
            for idx in 0..delims.ident_count {
                let s = delims.ident_starts[idx] as usize;
                let e = delims.ident_ends[idx] as usize;
                let prefix = trimmed[..s].trim_end();
                if has_type_keyword_before(prefix) && is_pascal_case(&trimmed[s..e]) {
                    return StructuralResult {
                        tag: 2,
                        is_def: true,
                        defined_name: Some(&trimmed[s..e]),
                    };
                }
            }
        }
        // Not a block-start line. Could still be a local binding or param.
        // V83 B1/B2: `type X = ...`, `const X = ...`, `static X: T = ...`
        // (PascalCase/SCREAMING_CASE bound name) are symbol declarations,
        // not local bindings. Blind-tokio forensics: 381 `type X = ...`
        // sites and 69 module constants were bound to the annotation word
        // (`Runtime` in `const EXECUTOR: Lazy<Runtime> = ...`) or dropped.
        if let Some(name) = decl_assign_name(trimmed) {
            // V86: `let ones: Vec<i32> = {` is a local binding, not a type
            // declaration of `i32`. `decl_assign_name` requires the bound name
            // to be PascalCase/SCREAMING_CASE, and `decl_assign_name` itself
            // already cuts at a top-level annotation colon — but the value here
            // starts with `{`, so this line reached the block-start path and
            // the annotation was never cut. A value-binding keyword before the
            // name means the name is the binding, whatever its case: the
            // declaration-assign rule exists for `type X =` / `const X =`,
            // never for `let`.
            if !has_value_binder_before(trimmed) {
                return StructuralResult { tag: 2, is_def: true, defined_name: Some(name) };
            }
        }
        // (Equation rule ran before the block-start decision — see B10 at
        // the top of this function: binder-led equations carry parens, so
        // they reach the call-signature path, not this one.)
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
    let _paren_pos = delims.first_paren_pos();
    // V98: an opening brace ANYWHERE on the line makes it a block start, not
    // only a brace at end-of-line. A one-line declaration with a non-empty
    // body (`pub struct Alpha { pub tag: u8 }`, `class Foo { int x; }`,
    // `enum E { A, B }`) ends with `}` and matched no branch, so it was water:
    // `def_only` skipped it, `list_methods` had no impl target, and qualified
    // names lost the type. The `{}` shape was special-cased; a populated body
    // was not. Found by the v0.9.1 install smoke test on a six-line file.
    //
    // Safe because the V95 rule below already requires a declaration word in
    // the head, so `if x { y(); }` and `match v { A => 1 }` stay water.
    let has_open_block = bytes.contains(&b'{');
    let brace_pos = if ends_with_brace || has_open_block {
        delims.first_brace_pos()
    } else {
        None
    };
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

    if is_function_signature && first_sig_paren.is_some() {
        // P3-1 principled: the function name is the LAST identifier whose
        // next non-whitespace char is `(` or `<`, restricted to the part of
        // the line before any inline body (`{`). Without that restriction a
        // one-liner like `pub fn alpha() -> i32 { beta() + 1 }` would record
        // the definition under the trailing call's name.
        // The name comes from the signature paren chosen above, not from a
        // line-wide "last ident followed by (" scan.
        // Take the name from the chosen signature paren. If that lands on a
        // declaration word (a return type such as `-> impl Iterator`), use the
        // identifier immediately after the declaration keyword instead of
        // falling back to a line-wide scan that would pick the same word.
        let from_sig_paren = first_sig_paren.and_then(|p| name_before_paren(trimmed, p));
        let after_keyword = decl_keyword_follower(trimmed);
        let from_ident_scan =
            find_function_name_pos_before(&delims, delims.first_brace_pos());
        let chosen = from_sig_paren
            .map(|(s, _n)| (s, 0usize))
            .or(after_keyword)
            .or(from_ident_scan);
        if let Some((name_start, _idx)) = chosen {
            // V65: a bare call like `impl_target_identifier(before_brace)` is
            // NOT a definition. The name must be preceded by a declaration
            // keyword (`fn`, `pub`, `async`, `unsafe`, `extern`, `const`) or
            // by a block boundary (`{`, `}`) or line start. A call is
            // preceded by `=`/`,`/`(`/`)`/`.`/`return`/`if`/`else`/`while`/
            // `for`/`match`/`let` — an expression context. Grammar-free:
            // inspect the char before the name (skipping whitespace).
            let mut before_name = trimmed[..name_start].trim_end();
            // B5: `get`/`set` are transparent prefixes (`get foo() {` →
            // bare `foo(`). The words never classify anything alone; the
            // empty-name rule below does the real work.
            if before_name == "get" || before_name == "set" {
                before_name = "";
            }
            let before_bytes = before_name.as_bytes();
            // Body brace after the paren group on the same line (shared by
            // the A shape rule and the B5 empty-name rule).
            let has_body = match (delims.first_paren_pos(), delims.first_brace_pos()) {
                (Some(p), Some(b)) => b > p,
                _ => false,
            };
            // V86: an assignment whose value contains a call is an
            // expression, never a declaration:
            //   `text = resp["x"] if cond else str(resp)`
            // This line has a top-level `=` before the paren group and no
            // declaration keyword, yet the last `ident(` before the inline
            // body was `str` — so `def_only("str")` answered with a Python
            // script. A real declaration has no top-level `=` before the name
            // (`const foo = (x) => ...` is handled by the arrow-assignment
            // rule above and never reaches here).
            let assign_before_name = find_top_level_eq(&trimmed[..name_start]).is_some();
            let decl_ok = if assign_before_name && !has_any_decl_keyword(before_name) {
                false
            } else if before_bytes.is_empty() {
                // B5: bare `name(args) {` at depth ≥ 1 with a same-line body
                // brace is a shorthand method (`foo() {`, `get foo() {`,
                // `constructor() {`). The brace context carries it — a call
                // statement ends with `;` or nothing, never `{`. Depth 0
                // stays rejected (top-level bare calls). JS ASI call+block
                // on one line is the accepted residual (<0.1%).
                has_body && block_depth >= 1
            } else {
                let last_c = before_bytes[before_bytes.len() - 1];
                let keyword_before = has_decl_keyword_before(before_name);
                // V79: a *type/return* qualifier (not a value operator) before
                // the name marks a method declaration in C#/Java/TS/Go:
                // `public async Task<IActionResult> GetAsync(Guid id)` and the
                // camelCase `public int handleRequest(int id)`. The return type
                // sits between the modifier and the name, so `before_name` does
                // not *end* with the keyword — scan it for one anywhere. A call
                // (`result = compute(x)`, `return foo(x)`, `new Foo()`) has no
                // declaration keyword before the name, so this does not admit
                // expressions. Case is deliberately not constrained: Java/Go/
                // Kotlin methods are camelCase.
                let typed_decl = has_any_decl_keyword(before_name);
                // A: return-type shape — two or more words before the name
                // (`EXPORT int`, `unsigned long foo(`) admit the definition
                // whatever the words are; a single word (`int foo(`) admits
                // only with a `{` body after the paren group (separates
                // `int foo() {` from `return foo(x)`). Structural: calls
                // fail this shape (`=`/`.`/empty before the name).
                let return_shape = has_return_type_shape(before_name, has_body);
                (last_c == b'{' || last_c == b'}') || keyword_before || typed_decl || return_shape
            };
            if !decl_ok {
                return StructuralResult { tag: 0, is_def: false, defined_name: None };
            }
            delim_pos = Some(name_start + trimmed[name_start..].bytes().take_while(
                |c| c.is_ascii_alphanumeric() || *c == b'_').count());
        } else {
            return StructuralResult { tag: 0, is_def: false, defined_name: None };
        }
        delim_char = b'(';
    } else if is_trait_method_decl && first_sig_paren.is_some() {
        // C9: trait method declaration — same principled logic.
        if let Some((ns, _)) = first_sig_paren.and_then(|p| name_before_paren(trimmed, p)) {
            delim_pos = Some(ns + trimmed[ns..].bytes().take_while(
                |c| c.is_ascii_alphanumeric() || *c == b'_').count());
        } else {
            return StructuralResult { tag: 0, is_def: false, defined_name: None };
        }
        delim_char = b'(';
    } else if (ends_with_brace || has_open_block) && brace_pos.is_some() {
        // Type: `struct Foo {`, `enum Bar {`, `class Baz {`, and the one-line
        // forms `struct Foo { x: u8 }` / `class Baz { int x; }`.
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

    // V86: a brace directly after a closure's closing `|` opens a closure
    // body, not a type body:
    //   `seen: &mut HashSet<(String, String, i32)>| {`
    // The last identifier before that brace is a type argument, so this line
    // produced a definition of `i32`.
    if delim_char == b'{' {
        let head = trimmed[..delim_pos].trim_end();
        if head.ends_with('|') {
            return StructuralResult { tag: 0, is_def: false, defined_name: None };
        }
    }

    // V86: a top-level `=` before the opening delimiter means this is an
    // assignment whose value happens to open a block:
    //   `let ones: Vec<i32> = {`
    // The `{` path would otherwise name the last identifier before the brace,
    // which is the *annotation* (`i32`), and report a type definition. That
    // made `find_references("i32", def_only)` answer with a function.
    // Declarations the brace path exists for carry no `=` (`struct Foo {`,
    // `impl Foo for Bar {`), so this is a positional discriminator, not a list.
    if delim_char == b'{' {
        if let Some(eq) = find_top_level_eq(&trimmed[..delim_pos]) {
            let lhs = &trimmed[..eq];
            let name = scan_last_identifier_before(lhs).unwrap_or("");
            return StructuralResult {
                tag: 6, // local_binding — a value, not a symbol definition
                is_def: false,
                defined_name: if is_valid_identifier(name) { Some(name) } else { None },
            };
        }
    }

    // V95: brace-shaped lines that are NOT type declarations.
    //
    // The `{` branch above exists for `struct Foo {` / `class Bar {`. Other
    // shapes end in `{` and were misclassified as definitions, which is worse
    // than a plain miss: `usage_only` filters definition sites out of the
    // caller list, so a call site wearing a definition tag becomes INVISIBLE
    // to "who calls X". Measured on identifier-scrambled tokio, 1,133 of
    // 5,666 tag-2 rows were one of these, and every missed usage traced to
    // this single bug (usage recall 0.894 -> 0.998).
    //
    // Two positional tests, no language detection:
    //
    //   1. A declaration always names a declaration word somewhere before the
    //      brace (`struct Foo {`, `impl X for Y {`, `class A:`), so a head
    //      with no such word is an expression opening a block:
    //          `Self {`  `unsafe {`  `loop {`  `async move {`  `Foo {`
    //      The word only DEFERS the reject; the declared name still comes
    //      from position, never from the word.
    //   2. Two shapes carry a word but are still not declarations:
    //          `tokio::select! {`   — `!` immediately before the brace
    //          `use tokio::{`       — `::` immediately before the brace
    //
    // Nesting is NOT a reject any more: `macro_rules!` bodies and functions
    // declared inside functions are real definitions, and a blanket depth
    // reject lost 67 of them (recall 0.9976 -> 0.9938). A struct literal is
    // already caught by test 1 because it has no declaration word.
    if delim_char == b'{' {
        let head = trimmed[..delim_pos].trim_end();
        let head_declares = head
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|w| matches!(
                w,
                "struct" | "enum" | "union" | "trait" | "class" | "interface"
                    | "type" | "record" | "typedef" | "impl" | "fn" | "def"
                    | "func" | "function" | "macro_rules" | "data"
                    | "newtype" | "module" | "object" | "namespace"
            ));
        // `macro_rules! name {` declares `name`. Take it from the token after
        // the bang: without this the name resolved to `macro_rules` and the
        // macro itself had no definition row (ctags reports it, so the blind
        // oracle counted it missing).
        if head.ends_with('!') {
            let lower = head.to_ascii_lowercase();
            if let Some(b) = lower.rfind("macro_rules!") {
                let after = head[b + "macro_rules!".len()..].trim();
                if let Some(name) = scan_last_identifier(after) {
                    if is_valid_identifier(name) {
                        return StructuralResult {
                            tag: 1,
                            is_def: true,
                            defined_name: Some(name),
                        };
                    }
                }
            }
            return StructuralResult { tag: 0, is_def: false, defined_name: None };
        }
        if !head_declares || head.ends_with("::") {
            return StructuralResult { tag: 0, is_def: false, defined_name: None };
        }
    }

    // Find last identifier before the delimiter.
    let mut before = &trimmed[..delim_pos];
    // B7: strip a trailing `where ...` clause FIRST (`struct Foo<T> where
    // T : Clone {` resolved to `Clone`). Word-boundary `where` followed
    // by whitespace up to the delimiter is a constraint clause, not part
    // of the name. One routing word gating a positional cut (same class
    // as `define`); the name comes from shape, never from the word.
    // Runs before the V59 angle-strip so `<T>` resolves after the cut.
    if delim_char == b'{' {
        // Last occurrence of word-boundary " where ".
        let mut wpos: Option<usize> = None;
        let bytes = before.as_bytes();
        let mut k = 1;
        while k + 6 <= bytes.len() {
            if &bytes[k..k + 5] == b"where"
                && (bytes[k - 1] == b' ' || bytes[k - 1] == b'\t')
                && (bytes[k + 5] == b' ' || bytes[k + 5] == b'\t')
            {
                wpos = Some(k);
            }
            k += 1;
        }
        if let Some(wp) = wpos {
            before = before[..wp].trim_end();
        }
        // V84 P-F: strip `extends ...` / `implements ...` clauses the same
        // way. `export interface $ZSFAny extends $ZSF {` bound the name to
        // the SUPERCLASS (`ZSF`) in 379 zod interface sites. The declared
        // name is the identifier immediately BEFORE the clause; the clause
        // itself is a type reference, never the definition. Positional:
        // word boundary, clause runs to the delimiter.
        let mut cpos: Option<usize> = None;
        let bytes = before.as_bytes();
        // Angle-depth 0 only: `interface X<T extends Y> extends Z {` has an
        // `extends` INSIDE the generic parameter list; stripping at the first
        // occurrence named the parameter (`PrefixItems`). The clause that
        // precedes a class body is the one at depth 0.
        // Cut at the FIRST depth-0 clause: `class Foo extends Bar implements
        // Baz` must yield `Foo`, so cutting at `implements` (the later match)
        // would leave `... extends Bar` and name `Bar`.
        let mut angle: i32 = 0;
        let mut k = 0usize;
        'scan: while k < bytes.len() {
            match bytes[k] {
                b'<' => angle += 1,
                b'>' if angle > 0 => angle -= 1,
                _ => {}
            }
            if angle == 0 && k > 0 && (bytes[k - 1] == b' ' || bytes[k - 1] == b'\t') {
                for kw in [b"extends ".as_slice(), b"implements ".as_slice()] {
                    if bytes[k..].starts_with(kw) {
                        cpos = Some(k);
                        break 'scan;
                    }
                }
            }
            k += 1;
        }
        if let Some(cp) = cpos {
            before = before[..cp].trim_end();
        }
        // V92: a top-level `:` before the body opens a bound/base list:
        //   `pub trait StreamExt: Stream {`      -> StreamExt
        //   `public class Foo: Bar, IBaz {`      -> Foo
        // Inside `<...>` the colon is a generic bound and is already skipped
        // by the angle-depth check; a `::` path segment is not a separator.
        let bytes = before.as_bytes();
        let mut angle: i32 = 0;
        let mut cpos: Option<usize> = None;
        let mut k = 0usize;
        while k < bytes.len() {
            match bytes[k] {
                b'<' => angle += 1,
                b'>' if angle > 0 => angle -= 1,
                b':' if angle == 0 => {
                    let prev = if k > 0 { bytes[k - 1] } else { 0 };
                    let next = if k + 1 < bytes.len() { bytes[k + 1] } else { 0 };
                    if prev != b':' && next != b':' {
                        cpos = Some(k);
                    }
                }
                _ => {}
            }
            k += 1;
        }
        if let Some(cp) = cpos {
            let head = before[..cp].trim_end();
            // Only cut when the head still names something: `a: b` alone is
            // a field, not a declaration.
            if head.bytes().any(|c| c.is_ascii_alphabetic() || c == b'_') {
                before = head;
            }
        }
    }
    // V59: for brace-terminated type defs (`struct Foo<T> {`), the last ident
    // before `{` is a generic param/lifetime. Walk back past the `<...>` group
    // so defined_name is the TYPE name. Pure bracket-counting, grammar-free.
    if delim_char == b'{' {
        let trimmed_before = before.trim_end();
        let bytes_b = trimmed_before.as_bytes();
        if !bytes_b.is_empty() && *bytes_b.last().unwrap() == b'>' {
            // V94: cut at the FIRST `<` whose angle depth is 0 on this line.
            // Walking backwards from the trailing `>` breaks when the generic
            // list contains an arrow: `struct CallOnDrop<O, F: FnOnce() -> O>`
            // stopped at the `->`'s `>`, leaving `O` as the name.
            let depths = angle_depth_at(trimmed_before);
            let mut cut: Option<usize> = None;
            for (i, d) in depths.iter().enumerate() {
                if *d == 0 && bytes_b[i] == b'<' {
                    cut = Some(i);
                    break;
                }
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
    // V94: `unsafe impl` / `default impl` are impl headers too. The old test
    // required the line to START with `impl`, so `unsafe impl<T> Send for
    // JoinHandle<T> {}` fell through to the generic path and recorded the
    // name `impl`.
    let is_impl_line = {
        let mut rest = trimmed;
        for p in ["unsafe ", "default "] {
            if let Some(r) = rest.strip_prefix(p) {
                rest = r.trim_start();
            }
        }
        rest.starts_with("impl")
            && matches!(rest.as_bytes().get(4), Some(b' ') | Some(b'<') | Some(b'{'))
    };
    let name = if is_impl_line {
        let before_brace = &trimmed[..delim_pos];
        // Find the `for` keyword — after it is the target type.
        // V94: look for ` for ` in the header BEFORE the where-clause strip,
        // which removes `for St` in `impl<St> Trait for St where St: Bound {}`.
        let header_full = &trimmed[..delim_pos];
        let for_in_header = header_full.find(" for ");
        let target_type = if let Some(for_pos) = for_in_header {
            self_type_name(header_full[for_pos + 5..].trim())
        } else if let Some(for_pos) = before_brace.find(" for ") {
            after_for_identifiers(before_brace[for_pos + 5..].trim())
        } else {
            // No `for`: pick the identifier after `impl` skipping generics.
            impl_target_identifier(before_brace)
        };
        // An impl whose self type has no name (`impl X for ()`) declares
        // nothing queryable. Falling back to `last_id` here named `for`.
        match target_type {
            Some(t) => t,
            None => return StructuralResult { tag: 0, is_def: false, defined_name: None },
        }
    } else {
        last_id
    };

    // V86: a line that STARTS with `)` continues a signature opened on a
    // previous line. The name was defined up there; the only identifiers down
    // here are the return type and parameters. `) -> String {` used to produce
    // a type definition of `String`, which is why `find_references("str")` and
    // friends answered with unrelated functions.
    if trimmed.starts_with(')') {
        return StructuralResult { tag: 0, is_def: false, defined_name: None };
    }

    // Step 4: Depth filter — definitions at module/impl level (depth ≤ 2).
    // This filters out control-flow blocks (if/while/match/for) nested inside
    // function bodies; those are already excluded by the start-keyword guard
    // above, so a *keyword-confirmed* declaration (`fn x`, `pub fn x`, `def x`,
    // `struct X`, …) is exempt. Without the exemption, any real definition
    // nested 3+ levels deep (inside `macro_rules!` bodies or 3+ `mod` blocks)
    // was silently dropped — tokio had 265 such `fn <name>` lines indexed
    // is_def=0, e.g. `fn as_handle` generated by the `impl_traits!` macro.
    let name_prefix = trimmed[..last_id_start].trim_end();
    let keyword_confirmed = has_decl_keyword_before(name_prefix) || is_impl_line;
    if block_depth > 2 && !keyword_confirmed {
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

impl<'a> StructuralResult<'a> {
    /// "Not a definition" — the value every gated pipeline must return.
    ///
    /// A single constructor keeps the write paths (ingest, JIT, reindex,
    /// warrant scan) from drifting: three separate `StructuralResult { ... }`
    /// literals with one wrong field is exactly how the tag/is_def divergence
    /// class started.
    #[inline]
    pub const fn water() -> Self {
        StructuralResult { tag: 0, is_def: false, defined_name: None }
    }
}

/// V21: Branch-free PascalCase check — first byte is an uppercase ASCII letter.
/// Uses `is_ascii_uppercase()` which checks `A <= b <= Z` (0x41-0x5A).
/// Note: `_` (0x5F) is NOT uppercase despite having bit 5 clear.
fn is_pascal_case(name: &str) -> bool {
    // V92: skip leading underscores. `_CookieCompatResponse` is a private
    // class, `__Version__` a private type; the old check looked at byte 0,
    // saw `_`, and rejected them, so every underscore-prefixed type in every
    // Python codebase was invisible to `class X:` detection.
    name.trim_start_matches('_')
        .as_bytes()
        .first()
        .map(|&b| b.is_ascii_uppercase())
        .unwrap_or(false)
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
    // B4: also `)->ANNOT:` — a return annotation sits between `)` and `:`.
    // Shape-constrained (no keyword list): after the last `)`, optional
    // whitespace, literal `->`, then only annotation characters (ident
    // chars, whitespace, `[]`, `,`, `|`, `.` for `list[int]`, `X | Y`,
    // `pkg.Type`) up to the trailing `:`. Anything else (calls, junk)
    // falls through to the later rules.
    let ends_with_annot_colon = if let Some(rp) = last_rparen {
        let mut tail = &trimmed[rp + 1..];
        tail = tail.trim_start_matches([' ', '\t']);
        if let Some(ann) = tail.strip_prefix("->") {
            let ann = ann.trim_start_matches([' ', '\t']);
            // Must end with `:` (mod trailing ws) and contain only
            // annotation characters before it.
            let ann = ann.trim_end_matches([' ', '\t']);
            ann.ends_with(':')
                && ann[..ann.len() - 1].bytes().all(|b| {
                    b.is_ascii_alphanumeric() || b == b'_' || b == b' ' || b == b'\t'
                        || b == b'[' || b == b']' || b == b',' || b == b'|' || b == b'.'
                })
                && !ann[..ann.len() - 1].trim().is_empty()
        } else {
            false
        }
    } else {
        false
    };
    if ends_with_rparen_colon || ends_with_annot_colon {
        // Find the identifier immediately before the matching `(`.
        let rp = last_rparen.unwrap();
        let lp = find_byte_backwards_outside_string(trimmed, b'(', rp);
        if let Some(lp_pos) = lp {
            let before_paren = &trimmed[..lp_pos];
            if let Some(name) = scan_last_identifier(before_paren) {
                if is_valid_identifier(name) {
                    // B4: mirror the paren path's expression-context guard.
                    // `x = foo(a):` / `obj.foo(a):` are calls, not defs —
                    // the char before the name (skipping whitespace) is
                    // `=`/`.`. `with open(x):` and `case Foo(x):` survive
                    // (before-name ends with a word char), preserving the
                    // documented <0.1% behavior.
                    let name_off = (name.as_ptr() as usize).saturating_sub(before_paren.as_ptr() as usize);
                    let before_name = before_paren[..name_off.min(before_paren.len())].trim_end();
                    if let Some(&last) = before_name.as_bytes().last() {
                        if last == b'=' || last == b'.' {
                            return StructuralResult { tag: 0, is_def: false, defined_name: None };
                        }
                    }
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

/// P3-1: Find the function name, ignoring identifiers that appear inside an
/// inline body.
///
/// `body_start` is the byte offset of the first `{` on the line (if any).
/// A definition's name always precedes its body, so for
/// `pub fn alpha() -> i32 { beta() + 1 }` the trailing `beta()` call must not
/// win the "last `(`-match" rule — otherwise the definition is recorded under
/// the callee's name (and the callee gains a phantom definition at this line).
fn find_function_name_pos_before(delims: &LineDelimiters, body_start: Option<usize>) -> Option<(usize, usize)> {
    // P3-1 principled rule (grammar-free):
    // 1. If any identifier at angle_depth=0 is followed by `(`, use the LAST
    //    such identifier (skips pub(crate) wrapper, handles `fn poll_ready() -> Poll<()>`).
    // 2. If NO `(` match exists, use the FIRST identifier at depth=0 followed by `<`
    //    (the function name is first; return type `Option<B>` comes later).
    //
    // Candidates at or after `body_start` live inside an inline body and are
    // calls, never the definition itself.
    let before_body = |pos: usize| body_start.is_none_or(|b| pos < b);
    let mut paren_matches: Vec<(usize, usize)> = Vec::new();
    let mut lt_matches: Vec<(usize, usize)> = Vec::new();
    for idx in 0..delims.ident_count {
        let nc = delims.ident_next[idx];
        let depth = delims.ident_angle_depth[idx];
        if depth > 0 { continue; }
        let pos = (delims.ident_starts[idx] as usize, idx);
        if !before_body(pos.0) { continue; }
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
            // `->` and `=>` are operators, not closers. Without this guard the
            // arrow's `>` decremented a depth it never incremented, so every
            // later `=` looked top-level: `fn f() -> impl Iterator<Item = u8>`
            // was read as an assignment and the definition was lost.
            b'>' if i > 0 && (bytes[i - 1] == b'-' || bytes[i - 1] == b'=') => {}
            b')' | b'>' | b'}' | b']' => depth -= 1,
            b'=' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// First top-level assignment `=` with operator neighbours excluded
/// (`==`, `!=`, `<=`, `>=`, `=>` are not assignments). Same walk as
/// `find_top_level_eq`, tightened for the declaration-assign rule.
fn top_level_assign_eq(s: &str) -> Option<usize> {
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
            b'>' if i > 0 && (bytes[i - 1] == b'-' || bytes[i - 1] == b'=') => {}
            b')' | b'>' | b'}' | b']' => depth -= 1,
            b'=' if depth == 0 => {
                let prev = if i > 0 { bytes[i - 1] } else { b' ' };
                let next = if i + 1 < bytes.len() { bytes[i + 1] } else { b' ' };
                if next != b'=' && next != b'>'
                    && prev != b'=' && prev != b'!' && prev != b'<' && prev != b'>'
                {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// First top-level `:` that is not part of `::` (a type annotation).
fn top_level_annotation_colon(s: &str) -> Option<usize> {
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
            b':' if depth == 0 => {
                let prev = if i > 0 { bytes[i - 1] } else { b' ' };
                let next = if i + 1 < bytes.len() { bytes[i + 1] } else { b' ' };
                if prev != b':' && next != b':' {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Declaration-assignment name (V83 B1/B2). The positional shape of
/// `type X = ...`, `const X = ...`, `static X: T = ...`, `X: T = ...`
/// where `X` is PascalCase or SCREAMING_CASE: a top-level `=` whose
/// bound identifier has that case shape is a symbol declaration, not a
/// local binding. Blind-tokio forensics: 381 `type X = ...` sites and
/// 69 module-constant sites were bound to a type-annotation word
/// (`Runtime` in `const EXECUTOR: Lazy<Runtime> = ...`) or watered by
/// the `=` rules. Ordinary lowercase bindings (`x = f()`, `let y = ...`,
/// `self.count = 5`) return None and keep their existing treatment.
/// Value-free: only the case shape of the bound identifier is read.
pub fn decl_assign_name(line: &str) -> Option<&str> {
    let eq = top_level_assign_eq(line)?;
    let prefix = line[..eq].trim_end();
    // A depth-0 annotation colon puts the name before it:
    // `const EXECUTOR: Lazy<Runtime>` -> the name is `EXECUTOR`.
    let name_src = match top_level_annotation_colon(prefix) {
        Some(c) => prefix[..c].trim_end(),
        None => prefix,
    };
    let name = scan_last_identifier_before(name_src)?;
    if !is_valid_identifier(name) {
        return None;
    }
    let b = name.as_bytes();
    let first_up = b.first().map(|c| c.is_ascii_uppercase()).unwrap_or(false);
    let all_caps = b
        .iter()
        .all(|c| c.is_ascii_uppercase() || *c == b'_' || c.is_ascii_digit());
    // V94: a `type` alias is a declaration whatever the name's case. Rust
    // aliases are snake_case (`pub type uid_t = u32;`), and the case gate
    // exists only to keep `x = 1` a local binding. The keyword carries the
    // shape, so it does not need the case.
    // The `type` keyword may follow visibility modifiers (`pub type X`).
    let type_led = name_src.split_whitespace().any(|w| w == "type");
    if first_up || all_caps || type_led {
        Some(name)
    } else {
        None
    }
}

/// Angle-bracket depth at each byte, treating `->`/`=>`/`<=`/`>=` as operators.
///
/// `>>` closes TWO groups, so it must decrement twice; treating it as an
/// operator left the depth stuck at 1 for the rest of the line and made
/// `pub fn segment_lines<S: AsRef<str>>(...)` look like a brace definition of
/// its return type.
fn angle_depth_at(line: &str) -> Vec<i32> {
    let b = line.as_bytes();
    let mut depth: i32 = 0;
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while i < b.len() {
        out.push(depth);
        match b[i] {
            b'<' => {
                let prev = if i > 0 { b[i - 1] } else { 0 };
                let next = b.get(i + 1).copied().unwrap_or(0);
                // `<=` and either half of `<<` are operators; `-<` does not
                // occur (only `->`). Without the `prev` check the second `<`
                // of a shift opened a phantom generic group.
                if next != b'=' && next != b'<' && prev != b'<' && prev != b'-' {
                    depth += 1;
                }
            }
            b'>' => {
                let prev = if i > 0 { b[i - 1] } else { 0 };
                // `->`, `=>`, `>=` are operators. `>>` is two closes and falls
                // through so the second `>` decrements again.
                if prev != b'-' && prev != b'=' && depth > 0 {
                    depth -= 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out.push(depth);
    out
}

/// Name declared by a brace-less `impl` header.
///
/// A multi-line impl keeps its `{` on the next line:
///
/// ```text
/// impl<I> Stream for Iter<I>
/// where
///     I: Stream,
/// {
/// ```
///
/// The brace path requires the `{` on the same line, so all 248 such impls in
/// tokio were invisible and `implementation` recall sat at 0.88 after the
/// other fixes. Positional: the line must start with `impl` (optionally after
/// `unsafe`/`default`), must not already end in `{`/`}`, and the declared type
/// is the last `::` segment of the token before the generic group, taken from
/// after a top-level ` for ` when one is present.
fn impl_header_without_brace(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let mut rest = t;
    for prefix in ["unsafe ", "default "] {
        if let Some(r) = rest.strip_prefix(prefix) {
            rest = r.trim_start();
        }
    }
    let rest = rest.strip_prefix("impl")?;
    // `impl` must be a whole word: `implementation` is not a header.
    if !rest
        .bytes()
        .next()
        .map(|c| !(c.is_ascii_alphanumeric() || c == b'_'))
        .unwrap_or(true)
    {
        return None;
    }
    if t.ends_with('{') || t.ends_with('}') || t.ends_with(';') {
        return None;
    }
    // A header's remaining text is a type path, optionally followed by
    // ` for <type>`. `impl Trace for X` style keywords are ordinary words.
    // Reject only when the rest is empty (a bare `impl`).
    if rest.trim().is_empty() {
        return None;
    }
    let mut r = rest.trim_start();
    // Skip the impl's own generic parameter list, honouring nesting.
    if r.starts_with('<') {
        let b = r.as_bytes();
        let mut angle = 0i32;
        let mut paren = 0i32;
        let mut end = None;
        for (i, &c) in b.iter().enumerate() {
            match c {
                b'(' => paren += 1,
                b')' if paren > 0 => paren -= 1,
                b'<' => angle += 1,
                // `->` inside a bound is not a closing angle.
                b'>' if angle > 0 && i > 0 && b[i - 1] == b'-' => {}
                b'>' if angle > 0 => {
                    angle -= 1;
                    if angle == 0 && paren == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let e = end?;
        // The end offset is ASCII-aligned, but guard anyway.
        if !r.is_char_boundary(e) {
            return None;
        }
        r = r[e..].trim_start();
    }
    // A top-level ` for ` introduces the self type.
    let target = {
        let b = r.as_bytes();
        let mut d = 0i32;
        let mut found = None;
        let mut i = 0usize;
        while i + 5 <= b.len() {
            match b[i] {
                b'<' => d += 1,
                b'>' if d > 0 => d -= 1,
                _ => {}
            }
            if d == 0 && &b[i..i + 5] == b" for " {
                found = Some(i + 5);
                break;
            }
            i += 1;
        }
        match found {
            Some(p) => &r[p..],
            None => r,
        }
    };
    // The name is the last path segment before its generic/argument group.
    let head_end = target
        .find(['<', '(', '{', ','])
        .map(|i| target[..i].len())
        .unwrap_or(target.len());
    let head = target[..head_end].trim();
    let name = head.rsplit("::").next()?.trim();
    if name.is_empty() {
        return None;
    }
    let mut chars = name.bytes();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return None;
    }
    Some(name)
}

/// Identifier immediately after the declaration keyword.
///
/// Used when the name taken from the signature paren is a keyword, which
/// happens when the chosen paren belongs to a return type
/// (`async fn f() -> impl Iterator<Item = u8>`). Positional: the keyword must
/// be a whole word followed by whitespace, and the follower must be a valid
/// identifier.
fn decl_keyword_follower(line: &str) -> Option<(usize, usize)> {
    let b = line.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        let start = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
            i += 1;
        }
        if i > start {
            let word = &line[start..i];
            if matches!(word, "fn" | "func" | "def" | "function" | "class" | "struct"
                              | "enum" | "trait" | "interface" | "type")
                && b.get(i) == Some(&b' ')
            {
                let mut j = i + 1;
                while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                    j += 1;
                }
                let ns = j;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                if j > ns {
                    return Some((ns, 0usize));
                }
            }
        }
        i = start.max(i) + 1;
    }
    None
}

/// Identifier that names the declaration whose parameter list opens at
/// `paren`, skipping a generic group between the name and the `(`.
///
/// `pub(super) fn throttle<T>(` has three `(`-adjacent identifiers (`pub`,
/// `throttle<T>`, and the params), and the old "last ident followed by `(`"
/// rule picked `pub`. `unsafe fn clone(raw: *const ())` picked `const`, from
/// the pointer type inside the parameters. Both produced a definition of the
/// wrong symbol, or none at all. Taking the name from the chosen signature
/// paren is positional and cannot drift into the parameter list.
fn name_before_paren(line: &str, paren: usize) -> Option<(usize, &str)> {
    // The identifier found this way can still be a keyword when the real name
    // is elsewhere (`async fn f() -> impl Trait` lands on `impl` only if the
    // signature paren was mis-located). Reject declaration words.

    let b = line.as_bytes();
    let mut end = paren;
    while end > 0 && (b[end - 1] == b' ' || b[end - 1] == b'\t') {
        end -= 1;
    }
    // Walk back over a `<...>` generic group.
    if end > 0 && b[end - 1] == b'>' {
        let mut d = 0i32;
        let mut i = end;
        while i > 0 {
            i -= 1;
            match b[i] {
                b'>' => d += 1,
                b'<' => {
                    d -= 1;
                    if d == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        while end > 0 && (b[end - 1] == b' ' || b[end - 1] == b'\t') {
            end -= 1;
        }
    }
    let mut start = end;
    while start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_') {
        start -= 1;
    }
    if start == end {
        return None;
    }
    let name = &line[start..end];
    if is_declarator_word_pub(name) {
        return None;
    }
    Some((start, name))
}

/// First `(` that can open a parameter list, skipping a leading qualifier
/// group.
///
/// `pub(crate) struct FastRand {` has a `(` — in the visibility qualifier —
/// so the plain "first paren" test called it a function signature, named the
/// qualifier word, and rejected it: 304 `pub(crate) struct|trait|enum`
/// declarations in tokio alone were invisible, which is most of why struct
/// recall was 0.73 and trait recall 0.44.
///
/// Positional rule: a leading `word(...)` group is a qualifier when a word
/// follows the closing paren. `pub(crate) fn close(&self) {` keeps its
/// signature paren (it follows the group); `pub(crate) struct X {` has none
/// and falls through to the brace path. `foo(x)` at line start has nothing
/// after the group, so it stays a call.
fn first_signature_paren(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    let depth = angle_depth_at(line);
    let mut i = 0usize;
    let mut skip_from = 0usize;
    let mut in_str = false;
    let mut esc = false;
    while i < b.len() {
        // A trailing comment is prose, not syntax: `pub f: Vec<T>,  // (a, b)`
        // must not supply a parameter list from inside the comment.
        if !in_str && b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            return None;
        }
        if esc { esc = false; i += 1; continue; }
        if in_str && b[i] == b'\\' { esc = true; i += 1; continue; }
        if b[i] == b'"' { in_str = !in_str; i += 1; continue; }
        if in_str {
            i += 1;
            continue;
        }
        if i < skip_from || depth.get(i).copied().unwrap_or(0) != 0 {
            i += 1;
            continue;
        }
        if b[i] != b'(' {
            i += 1;
            continue;
        }
        // Is this paren part of a leading qualifier group?
        let head = line[..i].trim_end();
        let head_is_single_word = !head.is_empty()
            && head
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'#');
        if !head_is_single_word {
            return Some(i);
        }
        // Find the matching `)`.
        let mut d = 0i32;
        let mut j = i;
        let mut close = None;
        while j < b.len() {
            if b[j] == b'(' {
                d += 1;
            } else if b[j] == b')' {
                d -= 1;
                if d == 0 {
                    close = Some(j);
                    break;
                }
            }
            j += 1;
        }
        let Some(c) = close else { return Some(i) };
        let after = line[c + 1..].trim_start();
        let word_follows = after
            .bytes()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == b'_')
            .unwrap_or(false);
        if !word_follows {
            // `foo(x)` — the group is the call itself.
            return Some(i);
        }
        // Qualifier: keep scanning after it.
        skip_from = c + 1;
        i = c + 1;
    }
    None
}

/// Is byte `pos` inside an unclosed `<...>` group?
///
/// Shape-level assertion used by the operator-lookalike tests.
#[cfg(test)]
fn inside_angle_group(line: &str, pos: usize) -> bool {
    let d = angle_depth_at(line);
    d.get(pos.min(d.len().saturating_sub(1))).copied().unwrap_or(0) > 0
}

/// Does the line begin with a local value binder (`let`, `var`, `val`)?
///
/// Only used to keep the declaration-assign rule from reading
/// `let x: Vec<i32> = {` as a type definition of `i32`. The binder is at the
/// start of the trimmed line, so this is a positional check, not a scan.
fn has_value_binder_before(line: &str) -> bool {
    let t = line.trim_start();
    for kw in ["let ", "var ", "val "] {
        if t.starts_with(kw) {
            return true;
        }
    }
    false
}

/// Leading-whitespace byte count (masking preserves byte offsets).
#[inline]
pub fn leading_indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// V83 B9: equation rule. Haskell/OCaml define functions as
/// `foo x y = body` / `let f x = body`. The existing `=`-rules water every
/// top-level assignment (V66c) so `let bg = callee()` cannot type its
/// callee. Positional discriminator, on an indent-0 line whose prefix
/// before the top-level `=` is a whitespace-separated run of pure
/// identifier words only: skip leading declaration words with
/// [`crate::keywords::is_keyword`] plus `rec` (OCaml/ML recursive binder)
/// so `let f x =` names `f`; the first remaining word is the candidate
/// name; and at least one further word must follow the name — `foo x =`
/// is an equation (arguments follow the name) whereas `let bg =` and
/// `int x =` are plain bindings (the name is last) and `x = 5` is a
/// single word. Separators (`.`, `:`, `,`, brackets) bail: qualified
/// names, annotations and tuples are not equations. Two gates keep
/// locals out: indent 0 (OCaml/Haskell `let` inside a function is
/// indented) AND brace depth 0 (a `Foo x = new Foo` declaration inside a
/// C#/Rust/Java method body sits at depth >= 1 even when the source line
/// itself starts at column 0, e.g. inside `#[cfg(test)]` fixtures).
/// Binder words of the equation/binding families: `rec` (OCaml/ML
/// recursive binder), `val` (OCaml/Scala/Kotlin), `fun` (SML), `and`
/// (ML simultaneous bindings). Not folded into the shared keyword list
/// because they are ordinary identifiers in other languages.
#[inline]
fn is_binder_word(w: &str) -> bool {
    w == "rec" || w == "val" || w == "fun" || w == "and"
}

/// Words that may precede the bound name in a declaration without being it:
/// binder words plus the shared declaration/type predicates (`export`,
/// `const`, `type`, `class`, `fn`, ... — several are not in the keyword
/// list because they are ordinary identifiers in other languages).
#[inline]
fn is_declarator_word(w: &str) -> bool {
    is_binder_word(w)
        || crate::keywords::is_keyword(w)
        || has_decl_keyword_before(w)
        || has_type_keyword_before(w)
}

/// Public form of [`is_declarator_word`] for the source-likeness heuristic.
#[inline]
pub fn is_declarator_word_pub(w: &str) -> bool {
    is_declarator_word(w)
}

/// Declaration MODIFIERS only: binders plus the shared declaration/type
/// keyword predicates. Unlike [`is_declarator_word`] this excludes plain
/// keywords, so a keyword-shaped FUNCTION NAME (`let raise t = ...`,
/// `let await (req, resp) = ...`, `let enum variants json = ...` in OCaml,
/// where Rust keywords are ordinary identifiers) is not skipped over:
/// the name-selection loop in the equation path uses this predicate.
/// Modifiers (`const`, `export`, `static`, ...) stay skipped, so
/// `export const bar = (a, b) => {` still resolves through the arrow path.
#[inline]
fn is_decl_modifier(w: &str) -> bool {
    is_binder_word(w) || has_decl_keyword_before(w) || has_type_keyword_before(w)
}

pub fn equation_name(line: &str, depth: i32) -> Option<&str> {
    if depth != 0 {
        return None;
    }
    let eq = top_level_assign_eq(line)?;
    let prefix = line[..eq].trim_end();
    if prefix.is_empty() {
        return None;
    }
    let rhs = &line[eq + 1..];
    // Expression-lets (`let f x = x in body`) are not definitions; ctags
    // agrees (`let inner y = y in` in the probe is not a def). Same-line,
    // top-level standalone `in` only — a masked string `"in"` yields quote
    // fragments, never the word.
    let rhs_has_in = rhs.split_whitespace().any(|w| w == "in");
    let pure = prefix
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b' ' || b == b'\t');

    // Path A (strict non-binder equations): pure identifier words only,
    // the name follows any leading binders, and at least one ARGUMENT
    // word must follow it — `foo x =` is an equation, `bg = f()` is not.
    if pure {
        let mut name: Option<&str> = None;
        let mut args_after_name = 0usize;
        let mut binder_led = false;
        for w in prefix.split_whitespace() {
            if name.is_none() && is_declarator_word(w) {
                binder_led = true;
                continue;
            }
            if name.is_none() {
                name = Some(w);
            } else {
                args_after_name += 1;
            }
        }
        if let Some(name) = name {
            let lower = !name
                .as_bytes()
                .first()
                .map(|b| b.is_ascii_uppercase())
                .unwrap_or(true);
            if is_valid_identifier(name)
                && args_after_name > 0
                && lower
                && !rhs_has_in
                && (leading_indent(line) == 0 || binder_led)
            {
                return Some(name);
            }
        }
    }

    // Path B (binder-led, relaxed arguments): `let create () =`,
    // `let recv (i, _) =`, `let map t ~f =`, `let to_dyn f { n; d } =`,
    // `and type_field fields =`. The binder word licenses the shape: no
    // brace language writes `let name args = ...` (there a `let` binding
    // always has the name LAST, `let x = f()`), so a binder followed by a
    // name is an equation; with zero following words it is a module-level
    // binding (`let value = 42`), which ctags counts as a definition too.
    // OCaml module bodies indent their `let`s (1626 of 3149 ocaml-lsp
    // function sites), hence no indent gate beyond the binder itself.
    // The `in` guard still waters expression-lets; the PascalCase gate
    // keeps type-led declarations on the binding path. After a value
    // binder only `rec`/`mut`/`ref` may precede the name (see below).
    let mut it = prefix.split_whitespace();
    let first = it.next()?;
    if !is_declarator_word(first) {
        return None;
    }
    // After a VALUE binder (`let`/`def`/ML binders) the only words that
    // can precede the name are `rec` and Rust's `mut`/`ref` — OCaml writes
    // nothing else there, so a keyword-shaped second word IS the function
    // name (`let enum variants json =`). After a VISIBILITY modifier
    // (`export`/`static`/`pub`/...) the further modifiers still need
    // skipping (`export const X = ...`). The set is positional and tiny.
    let value_binder_first = is_binder_word(first) || first == "let" || first == "def";
    let mut name: Option<&str> = None;
    let mut args_after_name = 0usize;
    for w in it {
        let skip = if value_binder_first {
            is_binder_word(w) || w == "mut" || w == "ref"
        } else {
            is_decl_modifier(w) || w == "mut" || w == "ref"
        };
        if name.is_none() && skip {
            continue;
        }
        if name.is_none() {
            // The name must be a single valid identifier token (a
            // destructuring pattern `(a, b)` yields none). A
            // keyword-shaped token IS admitted here: Rust keywords
            // (`raise`, `await`, `enum`) are ordinary function names in
            // OCaml (`let raise t = raise (E t)`), and the Rust
            // `let mut x = ...` shape is already excluded by the
            // binding-modifier skip above plus the argument-count gate.
            if !w.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || !w.as_bytes().first().map(|b| b.is_ascii_alphabetic() || *b == b'_').unwrap_or(false)
            {
                continue;
            }
            name = Some(w);
        } else {
            args_after_name += 1;
        }
    }
    let name = name?;
    // Arguments must follow the name. ctags agrees: `let value = 42`
    // (zero args) is NOT a def in .ml, and a zero-arg `let fmt = ...`
    // inside a Rust body must stay local_var.
    if !is_valid_identifier(name)
        || args_after_name == 0
        || name
            .as_bytes()
            .first()
            .map(|b| b.is_ascii_uppercase())
            .unwrap_or(true)
        || rhs_has_in
    {
        return None;
    }
    Some(name)
}

/// V84 P-D: point-free / zero-argument definitions at module scope.
///
/// `arrayValues = List.map f xs` (Haskell), `default = Settings 0` (Haskell
/// record), `MAX_RETRIES = 5` (Python module constant). These are real
/// definitions that ctags counts (`function`/`variable` kinds), but the
/// equation rules above require an argument after the name to distinguish a
/// definition from a local binding (`let bg = f()`).
///
/// Positional guards, each addressing a measured false-positive class:
///   - INDENT must be 0. A statement in a function body is indented; a
///     module-level binding is not. This is what separates `X = 5` at the
///     top of a file from `x = 5` inside a Rust/JS body.
///   - The line must NOT be binder-led (`let`/`const`/`var`/...). Those are
///     Rust/JS/OCaml value bindings; ctags does not count them as function
///     definitions, and `let fmt = format!(...)` inside a body must stay a
///     local. Path B keeps the binder-led shape with arguments only.
///   - The prefix must be ONE identifier word: no dots (`a.b = x`), no
///     indexing (`a[i] = x`), no type annotations (`x: T = ...` — the
///     declaration rules handle those).
///   - The name is lowercase or SCREAMING_SNAKE, never PascalCase (a
///     PascalCase head is a type alias or constructor, handled elsewhere).
///   - The right-hand side must be non-empty.
///
/// Data files are handled before classification: `is_known_data_ext` skips
/// them at walk time and `is_source_like` rejects binding-only files, so a
/// TOML/INI file never reaches this rule.
pub fn pointfree_name(line: &str, depth: i32) -> Option<&str> {
    if depth != 0 {
        return None;
    }
    if leading_indent(line) != 0 {
        return None;
    }
    let eq = top_level_assign_eq(line)?;
    let prefix = line[..eq].trim_end();
    let rhs = line[eq + 1..].trim();
    if prefix.is_empty() || rhs.is_empty() {
        return None;
    }
    // A trailing `;` marks a STATEMENT in brace languages (`total =
    // compute();` at file scope is an assignment, and C-family definitions
    // are binder-led anyway). Point-free definitions are expression-bound:
    // Haskell has no `;`, Python has no `;` after a binding, and a
    // SCREAMING constant with a semicolon is a `const`/`static` declaration
    // handled by the declaration rules.
    if rhs.ends_with(';') {
        return None;
    }
    // Exactly one identifier word, nothing else.
    let mut it = prefix.split_whitespace();
    let name = it.next()?;
    if it.next().is_some() {
        return None;
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || !name
            .as_bytes()
            .first()
            .map(|b| b.is_ascii_alphabetic() || *b == b'_')
            .unwrap_or(false)
    {
        return None;
    }
    if !is_valid_identifier(name) {
        return None;
    }
    // Binder-led lines are value bindings, not point-free definitions.
    if is_declarator_word(name) || is_decl_modifier(name) {
        return None;
    }
    // PascalCase is a type/constructor. Mixed Case (camelCase) is allowed:
    // Haskell/OCaml functions and JS module values use it.
    if name
        .as_bytes()
        .first()
        .map(|b| b.is_ascii_uppercase())
        .unwrap_or(true)
        && !name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        return None;
    }
    Some(name)
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

/// Blank out string/char literal contents (preserving byte length and column
/// positions) so callers that tokenize identifiers do not index names that
/// appear only inside a string. Grammar-free: a quote toggles string state;
/// `//` and `#` comments are also removed.
///
/// Motivation: `panic!("call_me() failed")` contains a call-shaped token that
/// is not a call site. Indexing it inflated caller lists for every language
/// (found via the llama.cpp bench: `write_vocab` inside an error string was
/// reported as a caller).
/// B1-B3: preprocessor island. `#`-leading lines are water for every other
/// path, but three directive forms carry name information worth indexing:
/// `#define NAME` / `#define NAME(params) body` (a definition plus body
/// usages), `#ifdef/#ifndef/#undef NAME` and `#if/#elif defined(NAME)`
/// (usage references). Everything else (`#include`, `#pragma`, `#error`,
/// `#line`, `#else`, `#endif`, bare `#if EXPR`) stays water.
///
/// Grammar-free: the single literal `define` gates one directive form (same
/// class as the `def`/`fn` words already in the decl lists); everything
/// after it is positional (skip whitespace, read ident, check adjacency).
/// `ifdef`/`ifndef`/`undef`/`defined` are routing words for the usage-only
/// forms, never definitions, never scoring.
pub enum Preproc<'a> {
    /// `#define NAME` or `#define NAME(params) body`. Params must be
    /// excluded from body tokens (B3).
    Def { name: &'a str, params: Vec<&'a str>, variadic: bool },
    /// Usage-only directive. Names are tokenized but never defined.
    Uses,
}

fn preproc_read_ident(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    if b.is_empty() || !(b[0].is_ascii_alphabetic() || b[0] == b'_') {
        return None;
    }
    let mut i = 1;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
        i += 1;
    }
    Some(&s[..i])
}

fn preproc_read_word(s: &str) -> (&str, &str) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
        i += 1;
    }
    (&s[..i], &s[i..])
}

pub fn classify_preprocessor_line(line: &str) -> Option<Preproc<'_>> {
    let t = line.trim_start();
    let rest = t.strip_prefix('#')?;
    // Rust attributes (`#[`) and raw-string prefixes (`#"`) are not
    // preprocessor lines — same carve-out as the strip functions.
    if rest.starts_with('[') || rest.starts_with('"') {
        return None;
    }
    let (word, after) = preproc_read_word(rest.trim_start());
    if word == "define" {
        let after = after.trim_start();
        let name = preproc_read_ident(after)?;
        let after_name = &after[name.len()..];
        // Function-like iff `(` is IMMEDIATELY adjacent (per cpp spec;
        // `#define F (x)` is object-like with a paren body).
        if let Some(inner) = after_name.strip_prefix('(') {
            // Parse the single-level param group.
            let mut params: Vec<&str> = Vec::new();
            let mut variadic = false;
            let b = inner.as_bytes();
            let mut i = 0;
            let mut tok_start: Option<usize> = None;
            while i < b.len() {
                let c = b[i];
                if c == b')' {
                    break;
                } else if c == b',' {
                    if let Some(s) = tok_start.take() {
                        let tok = inner[s..i].trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
                        if !tok.is_empty()
                            && tok != "..."
                            && preproc_read_ident(tok).is_some_and(|id| id.len() == tok.len())
                        {
                            params.push(tok);
                        }
                    }
                } else if c.is_ascii_alphanumeric() || c == b'_' {
                    if tok_start.is_none() {
                        tok_start = Some(i);
                    }
                } else {
                    if c == b'.' {
                        variadic = true;
                    }
                    if let Some(s) = tok_start.take() {
                        let tok = inner[s..i].trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
                        if !tok.is_empty()
                            && tok != "..."
                            && preproc_read_ident(tok).is_some_and(|id| id.len() == tok.len())
                        {
                            params.push(tok);
                        }
                    }
                }
                i += 1;
            }
            if let Some(s) = tok_start {
                let tok = inner[s..i].trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
                if !tok.is_empty()
                    && tok != "..."
                    && preproc_read_ident(tok).is_some_and(|id| id.len() == tok.len())
                {
                    params.push(tok);
                }
            }
            Some(Preproc::Def { name, params, variadic })
        } else {
            Some(Preproc::Def { name, params: Vec::new(), variadic: false })
        }
    } else if word == "ifdef" || word == "ifndef" || word == "undef" {
        Some(Preproc::Uses)
    } else if word == "if" || word == "elif" {
        // Only `defined(NAME)` forms; bare `#if EXPR` stays water (no
        // shape separates its names from operators without a list).
        if after.contains("defined") {
            Some(Preproc::Uses)
        } else {
            None
        }
    } else {
        None
    }
}

/// Identifier-tokenization entry point. For ordinary lines this is
/// `strip_strings_and_comments`. For preprocessor islands it returns the
/// tokenizable remainder instead of nothing:
///
/// - `#define NAME body` becomes `NAME body` (NAME binds via the
///   classifier's `defined_name`; body idents become usages).
/// - `#define NAME(p1,p2) body` becomes `NAME body-minus-params` (B3:
///   params are blanked by word-boundary span so `a`/`b` never file as
///   usages; `__VA_ARGS__` is blanked when variadic).
/// - `#ifdef/#ifndef/#undef NAME` and `#if defined(NAME)` become the line
///   with the directive word blanked (usages only; the classifier never
///   defs these).
/// - Everything else becomes `strip_strings_and_comments` output
///   (unchanged behavior).
///
/// Use ONLY at identifier-tokenization sites (ingest + JIT). Brace-graph,
/// block, and comment paths keep calling `strip_strings_and_comments`.
/// Tokenizable code with CROSS-LINE literal state.
///
/// `tokenizable_code` is per-line: it resets its lexer on every call, so it
/// cannot know that a line is in the middle of a Python docstring or a
/// JS template literal. Every line of a docstring was therefore tokenized as
/// code, and prose words became call sites: in `psf/requests`, `request`
/// picked up 241 docstring occurrences (`>>> import requests`,
/// `:param request: ...`), which is why usage precision was 0.48 there while
/// it was 0.99 on Rust.
///
/// This variant carries `st` across calls. Callers reset it once per file.
pub fn tokenizable_code_with(line: &str, st: &mut crate::lex::LexState) -> String {
    let t = line.trim_start();
    if !t.starts_with('#') {
        return crate::lex::masked_line(line, st, true);
    }
    // Preprocessor handling is per-line by nature; keep it, but still advance
    // the cross-line state so a directive cannot desync the literal tracker.
    let _ = crate::lex::starts_in_code(line, st, true);
    tokenizable_code(line)
}

pub fn tokenizable_code(line: &str) -> String {
    let t = line.trim_start();
    if !t.starts_with('#') {
        return strip_strings_and_comments(line);
    }
    match classify_preprocessor_line(line) {
        None => strip_strings_and_comments(line),
        Some(Preproc::Uses) => {
            // Keep only what follows the directive word; blank `defined`
            // so it never files as a usage. The leading `#word` prefix is
            // dropped entirely (else the strip pass below would treat the
            // surviving `#` as a comment start and erase the names).
            let hash = line.find('#').unwrap_or(0);
            let after_hash = &line[hash + 1..];
            let ws = after_hash.len() - after_hash.trim_start().len();
            let wstart = hash + 1 + ws;
            let (word, _) = preproc_read_word(&line[wstart..]);
            let mut out = line[wstart + word.len()..].to_string();
            // Also blank `defined` so it never files as a usage.
            blank_word(&mut out, "defined");
            // String literals in the remainder (e.g. `#error "msg"`)
            // must not tokenize.
            strip_strings_and_comments(&out)
        }
        Some(Preproc::Def { name, params, variadic }) => {
            // Rebuild: NAME + body with param spans blanked.
            let hash = line.find('#').unwrap_or(0);
            let after_hash = &line[hash + 1..];
            let ws = after_hash.len() - after_hash.trim_start().len();
            // Skip `define` word.
            let dstart = hash + 1 + ws;
            let (_, after_define) = preproc_read_word(&line[dstart..]);
            let npos = dstart + "define".len();
            let nstart = npos + (after_define.len() - after_define.trim_start().len());
            let body_start = nstart + name.len();
            let body = &line[body_start.min(line.len())..];
            // Skip the param group for function-like macros ONLY
            // (immediately-adjacent `(`). Per cpp spec `#define F (x)` is
            // object-like: `(x)` is body, and `x` is a usage, not a param.
            let func_like = body.starts_with('(');
            let mut code = String::with_capacity(line.len());
            code.push_str(name);
            code.push(' ');
            let mut rest = body;
            if func_like {
                if let Some(end) = rest.find(')') {
                    rest = &rest[end + 1..];
                }
            }
            // Blank param idents by word-boundary span.
            let mut out = rest.to_string();
            if variadic {
                blank_word(&mut out, "__VA_ARGS__");
            }
            for p in params.iter() {
                let word: &str = p;
                blank_word(&mut out, word);
            }
            code.push_str(&strip_strings_and_comments(&out));
            code
        }
    }
}

/// Blank every word-boundary occurrence of `word` in `out` (span-preserving,
/// so columns of surviving tokens do not shift).
fn blank_word(out: &mut String, word: &str) {
    let mut search = 0;
    while let Some(f) = out[search..].find(word) {
        let a = search + f;
        let e = a + word.len();
        let bb = out.as_bytes();
        let before_ok = a == 0 || (!bb[a - 1].is_ascii_alphanumeric() && bb[a - 1] != b'_');
        let after_ok = e >= out.len() || (!bb[e].is_ascii_alphanumeric() && bb[e] != b'_');
        if before_ok && after_ok {
            out.replace_range(a..e, &" ".repeat(word.len()));
        }
        search = e;
    }
}

pub fn strip_strings_and_comments(line: &str) -> String {
    // V81: one grammar-free cross-language lexer (crate::lex). String and
    // comment DELIMITERS are preserved and their INTERIORS blanked, so
    // header shapes and byte columns stay stable while no literal or
    // comment content can look like code. Covers `//`, `/* */`, `#`,
    // `"…"`, `'…'`, Rust raw `r#"…"#`, C# verbatim `@"…"`, plus the
    // previously-missed `"""…"""`, `'''…'''`, and `` `…` `` on one line.
    // (Per-line: multi-line interiors are tracked by `count_braces` and
    // the brace graph, which carry state across lines.)
    let mut st = crate::lex::LexState::new();
    crate::lex::masked_line(line, &mut st, true)
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
///
/// V92: single-character identifiers are valid. The floor was 2, so
/// `def u(s):` — a real function in `requests/tests/compat.py` — never
/// classified, and `x`/`y`/`i` definitions were invisible in every language
/// that uses them. One-letter names are common in Python, Go and shell.
fn is_valid_identifier(s: &str) -> bool {
    if s.is_empty() { return false; }
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
        let b = rest.as_bytes();
        let mut depth = 1i32;
        let mut paren = 0i32;
        let mut i = 1;
        let mut end = rest.len();
        while i < b.len() && depth > 0 {
            match b[i] {
                b'(' => paren += 1,
                b')' if paren > 0 => paren -= 1,
                b'<' => depth += 1,
                // `->` in a bound (`FnOnce() -> O`) is an operator, not a closer.
                b'>' if i > 0 && (b[i - 1] == b'-' || b[i - 1] == b'=') => {}
                b'>' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let _ = paren;
        if end > rest.len() {
            return None;
        }
        rest[end..].trim_start()
    } else {
        rest
    };
    // Find `for` clause — the declared type is the self type after it.
    if let Some(for_idx) = rest.find(" for ") {
        return self_type_name(&rest[for_idx + 5..]);
    }
    // No `for`: the self type is what follows `impl` and its generics.
    self_type_name(rest)
}

/// Name of the self type in an `impl` header's type expression.
///
/// `impl From<Elapsed> for std::io::Error {` used to record `std` (the first
/// path segment) and `impl Read for &'_ MockFile {` recorded `_`. Both are
/// ctags `implementation` sites, and tokio has 73 such impls, so the
/// `implementation` recall gap was almost entirely this.
///
/// Positional: skip leading `&`, a lifetime, `mut`, `dyn`; take the last
/// `::` segment before any generic or argument group. A tuple self type
/// (`impl X for ()`) has no name.
fn self_type_name(s: &str) -> Option<&str> {
    // A trailing `where` clause is a constraint, not part of the type.
    let s = match s.find(" where ") {
        Some(i) => &s[..i],
        None => s,
    };
    let b = s.as_bytes();
    let mut i = 0usize;
    loop {
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        match b.get(i) {
            Some(b'&') => {
                i += 1;
                // Skip a lifetime: `'a`, `'_`.
                if b.get(i) == Some(&b'\'') {
                    i += 1;
                    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                        i += 1;
                    }
                }
                continue;
            }
            Some(b'\'') => {
                i += 1;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        // `mut` / `dyn` as whole words.
        for word in ["mut", "dyn"] {
            if b[i..].starts_with(word.as_bytes())
                && b.get(i + word.len())
                    .map(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
                    .unwrap_or(true)
            {
                i += word.len();
                continue;
            }
        }
        break;
    }
    if i >= b.len() {
        return None;
    }
    let rest = &s[i..];
    // Cut at the first generic/argument group; the name precedes it.
    let cut = rest
        .find(['<', '(', '[', '{', ','])
        .unwrap_or(rest.len());
    let head = rest[..cut].trim_end();
    if head.is_empty() {
        return None;
    }
    let name = head.rsplit("::").next()?.trim();
    if name.is_empty() {
        return None;
    }
    let mut ch = name.bytes();
    let first = ch.next()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    if !ch.all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return None;
    }
    if name.chars().all(|c| c == '_') {
        return None;
    }
    Some(name)
}

fn after_for_identifiers(s: &str) -> Option<&str> {
    // `impl` headers now use `self_type_name` so a path or reference resolves
    // to its last segment rather than its first.
    self_type_name(s)
}

#[cfg(test)]
mod tests {
    /// V86: a `(` inside an unclosed `<...>` is a generic argument, not a
    /// parameter list. `pub symbols_added: Vec<(String, String, i32)>,` was a
    /// function signature naming `Vec`, so `def_only("vec")` answered with
    /// struct fields.
    #[test]
    fn generic_argument_parens_are_not_a_parameter_list() {
        for line in [
            "    pub symbols_added: Vec<(String, String, i32)>,   // (name, line)",
            "    pub edges_added: Vec<(String, String)>,",
            "    field: HashMap<String, Vec<(u32, u32)>>,",
            // A brace after a closure's closing `|` opens a closure body.
            "        seen: &mut std::collections::HashSet<(String, String, i32)>| {",
        ] {
            let r = classify_structural(line, 1, false, false);
            assert!(!r.is_def, "{line:?} must not be a definition, got {r:?}");
        }
        // A paren AFTER the generic list closes is still a parameter list.
        for (line, name) in [
            ("pub fn foo<T>(x: T) -> T {", "foo"),
            ("fn beta(x: i32) -> String {", "beta"),
            ("fn map<B, F: FnOnce(A) -> B>(self, f: F) -> Option<B> {", "map"),
            ("    pub fn block_on<F: Future>(&self, future: F) -> F::Output {", "block_on"),
            // `>>` closes two groups; treating it as an operator left the depth
            // stuck and the line was read as a brace definition of `Vec`.
            ("pub fn segment_lines<S: AsRef<str>>(lines: &[S], events: &[i64]) -> Vec<Segment> {", "segment_lines"),
        ] {
            let r = classify_structural(line, 0, true, false);
            assert!(r.is_def, "{line:?} must still be a definition");
            assert_eq!(r.defined_name, Some(name));
        }
        // Operator lookalikes must not count as brackets.
        assert!(!inside_angle_group("a << b,", 6));
        assert!(!inside_angle_group("if x >= y,", 8));
    }

    /// V86: an assignment whose value contains a call is an expression.
    /// `text = ... else str(resp)` used to define `str`, so `def_only("str")`
    /// answered with a Python benchmark script.
    #[test]
    fn assignment_with_call_is_not_a_definition() {
        let l = "        text = resp[\"choices\"][0][\"message\"][\"content\"] if isinstance(resp, dict) else str(resp)";
        let r = classify_structural(l, 2, false, false);
        assert!(!r.is_def, "assignment must not define a symbol, got {r:?}");
        // Declarations that carry a call still work.
        for (line, name) in [
            ("pub fn alpha() -> i32 {", "alpha"),
            ("fn beta(x: i32) -> String {", "beta"),
            ("def gamma(x):", "gamma"),
        ] {
            let r = classify_structural(line, 0, true, false);
            assert!(r.is_def, "{line:?} must still be a definition");
            assert_eq!(r.defined_name, Some(name));
        }
    }

    /// V86: a line that starts with `)` continues a signature opened above.
    /// `) -> String {` used to define `String` (the return type), which made
    /// `find_references("str")` answer with unrelated functions.
    #[test]
    fn signature_continuation_never_defines_the_return_type() {
        for line in [") -> String {", ") -> Result<Vec<i32>, Error> {", ");", ") {"] {
            let r = classify_structural(line, 0, true, false);
            assert!(
                !r.is_def,
                "{line:?} is a continuation and must not define anything, got {r:?}"
            );
        }
        // The line that actually names the function still defines it.
        let r = classify_structural("pub fn alpha(", 1, false, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("alpha"));
    }

    /// V86: `let ones: Vec<i32> = {` is a local binding. The declaration-assign
    /// rule tagged `i32` (the annotation, after the colon) as a type definition
    /// because the value starts with `{`, routing the line down the block path.
    /// It also made `find_references("i32", def_only)` answer with a function.
    #[test]
    fn let_binding_with_annotation_is_not_a_type_definition() {
        for line in [
            "let ones: Vec<i32> = {",
            "        let ones: Vec<i32> = {",
            "let acc: HashMap<String, i32> = {",
            "var x: Foo = {",
        ] {
            let r = classify_structural(line, 1, true, false);
            assert!(
                !(r.is_def && r.defined_name == Some("i32")),
                "{line:?} must not define i32, got {r:?}"
            );
        }
        // The declaration forms the rule exists for still work.
        let r = classify_structural("type Alias = Thing;", 0, false, false);
        assert!(r.is_def, "type alias must still be a definition");
        assert_eq!(r.defined_name, Some("Alias"));
        let r = classify_structural("const EXECUTOR: Lazy<Runtime> = Lazy::new();", 0, false, false);
        assert_eq!(r.defined_name, Some("EXECUTOR"));
    }

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
        // V87: the old contract accepted `except Exception:` as a type_def
        // because `Exception` is PascalCase (<0.1% of lines). The fraction was
        // fine; the symbol was not — `Exception` is a common query and it
        // answered with 153 `except` clauses presented as definitions.
        // Control flow is control flow: no definition.
        for line in [
            "except Exception:",
            "except ValueError as e:",
            "except:",
            "except* TypeError:",
            "elif x == 1:",
            "finally:",
        ] {
            let r = classify_structural(line, 1, false, false);
            assert!(!r.is_def, "{line:?} must not define anything, got {r:?}");
        }
        // The rule only removes control flow; real declarations still type.
        let r = classify_structural("class Foo:", 0, false, false);
        assert_eq!(r.tag, 2);
        assert_eq!(r.defined_name, Some("Foo"));
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
    fn namespace_is_not_a_definition() {
        // V80: `namespace App {` is a scope container, not a symbol.
        let r = classify_structural("namespace App {", 0, true, false);
        assert!(!r.is_def, "a namespace must not be a definition");
        // Negative control: a class in the same position still is.
        let c = classify_structural("class App {", 0, true, false);
        assert!(c.is_def);
        assert_eq!(c.defined_name, Some("App"));
    }

    #[test]
    fn test_csharp_type_declarations() {
        // V78: the dominant .NET/Java styles where the declaration line ends in
        // neither `{`, `(`, nor `:`.
        for (line, name) in [
            ("public class V1Controller : ControllerBase", "V1Controller"),
            ("public partial class V2Controller", "V2Controller"),
            ("public sealed class V3Controller<T> where T : class", "V3Controller"),
            ("public class V4Controller", "V4Controller"),
            ("public interface IThing", "IThing"),
            ("public enum Color", "Color"),
            ("public struct Point", "Point"),
            ("public record Person", "Person"),
        ] {
            let r = classify_structural(line, 0, false, false);
            assert!(r.is_def, "should be a definition: {}", line);
            assert_eq!(r.tag, 2, "should be a type_def: {}", line);
            assert_eq!(r.defined_name, Some(name), "wrong name for: {}", line);
        }
    }

    #[test]
    fn test_csharp_type_declaration_negative_controls() {
        // Guard 1: the keyword must be a whole word — `Datatype`, `prototype`,
        // `subclass` are not declarations.
        assert!(!classify_structural("Datatype x", 0, false, false).is_def);
        assert!(!classify_structural("prototype y", 0, false, false).is_def);
        // Guard 2: a *call* to a lowercase method is not a TYPE declaration.
        // V84 P-D: at depth 0 this is a module-level binding definition
        // (ctags kind:variable — 141 such sites in the Python corpus, 11 with
        // a call RHS), so it is a def with tag 1, never tag 2.
        let r = classify_structural("value = compute()", 0, false, false);
        assert_ne!(r.tag, 2, "a lowercase call assignment is not a type decl");
        assert_eq!(r.defined_name, Some("value"));
        // Inside a body it is a local binding, not a definition.
        let r = classify_structural("value = compute()", 3, false, false);
        assert!(!r.is_def, "indented/depth>0 assignment must stay local");
        // Guard 3: depth >2 (inside a method body) never yields a type decl.
        assert!(!classify_structural("Foo x = new Foo", 3, false, false).is_def);
        // Guard 4: an assignment to a field named like a type is a binding, not
        // a type declaration (and is_def=false regardless).
        let r = classify_structural("Type2 z = a.Type", 0, false, false);
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
    fn test_decl_assign_typedef_is_def() {
        // V83 B1: `type X = ...` is a definition, not a local binding.
        let r = classify_structural(
            "type AttributeArgs = syn::punctuated::Punctuated<syn::Meta, syn::Token![,]>;",
            0, false, false);
        assert!(r.is_def, "type alias must be a definition");
        assert_eq!(r.defined_name, Some("AttributeArgs"));
        // Negative control: lowercase reassignment stays a local binding.
        let r = classify_structural("park = compute();", 1, false, false);
        assert!(!r.is_def, "lowercase assignment must stay a local binding");
        assert_eq!(r.tag, 6);
    }

    #[test]
    fn test_decl_assign_const_static() {
        // V83 B2: `const EXECUTOR: T = ...` binds EXECUTOR, not the
        // annotation word. Negative control: lowercase annotation.
        let r = classify_structural(
            "const EXECUTOR: Lazy<Runtime> = Lazy::new(|| Runtime::new().unwrap());",
            0, false, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("EXECUTOR"));
        let r = classify_structural("static VERSION: &str = \"1.0\";", 0, false, false);
        assert!(r.is_def, "SCREAMING_CASE static must be a definition");
        assert_eq!(r.defined_name, Some("VERSION"));
        let r = classify_structural("let mut park: Vec<u8> = Vec::new();", 2, false, false);
        assert!(!r.is_def, "`let` binding with lowercase name stays a local");
    }

    #[test]
    fn test_decl_assign_excludes_comparisons_and_arrows() {
        // Operator-adjacent `=` is not an assignment.
        for line in [
            "if MAX_SIZE == limit {",
            "let ok = a >= b;",
            "const F = (x) => x + 1;",
        ] {
            let r = classify_structural(line, 1, false, false);
            assert!(
                !(r.is_def && r.defined_name == Some("MAX_SIZE")),
                "comparison/arrow must not bind through decl_assign_name: {line}"
            );
        }
    }

    #[test]
    fn test_decl_assign_rust_field_assignment_not_def() {
        // Rust struct-literal fields: `Self { count: 3 }` and
        // `count: usize,` must not become defs via the annotation colon.
        let r = classify_structural("            data,", 2, false, false);
        assert!(!r.is_def);
        // V95: `Self {` inside a body is a struct LITERAL. This test used to
        // assert is_def=true, encoding the bug that made 357 call sites
        // invisible to `usage_only` (a definition row is filtered out of the
        // caller list). At depth 0/1 the same line does not occur: the header
        // there is `impl Foo {`, which is handled by the impl path.
        let r = classify_structural("        Self {", 2, true, false);
        assert!(!r.is_def, "Self {{ at body depth is a struct literal, not a definition");
        // The real declaration shapes still classify, including nested ones
        // (macro bodies and functions inside functions).
        for line in [
            "struct Foo {",
            "            struct Local {",
            "pub(crate) struct Qualified {",
            "        impl Bar for Baz {",
        ] {
            let r = classify_structural(line, 2, true, false);
            assert!(r.is_def, "{line} must stay a definition");
        }
    }

    /// V98: a declaration with a populated body on one line.
    ///
    /// Found by the 0.9.1 install smoke test: `pub struct Alpha { pub tag: u8 }`
    /// was water because only a brace at END of line opened the block branch.
    #[test]
    fn v98_one_line_declaration_with_body() {
        let r = classify_structural("pub struct Alpha { pub tag: u8 }", 0, false, false);
        assert!(r.is_def, "one-line struct with a body must be a definition");
        assert_eq!(r.defined_name, Some("Alpha"));
        let r = classify_structural("pub enum E { A, B }", 0, false, false);
        assert_eq!(r.defined_name, Some("E"));
        // Control flow with a populated body must stay water.
        for line in ["if x { y(); }", "match v { A => 1 }", "while go { step() }"] {
            assert!(!classify_structural(line, 1, false, false).is_def, "{line:?}");
        }
    }

    /// V95/V97: the false-definition reject and the exact-token filter.
    ///
    /// A definition row that is not a definition is worse than a miss:
    /// `usage_only` filters definition sites out of the caller list, so each
    /// one silently HIDES a real call site. Measured on identifier-scrambled
    /// tokio, 1,133 of 5,666 tag-2 rows were struct literals, macro
    /// invocations, `use` groups or `unsafe {` blocks.
    #[test]
    fn v95_bare_brace_never_declares() {
        for (line, depth) in [
            ("        Z5jv0zc {", 3),
            ("    Self {", 2),
            ("tokio::select! {", 1),
            ("use tokio::{", 0),
            ("unsafe {", 1),
            ("async move {", 1),
        ] {
            let r = classify_structural(line, depth, true, false);
            assert!(!r.is_def, "{line:?} must not be a definition");
        }
        for (line, depth) in [
            ("struct Foo {", 0),
            ("pub(crate) struct Qualified {", 0),
            ("        struct Local {", 2),
            ("impl Bar for Baz {", 0),
        ] {
            let r = classify_structural(line, depth, true, false);
            assert!(r.is_def, "{line:?} must stay a definition");
        }
        // A macro declares the token after the bang, not `macro_rules`.
        let r = classify_structural("macro_rules! check_poll {", 0, true, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("check_poll"));
    }

    /// V95: brace-shaped lines that are not type declarations must not be
    /// definition rows. A false definition is worse than a miss: `usage_only`
    /// filters definition sites out of the caller list, so each one silently
    /// hides a real call site. Every case below was measured on
    /// identifier-scrambled tokio (1,133 of 5,666 tag-2 rows).
    #[test]
    fn test_v95_non_declaration_brace_shapes() {
        let water = [
            ("        Z5jv0zc {", 3),          // R1: struct literal in a body
            ("    Self {", 2),                 // R1/R4: Self literal
            ("tokio::select! {", 1),           // R2: macro invocation
            ("        tokio::mac! {", 2),      // R2 at depth
            ("use tokio::{", 0),               // R3: import group
            ("    pub use crate::x::{", 1),    // R3 with a qualifier
            ("unsafe {", 1),                   // R4: lone identifier
            ("loop {", 1),
            ("async move {", 1),
        ];
        for (line, depth) in water {
            let r = classify_structural(line, depth, true, false);
            assert!(!r.is_def, "{line:?} at depth {depth} must not be a definition");
        }
        let defs = [
            ("struct Foo {", 0),
            ("pub(crate) struct Qualified {", 0),
            ("        struct Local {", 2),     // nested but declared
            ("impl Bar for Baz {", 0),
            ("    impl Bar for Baz {", 1),
        ];
        for (line, depth) in defs {
            let r = classify_structural(line, depth, true, false);
            assert!(r.is_def, "{line:?} at depth {depth} must stay a definition");
        }
    }

    #[test]
    fn test_equation_rule_haskell_ocaml() {
        // V83 B9: `foo x = ...` is a function equation.
        let r = classify_structural("twice x = x * 2", 0, false, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("twice"));
        // OCaml: `let f x = ...` / `let rec f x = ...` name f.
        let r = classify_structural("let add x y = x + y", 0, false, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("add"));
        let r = classify_structural("let rec count n = if n = 0 then 0 else count (n - 1)", 0, false, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("count"));
        // Documented collision: a Haskell function named like a C keyword
        // (`double x = ...`) is read as the C declaration of `x` — the two
        // readings are positionally indistinguishable, and the C reading
        // is the safe one (the name-last/binding shape).
        let r = classify_structural("double x = 5", 0, false, false);
        assert!(!r.is_def);
    }

    #[test]
    fn test_equation_rule_binder_led_relaxed_args() {
        // V83 B9 Path B: OCaml module bodies indent their lets and the
        // argument list may carry parens/tildes/braces.
        let r = classify_structural("    let create () = Queue.create ()", 0, false, false);
        assert!(r.is_def, "binder-led `let create () =` must type");
        assert_eq!(r.defined_name, Some("create"));
        let r = classify_structural("  let recv (i, _) = In.read i", 0, false, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("recv"));
        let r = classify_structural("  let map t ~f =", 0, false, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("map"));
        let r = classify_structural("and type_field fields = field type fields", 0, false, false);
        assert!(r.is_def, "`and` is an ML simultaneous binder");
        assert_eq!(r.defined_name, Some("type_field"));
        // Expression-lets stay out (`let f x = x in body`; ctags agrees).
        let r = classify_structural("  let inner y = y in", 0, false, false);
        assert!(!r.is_def, "expression-let must not type");
        // Zero-arg binder bindings are not defs (ctags agrees: `let value = 42`).
        let r = classify_structural("let value = 42", 0, false, false);
        assert!(!r.is_def, "zero-arg `let value = 42` must not type");
        // Rust local inside a body: binder-led but zero args -> local.
        let r = classify_structural("let fmt = format!(\"{}\", x);", 1, false, false);
        assert!(!r.is_def, "Rust local binding must stay local_var");
    }

    #[test]
    fn test_equation_rule_negative_controls() {
        // Binding-shaped lines stay on the binding path (0 args after the
        // name): `let bg = callee()`, `int x = 5;`, `x = 5`.
        let r = classify_structural("let bg = build_brace_graph(&lines);", 1, false, false);
        assert!(!r.is_def, "`let bg = callee()` must stay a binding");
        let r = classify_structural("int x = 5;", 0, false, false);
        assert!(!r.is_def, "`int x = 5;` must stay a binding");
        // V84 P-D: `total = compute();` is a module-scope binding definition
        // (ctags kind:variable) when the RHS is an expression. The trailing
        // `;` marks a statement in a brace language, so that shape keeps the
        // binding path and is covered by the `int x = 5;` case above.
        let r = classify_structural("total = compute()", 0, false, false);
        assert_eq!(r.defined_name, Some("total"));
        let r = classify_structural("total = compute();", 0, false, false);
        assert!(!r.is_def, "semicolon-terminated statement must stay a binding");
        // Type-led declaration in the C family: PascalCase binder.
        let r = classify_structural("Type2 z = a.Type", 0, false, false);
        assert!(!r.is_def, "`Type2 z = ...` declares z, not Type2");
        // Depth gate: an equation inside a body (depth > 0) is a local.
        let r = classify_structural("helper x = x + 1", 2, false, false);
        assert!(!r.is_def, "indented/depth>0 equation must stay local");
        // V92: an INDENTED binder-led equation at depth 0 is an OCaml
        // module-level definition, not a local. This assertion used to read
        // "indented `let` must stay local" and passed only because a
        // single-character name was rejected by the identifier length floor
        // — a test passing for the wrong reason. The documented rule (V84
        // B9) is that 1,626 of 3,149 ocaml-lsp function sites are indented
        // `let`s inside module bodies, so they must type. The real guard is
        // the depth gate, covered by the `helper x` case above and by the
        // indented NON-binder case below.
        let r = classify_structural("  let f x = x + 1", 0, false, false);
        assert_eq!(r.defined_name, Some("f"),
                   "an indented binder-led equation is a module-level definition");
        // Indented, no binder, no args: still a local, whatever the depth.
        let r = classify_structural("    helper = compute()", 0, false, false);
        assert!(!r.is_def, "bare indented binding must stay local");
        // A zero-argument binder is a value binding, not a function.
        let r = classify_structural("  let f = 1", 0, false, false);
        assert!(!r.is_def, "zero-argument `let` binding is not a definition");
        // Single-character definitions are real (Python `def u(s):`, Go `i`).
        let r = classify_structural("def u(s):", 0, false, false);
        assert_eq!(r.defined_name, Some("u"),
                   "one-character Python function must classify");
        // Annotations bail: `x: T = ...` keeps its binding treatment.
        let r = classify_structural("result: usize = compute();", 0, false, false);
        assert!(!r.is_def, "annotated binding must stay a binding");
    }

    #[test]
    fn bodyless_and_aliased_declarations() {
        // V94: four shapes ctags counts as definitions that we dropped.
        for (line, want) in [
            // lowercase type alias (Rust aliases are snake_case)
            ("pub type uid_t = u32;", "uid_t"),
            ("type Message = String;", "Message"),
            // trait method without a `self` receiver
            ("unsafe fn from_raw_handle(handle: RawHandle) -> Self;", "from_raw_handle"),
            ("fn required(&self) -> u8;", "required"),
            // generic list containing an arrow
            ("struct CallOnDrop<O, F: FnOnce() -> O> {", "CallOnDrop"),
            // where-clause on the impl header
            ("impl<St: ?Sized> StreamExt for St where St: Stream {}", "St"),
            // `unsafe impl` is still an impl
            ("unsafe impl<T> Send for JoinHandle<T> {}", "JoinHandle"),
        ] {
            let r = classify_structural(line, 0, false, false);
            assert!(r.is_def, "{line:?} must be a definition");
            assert_eq!(r.defined_name, Some(want), "{line:?}");
        }
        // A local binding must not become a type alias.
        let r = classify_structural("total = compute();", 0, false, false);
        assert_eq!(r.defined_name, Some("total"));
        assert!(!r.is_def);
    }

    #[test]
    fn cross_line_literal_state_keeps_docstrings_out_of_the_index() {
        use crate::lex::LexState;
        // V93: the per-line masker reset its lexer every call, so every line
        // of a Python docstring was tokenized as code and prose became call
        // sites (`request` had 241 docstring hits in psf/requests).
        let mut st = LexState::new();
        let lines = [
            "class Thing:",
            "    \"\"\"",
            "    >>> request = get()",
            "    \"\"\"",
            "    def go(self):",
            "        return request",
        ];
        let masked: Vec<String> = lines
            .iter()
            .map(|l| tokenizable_code_with(l, &mut st))
            .collect();
        assert!(masked[0].contains("Thing"));
        assert!(!masked[2].contains("request"),
                "docstring interior must not be tokenized: {:?}", masked[2]);
        assert!(masked[5].contains("request"),
                "real code after the docstring must be tokenized");

        // A one-line masker cannot see this, which is why the state exists.
        assert!(tokenizable_code("    >>> request = get()").contains("request"));
    }

    #[test]
    fn impl_self_type_is_the_last_path_segment() {
        // V93: `impl From<Elapsed> for std::io::Error {` recorded `std`, and
        // `impl Read for &'_ MockFile {` recorded `_`. 73 impls were missing
        // from tokio's index because of this.
        for (line, want) in [
            ("impl From<Elapsed> for std::io::Error {", "Error"),
            ("impl Listener for tokio::net::TcpListener {", "TcpListener"),
            ("impl Read for &'_ MockFile {", "MockFile"),
            ("impl<T> AsyncBufRead for io::Cursor<T> {", "Cursor"),
            ("impl<O, F: FnOnce() -> O> CallOnDrop<O, F> {", "CallOnDrop"),
            ("impl sealed::FromStreamPriv<()> for () {", "FromStreamPriv"),
        ] {
            let r = classify_structural(line, 0, false, false);
            assert_eq!(r.defined_name, Some(want), "{line:?} -> {r:?}");
        }
        // A bare `()` self type has no name at all.
        let r = classify_structural("impl Trait for () {", 0, false, false);
        assert_eq!(r.defined_name, None);
        // A mut reference resolves to the type, not to `mut`.
        let r = classify_structural("impl Trait for &mut String {", 0, false, false);
        assert_eq!(r.defined_name, Some("String"));
    }

    #[test]
    fn arrow_does_not_break_angle_depth() {
        // `->`'s `>` used to decrement a depth it never incremented, so the
        // `=` in `<Item = u8>` looked top-level and the line was read as an
        // assignment. Both functions below were lost this way.
        for (line, want) in [
            ("async fn impl_trait() -> impl Iterator<Item = u8> {", "impl_trait"),
            ("fn f() -> impl Iterator<Item = u8> {", "f"),
            ("fn g() -> Result<Vec<u8>, Error> {", "g"),
            ("fn h(x: u8) -> u8 {", "h"),
        ] {
            let r = classify_structural(line, 0, false, false);
            assert!(r.is_def, "{line:?} must be a definition");
            assert_eq!(r.defined_name, Some(want), "{line:?}");
        }
        // The same guard must not hide a real assignment.
        let r = classify_structural("total = compute();", 0, false, false);
        assert!(!r.is_def);
    }

    #[test]
    fn signature_name_skips_generics_and_parameter_types() {
        // V92: the name is taken from the chosen signature paren, not from a
        // line-wide "last identifier followed by (" scan. That scan picked
        // `pub` (from `pub(super)`), `const` (from `*const ()` in the
        // parameters) and `impl` (from a return type), producing wrong or
        // missing definitions for ~90 functions in tokio.
        for (line, want) in [
            ("pub(super) fn throttle<T>(duration: Duration) -> Throttle<T>", "throttle"),
            ("pub(crate) fn try_join3<T1, F1, T2, F2, T3, F3, E>(", "try_join3"),
            ("unsafe fn clone(raw: *const ()) -> RawWaker {", "clone"),
            ("fn extend(_: Internal, _collection: &mut (), _item: ()) -> bool {", "extend"),
            ("pub fn abort_matching(&mut self, mut predicate: impl FnMut(&K) -> bool) {", "abort_matching"),
            ("pub(crate) async fn asyncify<F, T>(f: F) -> io::Result<T>", "asyncify"),
            ("async fn plain() -> u8 {", "plain"),
            ("async fn impl_trait() -> impl Iterator<Item = u8> {", "impl_trait"),
        ] {
            let r = classify_structural(line, 0, false, false);
            assert!(r.is_def, "{line:?} must be a definition");
            assert_eq!(r.defined_name, Some(want), "{line:?}");
        }
    }

    #[test]
    fn qualifier_paren_is_not_a_parameter_list() {
        // V92: `pub(crate) struct FastRand {` has a `(` inside the visibility
        // qualifier, so the signature test fired, named `crate`, and rejected
        // the line. 304 pub(crate)/pub(super) declarations in tokio were
        // invisible, which is most of why struct recall was 0.73.
        for (line, want) in [
            ("pub(crate) struct FastRand {", "FastRand"),
            ("pub(crate) trait T: X + Y {", "T"),
            ("pub(crate) enum Fast {", "Fast"),
            ("pub(super) struct S {", "S"),
            ("pub(in crate::io) struct Deep {", "Deep"),
        ] {
            let r = classify_structural(line, 0, false, false);
            assert!(r.is_def, "{line:?} must be a definition");
            assert_eq!(r.defined_name, Some(want), "{line:?}");
        }
        // The qualifier must not swallow a real signature.
        let r = classify_structural("pub(crate) fn close(&self) -> bool {", 0, false, false);
        assert_eq!(r.defined_name, Some("close"));
    }

    #[test]
    fn empty_body_and_bound_lists() {
        // `unsafe impl<T> Send for X {}` ends with `}`; the old brace test
        // only accepted a trailing `{`, so every empty body was invisible.
        for (line, want) in [
            ("impl<T> Unpin for Empty<T> {}", "Empty"),
            ("unsafe impl<T> Send for JoinHandle<T> {}", "JoinHandle"),
            ("impl<T> Stream for Iter<T> {", "Iter"),
            ("pub trait StreamExt: Stream {", "StreamExt"),
        ] {
            let r = classify_structural(line, 0, false, false);
            assert_eq!(r.defined_name, Some(want), "{line:?} -> {r:?}");
        }
    }

    #[test]
    fn brace_less_impl_header_names_the_self_type() {
        // Multi-line impls keep `{` on the next line. 248 in tokio alone.
        for (line, want) in [
            ("impl<I> Stream for Iter<I>", "Iter"),
            ("impl<T> Fuse<T>", "Fuse"),
            ("impl<T, U> Collect<T, U, U::InternalCollection>", "Collect"),
            ("impl<T: Fn() -> u8> Weird<T>", "Weird"),
            ("impl<T> Stream for Iter<T> where", "Iter"),
        ] {
            let r = classify_structural(line, 0, false, false);
            assert_eq!(r.defined_name, Some(want), "{line:?} -> {r:?}");
        }
        // Not headers.
        for line in ["impl", "let x = impl_thing();", "impl<T> Foo<T> {"] {
            let r = classify_structural(line, 0, false, false);
            if line == "impl<T> Foo<T> {" {
                assert_eq!(r.defined_name, Some("Foo"));
            } else {
                assert!(!r.is_def, "{line:?} must not be a definition");
            }
        }
    }

    #[test]
    fn underscore_prefixed_types_are_definitions() {
        // V92: `class _CookieCompatResponse:` was invisible because the
        // PascalCase test looked at byte 0, saw `_`, and rejected it.
        for src in ["class _CookieCompatResponse:",
                    "class __Version__:",
                    "class _Private:"] {
            let r = classify_structural(src, 0, false, false);
            assert!(r.is_def, "{src} must classify as a definition");
        }
        // Lowercase-after-underscore stays water: `_helper = 1` is a binding.
        let r = classify_structural("_helper:", 0, false, false);
        assert!(!r.is_def, "underscore + lowercase is not a type");
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
    fn test_strip_strings_blanks_literal_contents() {
        // A call-shaped token inside a string must not survive tokenization.
        let s = strip_strings_and_comments(r#"    panic!("call_me() failed");"#);
        assert!(!s.contains("call_me"), "string contents must be blanked: {s}");
        assert!(s.contains("panic"), "code outside the string survives: {s}");
    }

    #[test]
    fn test_strip_strings_keeps_code_and_lifetimes() {
        let s = strip_strings_and_comments("    let x = foo(&'a bar);");
        assert!(s.contains("foo"), "code identifiers survive: {s}");
        assert!(s.contains("bar"), "arguments after a lifetime survive: {s}");
        let s2 = strip_strings_and_comments("    let c = 'x'; let y = real();");
        assert!(s2.contains("real"), "code after a char literal survives: {s2}");
    }

    #[test]
    fn test_strip_strings_removes_line_comments() {
        let s = strip_strings_and_comments("    real(); // comment_only_word");
        assert!(s.contains("real"), "code survives: {s}");
        assert!(!s.contains("comment_only_word"), "comment text removed: {s}");
    }

    #[test]
    fn test_deep_nested_fn_is_definition() {
        // V80: a keyword-confirmed declaration is a definition at ANY depth.
        // `fn method` inside `impl` inside two `mod` blocks is depth 3.
        let r = classify_structural("            pub fn method(&self) -> u32 {", 3, true, false);
        assert!(r.is_def, "deep-nested pub fn must be is_def=true: {r:?}");
    }

    #[test]
    fn test_macro_body_fn_is_definition() {
        // `impl_traits!` expands to `impl ... { fn as_handle(...) { ... } }`.
        // The macro body sits inside `macro_rules! { ( ) => { ... } }`, so the
        // declaration is at depth 3 — it must still be indexed as a definition.
        let r = classify_structural("            fn as_handle(&self) -> u32 {", 3, true, false);
        assert!(r.is_def, "fn inside macro body must be is_def=true: {r:?}");
    }

    #[test]
    fn test_deep_control_flow_is_not_definition() {
        // The depth filter must still reject nested control flow.
        let r = classify_structural("            if x {", 3, true, false);
        assert!(!r.is_def, "deep if-block must not be a definition: {r:?}");
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

    // A: two-word return-type shape — no keyword list, whatever the words are.
    #[test]
    fn test_a_bare_int_return_is_def() {
        let r = classify_structural("int plain_api(int x) { return x; }", 0, true, false);
        assert!(r.is_def, "bare `int` return must classify: tag={} name={:?}", r.tag, r.defined_name);
        assert_eq!(r.defined_name, Some("plain_api"));
    }

    #[test]
    fn test_a_macro_prefixed_return_is_def() {
        // `EXPORT` is an unknown macro — the two-word shape admits it anyway.
        let r = classify_structural("EXPORT int macro_api(int x) { return x; }", 0, true, false);
        assert!(r.is_def, "macro-prefixed return must classify: tag={} name={:?}", r.tag, r.defined_name);
        assert_eq!(r.defined_name, Some("macro_api"));
    }

    #[test]
    fn test_a_multiword_return_is_def() {
        let r = classify_structural("unsigned long big_count(void) { return 0; }", 0, true, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("big_count"));
    }

    #[test]
    fn test_a_scoped_def_is_def() {
        let r = classify_structural("int Foo::bar(int x) { return x; }", 0, true, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("bar"));
    }

    #[test]
    fn test_a_return_call_is_not_def() {
        // Single word + no body brace: a call, not a definition.
        let r = classify_structural("    return foo(x);", 2, false, false);
        assert!(!r.is_def, "return-call must not classify: tag={} name={:?}", r.tag, r.defined_name);
    }

    #[test]
    fn test_a_assign_call_is_not_def() {
        let r = classify_structural("    x = compute(y);", 2, false, false);
        assert!(!r.is_def, "assign-call must not classify: tag={} name={:?}", r.tag, r.defined_name);
    }

    #[test]
    fn test_a_dotted_call_is_not_def() {
        let r = classify_structural("    obj.method(a);", 2, false, false);
        assert!(!r.is_def, "dotted call must not classify: tag={} name={:?}", r.tag, r.defined_name);
    }

    // B9: property test over a synthetic declarator grammar. A tiny
    // deterministic LCG (no rand dep) walks the cross product of
    // qualifiers x return types x names x params x bodies. Definitions
    // must classify as defs under the right name; calls must not.
    // This finds gaps no corpus contains (it caught the `::` tail bug
    // in has_return_type_shape during development).
    #[test]
    fn test_b9_declarator_shapes() {
        let quals = ["", "static ", "EXPORT ", "unsigned ", "public ", "pub ", "async "];
        let rets = ["int", "void", "char", "long", "Widget", "string"];
        let names = ["alpha", "beta", "gamma2", "do_work"];
        let params = ["(int x)", "(void)", "(int x, char y)"];
        let bodies = [" { return x; }", ";"];
        // Deterministic LCG, same pattern as the bench tape shuffle.
        let mut state: u64 = 0x1234_5678_9abc_def0;
        let mut next = move |n: u64| -> u64 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) % n
        };
        for _ in 0..300 {
            let q = quals[next(quals.len() as u64) as usize];
            let r = rets[next(rets.len() as u64) as usize];
            let n = names[next(names.len() as u64) as usize];
            let p = params[next(params.len() as u64) as usize];
            let b = bodies[next(bodies.len() as u64) as usize];
            let line = format!("{}{} {}{}{}", q, r, n, p, b);
            let c = classify_structural(&line, 0, b.contains('{'), false);
            if b.contains('{') {
                assert!(c.is_def, "declarator must classify: {:?}", line);
                assert_eq!(c.defined_name, Some(n), "wrong name for {:?}", line);
            } else {
                // `;`-terminated with a body-less qualifier shape: trait-decl
                // path may or may not fire, but it must never bind the
                // trailing call's name. Only assert no-crash here; the
                // negative controls above pin the call cases precisely.
                let _ = c;
            }
        }
        // Negative property: expression contexts never classify, whatever
        // the words are.
        for line in [
            "    return alpha(x);",
            "    x = beta(y);",
            "    obj.gamma2(a);",
            "    if (do_work(x)) {",
        ] {
            let c = classify_structural(line, 2, false, false);
            assert!(!c.is_def, "call must not classify: {:?}", line);
        }
    }

    // B1: `#define` islands. Object-like and function-like macros are
    // definitions; everything else on `#` lines stays water.
    #[test]
    fn test_b1_object_macro_is_def() {
        let r = classify_structural("#define MAX_SIZE 1024", 0, false, false);
        assert!(r.is_def);
        assert_eq!(r.tag, 1);
        assert_eq!(r.defined_name, Some("MAX_SIZE"));
    }

    #[test]
    fn test_b1_function_macro_is_def() {
        let r = classify_structural("#define WRAP(fn) wrapped_##fn", 0, false, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("WRAP"));
    }

    #[test]
    fn test_b1_indented_define_is_def() {
        let r = classify_structural("  #  define FOO 1", 0, false, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("FOO"));
    }

    #[test]
    fn test_b1_include_is_not_def() {
        for line in ["#include <stdio.h>", "#pragma once", "#ifdef FOO", "#endif", "#if 0"] {
            let r = classify_structural(line, 0, false, false);
            assert!(!r.is_def, "must not classify: {:?}", line);
        }
    }

    #[test]
    fn test_b1_rust_attr_untouched() {
        // `#[test]` is a Rust attribute, not a preprocessor line: routing
        // must be byte-identical to the old strip path (no behavior change).
        assert_eq!(tokenizable_code("#[test]"), strip_strings_and_comments("#[test]"));
    }

    // B2: directive usages tokenize; B3: params are blanked.
    #[test]
    fn test_b2_ifdef_tokenizes() {
        let t = tokenizable_code("#ifdef HAVE_PROTO");
        assert!(t.contains("HAVE_PROTO"), "directive name must tokenize: {:?}", t);
        assert!(!t.contains("ifdef"), "directive word must not tokenize: {:?}", t);
    }

    #[test]
    fn test_b2_defined_tokenizes() {
        let t = tokenizable_code("#if defined(FOO) && !defined(BAR)");
        assert!(t.contains("FOO") && t.contains("BAR"), "defined names must tokenize: {:?}", t);
        assert!(!t.contains("defined"), "routing word must not tokenize: {:?}", t);
    }

    #[test]
    fn test_b3_params_blanked() {
        let t = tokenizable_code("#define MAX(a,b) ((a)>(b)?(a):(b))");
        assert!(t.contains("MAX"), "macro name must tokenize: {:?}", t);
        // `a`/`b` are params — but single chars never tokenize anyway
        // (min length 2), so use longer params for the real check.
        let t2 = tokenizable_code("#define CLAMP(val, lo, hi) ((val)<(lo)?(lo):((val)>(hi)?(hi):(val)))");
        for tok in crate::scan_identifiers(&t2) {
            assert!(!["val", "lo", "hi"].contains(&tok.as_str()), "param leaked: {:?} in {:?}", tok, t2);
        }
        assert!(t2.contains("CLAMP"), "macro name must survive: {:?}", t2);
    }

    #[test]
    fn test_b3_variadic_va_args_blanked() {
        let t = tokenizable_code("#define LOG(fmt, ...) fprintf(stderr, fmt, __VA_ARGS__)");
        assert!(t.contains("LOG"), "macro name must survive: {:?}", t);
        assert!(!t.contains("__VA_ARGS__"), "VA_ARGS must be blanked: {:?}", t);
        assert!(!t.contains("fmt"), "param must be blanked: {:?}", t);
    }

    #[test]
    fn test_b3_adjacent_paren_rule() {
        // Per cpp spec, `#define F (x)` is object-like, not function-like:
        // `(x)` is body, so `x` is a usage, not a param.
        let t = tokenizable_code("#define FFF (xyz)");
        assert!(t.contains("FFF"), "name must survive: {:?}", t);
        assert!(t.contains("xyz"), "body ident is a usage, not a param: {:?}", t);
    }

    // B4: Python return annotations.
    #[test]
    fn test_b4_annotated_def() {
        for (line, name) in [
            ("def foo(x)->int:", "foo"),
            ("def foo(x) -> int:", "foo"),
            ("async def foo(x)->int:", "foo"),
            ("def foo(x)->list[int]:", "foo"),
            ("def foo(x)->X | None:", "foo"),
        ] {
            let r = classify_structural(line, 1, false, false);
            assert!(r.is_def, "annotated def must classify: {:?}", line);
            assert_eq!(r.defined_name, Some(name), "wrong name for {:?}", line);
        }
    }

    #[test]
    fn test_b4_annotated_call_is_not_def() {
        // `->` after a call with junk before `:` is not a definition.
        let r = classify_structural("    x = foo(a)->bar:", 2, false, false);
        assert!(!r.is_def, "annotated call must not classify: {:?}", r.defined_name);
    }

    // B5: shorthand methods and getters (brace context carries them).
    #[test]
    fn test_b5_shorthand_method_is_def() {
        for (line, name) in [
            ("    foo() {", "foo"),
            ("    get foo() {", "foo"),
            ("    set foo(v) {", "foo"),
            ("    constructor() {", "constructor"),
        ] {
            let r = classify_structural(line, 1, true, false);
            assert!(r.is_def, "shorthand must classify: {:?}", line);
            assert_eq!(r.defined_name, Some(name), "wrong name for {:?}", line);
        }
    }

    #[test]
    fn test_b5_bare_call_without_brace_is_not_def() {
        // No body brace → still a call, even at depth.
        for line in ["    foo(x);", "    foo(x)", "    get(x);"] {
            let r = classify_structural(line, 1, false, false);
            assert!(!r.is_def, "braceless call must not classify: {:?}", line);
        }
    }

    #[test]
    fn test_b5_top_level_bare_is_not_def() {
        // Depth 0 stays rejected (top-level bare calls).
        let r = classify_structural("foo() {", 0, true, false);
        assert!(!r.is_def, "depth-0 bare must not classify");
    }

    // B6: single-line annotations are already carried by the A shape rule
    // (`@Test String` = two words). Pin the behavior.
    #[test]
    fn test_b6_annotated_return_is_def() {
        for (line, name) in [
            ("@Test String foo() {", "foo"),
            ("@Test int foo() {", "foo"),
        ] {
            let r = classify_structural(line, 0, true, false);
            assert!(r.is_def, "annotated return must classify: {:?}", line);
            assert_eq!(r.defined_name, Some(name), "wrong name for {:?}", line);
        }
    }

    // B7: `where` clauses truncate before name resolution.
    #[test]
    fn test_b7_where_clause_resolves_type() {
        let r = classify_structural("struct Foo<T> where T : Clone {", 0, true, false);
        assert!(r.is_def);
        assert_eq!(r.defined_name, Some("Foo"), "must resolve type, not bound: {:?}", r.defined_name);
    }

    // B8: assignment arrows are definitions; match arms and bare arrows stay water.
    #[test]
    fn test_b8_arrow_assign_is_def() {
        for (line, name) in [
            ("const foo = (x) => x + 1;", "foo"),
            ("export const bar = (a, b) => {", "bar"),
        ] {
            let r = classify_structural(line, 0, false, false);
            assert!(r.is_def, "arrow assign must classify: {:?}", line);
            assert_eq!(r.defined_name, Some(name), "wrong name for {:?}", line);
        }
    }

    #[test]
    fn test_b8_match_arm_stays_water() {
        for line in [
            "            Scheduler::CurrentThread(exec) => exec.block_on(f),",
            "    x => x + 1,",
            "    foo(a => a + 1);",
        ] {
            let r = classify_structural(line, 2, false, false);
            assert!(!r.is_def, "arm must stay water: {:?}", line);
        }
    }
}

    #[test]
    fn debug_block_on_runtime() {
        let line = "    pub fn block_on<F: Future>(&self, future: F) -> F::Output {";
        let r = classify_structural(line, 1, true, false);
        eprintln!("tag={} is_def={} name={:?}", r.tag, r.is_def, r.defined_name);
    }

