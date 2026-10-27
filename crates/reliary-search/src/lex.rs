//! Grammar-free lexical mask (V81): one cross-language byte state machine
//! that decides which bytes of a line are CODE (not string/comment
//! contents). All three byte-level consumers (brace counting, the
//! tokenizer mask, and the brace graph) share this one machine instead of
//! carrying three divergent copies.
//!
//! No per-language lexer, no keyword list — positional byte rules only.
//!
//! Why cross-line state exists: the awkward cases are multi-line literals
//! whose interior braces must not count as block structure —
//!
//!   * Python/Ruby `"""…"""` and `'''…'''` (also C#/Java text blocks)
//!   * Go raw strings and JS template literals `` `…` ``
//!   * C# verbatim strings `@"…"` (spanning lines, `""` escapes a quote)
//!   * Rust raw strings `r#"…"#` / `r##"…"##`
//!   * Python/JS single-quoted strings `'…'` (vs Rust/C char `'x'` and
//!     Rust lifetime `'a` — disambiguated positionally, see `is_lifetime`)
//!
//! The single-quote rule is the subtle one. Rust `'a` (lifetime) and
//! Python `'text'` (string) begin identically. We use the byte AFTER the
//! leading identifier run: a lifetime is `'` + identifier + a byte that
//! is NOT `'`; a quoted region closes at the next unescaped `'`. `'a'`
//! reads as a one-char region in both languages, which is harmless for
//! brace counting.

/// Multi-line literal state carried across the lines of one file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ml {
    #[default]
    None,
    /// `"""` — Python/Ruby/C#/Java text block.
    TripleDq,
    /// `'''` — Python.
    TripleSq,
    /// `` ` `` — Go raw string / JS template literal.
    Backtick,
    /// `@"` — C# verbatim string (`""` escapes a quote).
    Verbatim,
    /// `r#"…"#` with N hashes before the opening quote.
    Raw(u8),
}

/// Longest a multi-line BACKTICK literal may span before it is treated as a
/// misparse.
///
/// A backtick in prose is the problem case: doc comments quote code with
/// `` ` `` constantly, and one unpaired backtick opens a Go/JS template
/// literal that never closes, so every later definition in the file is masked
/// and disappears from the index. `httpx/_urlparse.py` lost its `ParseResult`
/// class and five methods this way, from a lone backtick inside a `#` comment.
///
/// The bound applies ONLY to backticks. Triple-quoted strings are Python
/// docstrings and C#/Java text blocks, which are routinely longer than this
/// (a 59-line `httpx` docstring is normal), so bounding those would delete
/// real definitions — the opposite failure. Real template literals are short;
/// 50 lines is generous, and the heredoc already uses the same
/// abandon-on-timeout fail-safe.
const ML_MAX_LINES: u16 = 50;

/// Cross-line lexical state. Reset once per file — the state is carried
/// across the lines of one file and must never leak into the next file on
/// the same thread under Rayon (callers own that reset).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LexState {
    pub block_comment: bool,
    pub ml: Ml,
    /// Lines spent inside the current multi-line literal. Used to abandon a
    /// literal that runs implausibly long (see `ML_MAX_LINES`).
    pub ml_lines: u16,
    /// Active heredoc terminator (V84 P-B). `None` when not inside one.
    /// Ruby/Shell/PHP heredoc bodies are literal text — braces, quotes and
    /// comment markers inside them are not code. The tag is stored inline
    /// (max 15 bytes); longer tags are ignored (fail-safe: current behavior).
    pub heredoc: Option<HeredocTag>,
}

/// Inline heredoc terminator: fixed array avoids an allocation in the
/// per-line hot path and keeps `LexState: Copy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeredocTag {
    bytes: [u8; 15],
    len: u8,
    /// Remaining lines before the heredoc gives up and normal lexing
    /// resumes. A heredoc that never terminates must not swallow the rest
    /// of the file (a `flags << MASK` shift misread as an opener would
    /// otherwise blank every subsequent brace).
    countdown: u16,
}

/// Max lines a heredoc may run. Ruby/Shell heredocs are usually short;
/// 200 lines bounds the damage of a false opener while covering real ones.
const HEREDOC_MAX_LINES: u16 = 200;

impl HeredocTag {
    /// Build a tag; `None` when empty or longer than 15 bytes.
    pub fn new(word: &[u8]) -> Option<Self> {
        if word.is_empty() || word.len() > 15 {
            return None;
        }
        let mut bytes = [0u8; 15];
        bytes[..word.len()].copy_from_slice(word);
        Some(HeredocTag { bytes, len: word.len() as u8, countdown: HEREDOC_MAX_LINES })
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// Does this line terminate the heredoc? Compared after trimming, which
    /// covers both `<<EOF` (column 0) and `<<-EOF`/`<<~EOF` (indented).
    #[inline]
    pub fn terminates(&self, line: &str) -> bool {
        line.trim().as_bytes() == self.as_bytes()
    }
}

impl LexState {
    /// Const constructor (usable in `thread_local! { ... const { } }`).
    pub const fn new() -> Self {
        LexState { block_comment: false, ml: Ml::None, ml_lines: 0, heredoc: None }
    }
    pub fn reset(&mut self) {
        *self = LexState::new();
    }
}

#[inline]
fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Previous non-space/tab byte before `i` (None at line start).
fn prev_nonspace(b: &[u8], i: usize) -> Option<u8> {
    let mut j = i;
    while j > 0 {
        j -= 1;
        if b[j] != b' ' && b[j] != b'\t' {
            return Some(b[j]);
        }
    }
    None
}

/// Does the `'` at `i` begin a Rust lifetime (`'a`), not a quoted region?
fn is_lifetime(b: &[u8], i: usize) -> bool {
    let j = i + 1;
    let mut k = j;
    while k < b.len() && is_ident(b[k]) {
        k += 1;
    }
    k > j && k < b.len() && b[k] != b'\''
}

fn close_normal(b: &[u8], i: usize) -> Option<usize> {
    let mut k = i + 1;
    while k < b.len() {
        if b[k] == b'\\' {
            k += 2;
            continue;
        }
        if b[k] == b'"' {
            return Some(k + 1);
        }
        k += 1;
    }
    None
}

fn close_raw(b: &[u8], i: usize, hashes: usize) -> Option<usize> {
    let mut k = i + 1;
    while k < b.len() {
        if b[k] == b'"' {
            let mut m = k + 1;
            let mut seen = 0usize;
            while m < b.len() && b[m] == b'#' && seen < hashes {
                seen += 1;
                m += 1;
            }
            if seen == hashes {
                return Some(m);
            }
        }
        k += 1;
    }
    None
}

fn close_verbatim(b: &[u8], i: usize) -> Option<usize> {
    let mut k = i + 1;
    while k < b.len() {
        if b[k] == b'"' {
            if k + 1 < b.len() && b[k + 1] == b'"' {
                k += 2;
                continue;
            }
            return Some(k + 1);
        }
        k += 1;
    }
    None
}

/// Parse a heredoc opener at `i` (pointing at the first `<` of `<<`).
///
/// Recognised: `<<WORD`, `<<-WORD`, `<<~WORD`, `<<'WORD'`, `<<"WORD"`,
/// and PHP's `<<<WORD`. Returns `None` for shift/append operators:
/// `x << y`, `buf <<= 2`, `1 << n`. The positional rules:
///   - `<<=` is an assignment shift.
///   - The tag must be an UPPERCASE word (`EOF`, `PAGE`, `HTML`) or a
///     quoted word — the conventions every real heredoc uses. A lowercase
///     bare word after `<<` is a shift operand (`flags << mask`).
///   - The tag must be the last thing on the line (optionally followed by
///     `;`, `)`, or a method chain like `<<-EOF.dup` handled by trimming).
fn parse_heredoc_opener(b: &[u8], i: usize) -> Option<HeredocTag> {
    let mut k = i + 2;
    // PHP `<<<` — consume one more `<`.
    if k < b.len() && b[k] == b'<' {
        k += 1;
    }
    // `<<=` / `<=`: assignment, not a heredoc.
    if k < b.len() && b[k] == b'=' {
        return None;
    }
    // `<<~` (Ruby squiggly) and `<<-` (indented) modifiers.
    if k < b.len() && (b[k] == b'~' || b[k] == b'-') {
        k += 1;
    }
    // A space between `<<` and the tag is legal (`<< EOF`) but ALSO the
    // shape of a shift with a spaced operand. Only accept the spaced form
    // when the tag is uppercase or quoted.
    let mut spaced = false;
    while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
        spaced = true;
        k += 1;
    }
    if k >= b.len() {
        return None;
    }
    let quote = matches!(b[k], b'\'' | b'"');
    let start = if quote { k + 1 } else { k };
    let mut end = start;
    while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'_') {
        end += 1;
    }
    if end == start {
        return None;
    }
    // A SPACED `<<` whose left operand is an identifier, `)` or `]` is a
    // binary operator, never a heredoc — and that holds for a QUOTED tag too.
    // The bare-tag branch already had this test; the quoted branch did not, so
    // every C++ stream-output line opened a bogus heredoc:
    //
    //   *os << "NULL";     -> tag NULL -> 200 lines masked
    //   *os << ' ';        -> tag ' '  -> (single-quote path, same class)
    //
    // Measured on fmtlib/fmt: gtest.h lost 600 of 12,399 lines, starting at a
    // `} else {` four lines after `*os << "NULL";`, and 89 named functions were
    // unindexed. The rule is positional and mirrors the bare-tag one.
    if spaced {
        let mut j = i;
        while j > 0 && (b[j - 1] == b' ' || b[j - 1] == b'\t') {
            j -= 1;
        }
        // `>` too: a quoted tag after a closing string literal is a
        // concatenation (`s << "a" << "b"`), where the second `<<` follows
        // the `"` of the first operand.
        if j > 0
            && (b[j - 1].is_ascii_alphanumeric()
                || b[j - 1] == b'_'
                || b[j - 1] == b')'
                || b[j - 1] == b']'
                || b[j - 1] == b'"'
                || b[j - 1] == b'\'')
        {
            return None;
        }
    }
    if quote {
        // Closing quote must exist.
        if end >= b.len() || b[end] != b[k] {
            return None;
        }
    } else {
        // Bare tag: uppercase-only, unless it is directly attached (`<<EOF`
        // without a space). A lowercase attached tag would be a shift with
        // no spaces (`x<<y`), so keep the uppercase requirement everywhere.
        if !b[start..end].iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_') {
            return None;
        }
        // Require at least one letter so `<<2` is a shift.
        if !b[start..end].iter().any(|c| c.is_ascii_uppercase()) {
            return None;
        }
    }
    // The tag must not be followed by an operator that makes it an
    // expression (`<<FOO + 1`); allow `;`, `)`, `.`, EOF, or whitespace.
    let mut after = end;
    if quote {
        after += 1;
    }
    while after < b.len() && (b[after] == b' ' || b[after] == b'\t') {
        after += 1;
    }
    if after < b.len() {
        let c = b[after];
        if !matches!(c, b';' | b')' | b',' | b'.' | b'|' | b'&') {
            return None;
        }
    }
    HeredocTag::new(&b[start..end])
}

fn close_single(b: &[u8], i: usize) -> Option<usize> {
    let mut k = i + 1;
    while k < b.len() {
        if b[k] == b'\\' {
            k += 2;
            continue;
        }
        if b[k] == b'\'' {
            return Some(k + 1);
        }
        k += 1;
    }
    None
}

fn close_backtick(b: &[u8], from: usize) -> Option<usize> {
    let mut k = from;
    while k < b.len() {
        if b[k] == b'\\' {
            k += 2;
            continue;
        }
        if b[k] == b'`' {
            return Some(k + 1);
        }
        k += 1;
    }
    None
}

/// Core walk: invoke `on_code` with the byte index of every CODE byte of
/// `line`, updating the cross-line state `st` in place. One
/// implementation, wrapped by the two consumers below.
///
/// `hash_comment` selects whether `#` begins a line comment. Python/Ruby
/// want `true`; C/C++ brace counting wants `false` (a `#define FOO { }`
/// body's brace belongs to the depth count; Rust `#[`/`#!` are exempt).
/// Is the `/` at `i` in value position, i.e. can it start a regex literal?
/// Positional only: looks at the previous significant byte.
fn regex_can_start(b: &[u8], i: usize) -> bool {
    match prev_nonspace(b, i) {
        None => true,
        Some(p) => matches!(
            p,
            b'(' | b'=' | b',' | b':' | b'[' | b'!' | b'&' | b'|' | b'?' | b'{' | b';' | b'~' | b'*' | b'%' | b'^' | b'<' | b'>'
        ),
    }
}

/// Scan a regex literal starting at `i` (the opening `/`). Returns the byte
/// index just past the closing `/`, or `None` when the line ends first.
/// Handles `\\` escapes and `[...]` classes (which may contain `/`).
fn scan_regex(b: &[u8], i: usize) -> Option<usize> {
    let mut k = i + 1;
    let mut in_class = false;
    while k < b.len() {
        match b[k] {
            b'\\' => {
                k += 2;
                continue;
            }
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            b'/' if !in_class => return Some(k + 1),
            b'\n' => return None,
            _ => {}
        }
        k += 1;
    }
    None
}

fn walk_code(line: &str, st: &mut LexState, hash_comment: bool, on_code: &mut dyn FnMut(usize)) {
    let b = line.as_bytes();
    let entered_in_ml = st.ml != Ml::None;

    // V84 P-B: heredoc body. Everything until the terminator line is literal
    // text — braces and quotes inside it are not code. A heredoc that runs
    // past HEREDOC_MAX_LINES abandons the state and normal lexing resumes,
    // so a false opener cannot blank the rest of a file.
    if let Some(tag) = st.heredoc {
        if tag.terminates(line) {
            st.heredoc = None;
            return;
        }
        let mut t = tag;
        t.countdown = t.countdown.saturating_sub(1);
        if t.countdown == 0 {
            st.heredoc = None;
            // Fall through to normal lexing for this line.
        } else {
            st.heredoc = Some(t);
            return;
        }
    }

    let mut i = 0usize;
    while i < b.len() {
        if st.block_comment {
            if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                st.block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        match st.ml {
            Ml::None => {}
            Ml::TripleDq => {
                if i + 2 < b.len() && b[i] == b'"' && b[i + 1] == b'"' && b[i + 2] == b'"' {
                    st.ml = Ml::None;
                    i += 3;
                } else {
                    i += 1;
                }
                continue;
            }
            Ml::TripleSq => {
                if i + 2 < b.len() && b[i] == b'\'' && b[i + 1] == b'\'' && b[i + 2] == b'\'' {
                    st.ml = Ml::None;
                    i += 3;
                } else {
                    i += 1;
                }
                continue;
            }
            Ml::Backtick => {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == b'`' {
                    st.ml = Ml::None;
                    i += 1;
                } else {
                    i += 1;
                }
                continue;
            }
            Ml::Verbatim => {
                if b[i] == b'"' {
                    if i + 1 < b.len() && b[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    st.ml = Ml::None;
                    i += 1;
                } else {
                    i += 1;
                }
                continue;
            }
            Ml::Raw(h) => {
                if b[i] == b'"' {
                    let mut k = i + 1;
                    let mut seen = 0usize;
                    while k < b.len() && b[k] == b'#' && seen < h as usize {
                        seen += 1;
                        k += 1;
                    }
                    if seen == h as usize {
                        st.ml = Ml::None;
                        i = k;
                        continue;
                    }
                }
                i += 1;
                continue;
            }
        }

        let c = b[i];
        // V84 P-B: heredoc opener. `<<WORD`, `<<-WORD`, `<<~WORD`,
        // `<<'WORD'`, `<<"WORD"`, and PHP `<<<WORD`. The discriminator
        // against a shift/append operator (`flags << MASK`, `buf << x`) is
        // POSITIONAL and verified on real corpora: an opener's tag is an
        // uppercase word (Ruby/Shell convention) or a quoted word, while a
        // shift operand is an arbitrary expression. `<<=` (assignment) and
        // `<<` followed by a space then a lowercase bare word are shifts.
        if c == b'<' && i + 1 < b.len() && b[i + 1] == b'<' {
            if let Some(tag) = parse_heredoc_opener(b, i) {
                st.heredoc = Some(tag);
                // The opener itself is code; bytes before it were emitted.
                // Emit the opener delimiters so header shapes stay stable.
                on_code(i);
                on_code(i + 1);
                let rest = i + 2;
                let mut k = rest;
                while k < b.len() {
                    on_code(k);
                    k += 1;
                }
                return;
            }
        }
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            break;
        }
        // Block comment must be tested BEFORE the regex literal below.
        //
        // `regex_can_start` returns true at line start and after `( = , : [ ! &`
        // `| ? { ;` — all of which precede a doc comment. So `/** ... */` was
        // reaching `scan_regex`, which ran from the leading `/` and terminated
        // at the first unescaped `/` *inside the comment text* (a URL, a file
        // path, `@zod/core`). The leftover tail was then lexed as code, and if
        // it held an odd number of backticks it opened a template literal that
        // masked every following line (bounded only by ML_MAX_LINES).
        //
        //   `/** See \`classic/in-out.ts\`. */`
        //    ^ scan_regex start      ^ it stopped here
        //    leftover: `in-out.ts\`. */`  -> one backtick -> Ml::Backtick
        //
        // Measured on zod: 9 files lost 20 declaration-shaped lines, including
        // `export interface ZodError`. See docs/forensics/2026-10-25-*.
        //
        // A `/*` at value position is always a block-comment opener: no regex
        // literal can begin with a bare `*` (the regex for an asterisk is
        // `/\*/`, which starts `/` then `\`). This is a positional test.
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            st.block_comment = true;
            i += 2;
            continue;
        }
        if hash_comment
            && c == b'#'
            && !(i + 1 < b.len() && (b[i + 1] == b'[' || b[i + 1] == b'"'))
        {
            break;
        }
        // V84 P-B: regex literal. A `/` in VALUE position (`x = /}/`,
        // `(/\{)/`, `return /}/;`) opens a regex; in OPERATOR position
        // (`a / b`) it is division. The previous significant byte decides:
        // after `(`, `=`, `,`, `:`, `[`, `!`, `&`, `|`, `?`, `{`, `;`, or
        // line start, a `/` cannot be division in valid code, so it must
        // open a regex. The interior is blanked to the closing unescaped
        // `/` (excluding `/` inside `[...]` classes), so a `{` inside a
        // regex never counts as a brace.
        if c == b'/' && regex_can_start(b, i) {
            if let Some(end) = scan_regex(b, i) {
                on_code(i);
                on_code(end - 1);
                i = end;
                continue;
            }
            // No closing `/` on this line: treat as division (fail-safe,
            // current behavior).
        }
        // Strings: emit the DELIMITER bytes as code (shape-preserving —
        // header patterns and token columns stay stable) and blank the
        // interior. Content bytes are what must never look like code.
        if c == b'"' {
            if i + 2 < b.len() && b[i + 1] == b'"' && b[i + 2] == b'"' {
                on_code(i);
                on_code(i + 1);
                on_code(i + 2);
                st.ml = Ml::TripleDq;
                i += 3;
                continue;
            }
            if i >= 1 && b[i - 1] == b'r' {
                let mut h = 0usize;
                let mut j = i + 1;
                while j < b.len() && b[j] == b'#' {
                    h += 1;
                    j += 1;
                }
                on_code(i);
                if let Some(e) = close_raw(b, i, h) {
                    if e >= 1 {
                        on_code(e - 1);
                    }
                    i = e;
                } else {
                    st.ml = Ml::Raw(h as u8);
                    i = j;
                }
                continue;
            }
            if i >= 1 && b[i - 1] == b'@' {
                on_code(i);
                if let Some(e) = close_verbatim(b, i) {
                    if e >= 1 {
                        on_code(e - 1);
                    }
                    i = e;
                } else {
                    st.ml = Ml::Verbatim;
                    i += 1;
                }
                continue;
            }
            on_code(i);
            if let Some(e) = close_normal(b, i) {
                if e >= 1 {
                    on_code(e - 1);
                }
                i = e;
            } else {
                break;
            }
            continue;
        }
        if c == b'\'' {
            // Nix indented string: `''` followed by something other than a third
            // quote. Nix is a language whose only multi-line literal is this
            // one, and every one of its 3,763 `.nix` files in home-manager was
            // being tokenized as code inside them. Opening on `''` (not `'''`)
            // keeps Python and Ruby triple quotes on their existing path.
            // V99: `''` is NOT opened as a cross-line literal.
            //
            // Nix's indented string is `'' ... ''` and Ruby's empty string is
            // the same pair. At end of line the two are textually identical:
            // Nix `x = ''` opens a multi-line literal, Ruby `x = ''` is a
            // complete empty string. A grammar-free lexer cannot tell them
            // apart, and the failure modes are wildly asymmetric:
            //
            //   open-in-Ruby  -> the rest of the FILE is masked (measured:
            //                    1,596 of sinatra/base.rb's 2,174 lines, so
            //                    `def accept` and everything after vanished)
            //   open-in-Nix   -> the string interior is tokenized as code
            //                    (measured: 1,157 false definitions from shell
            //                    snippets in module docs)
            //
            // Ruby is a measured corpus and Nix is not, so the lexer takes the
            // Ruby reading: a `''` pair is masked only within its own line.
            // Nix indented strings are therefore NOT modelled, and that is
            // documented as a Nix limitation rather than papered over.
            if i + 2 < b.len() && b[i + 1] == b'\'' && b[i + 2] == b'\'' {
                on_code(i);
                on_code(i + 1);
                on_code(i + 2);
                st.ml = Ml::TripleSq;
                i += 3;
                continue;
            }
            // Single quote: Rust lifetime (`'a`), Rust/C char (`'x'`), and
            // Python/Ruby string (`'a { b } c'`) share the leading byte.
            // Positional disambiguation, innermost-first:
            //   1. `'` after `&` or `<` and followed by an identifier run
            //      is a lifetime (`&'a str`, `<'a, 'b>`) — never a string,
            //      so it can never legitimately wrap a brace.
            //   2. else, if a closing `'` exists on the line, it is a
            //      quoted region (char literal or string) — emit both
            //      delimiters, blank the interior.
            //   3. else an identifier run is a lifetime (`for<'a>`,
            //      `where 'a`); otherwise emit one byte.
            let prev = prev_nonspace(b, i);
            if matches!(prev, Some(b'&') | Some(b'<')) && is_lifetime(b, i) {
                on_code(i);
                i += 1;
                continue;
            }
            if let Some(e) = close_single(b, i) {
                on_code(i);
                if e >= 1 {
                    on_code(e - 1);
                }
                i = e;
                continue;
            }
            on_code(i);
            i += 1;
            continue;
        }
        if c == b'`' {
            on_code(i);
            if let Some(e) = close_backtick(b, i + 1) {
                if e >= 1 {
                    on_code(e - 1);
                }
                i = e;
            } else {
                st.ml = Ml::Backtick;
                i += 1;
            }
            continue;
        }
        on_code(i);
        i += 1;
    }

    // V92: bound multi-line literals. Counted only for lines that CONTINUE an
    // existing literal, so the opening line is not charged. A literal that
    // outlives the bound is a misparse, not a literal: drop the state and let
    // the rest of the file lex normally.
    if st.ml == Ml::Backtick {
        if entered_in_ml {
            st.ml_lines = st.ml_lines.saturating_add(1);
            if st.ml_lines > ML_MAX_LINES {
                // Misparse, not a literal: drop the state so the rest of the
                // file lexes normally instead of being masked wholesale.
                st.ml = Ml::None;
                st.ml_lines = 0;
            }
        } else {
            st.ml_lines = 0;
        }
    } else {
        st.ml_lines = 0;
    }
}

/// Is the next line's first byte already inside a string or block comment?
///
/// `walk_code` is given every code byte; the bytes it *skips* at the start of
/// a line are precisely the ones that continue a multi-line literal from the
/// previous line. A line that begins in prose (a docstring body, a block
/// comment, a template literal) must not be classified: the classifier is
/// handed raw text, so `A symbol mentioned inside a string` was read as code
/// and `"... write_vocab() ...")` became a definition of `string`.
///
/// Positional, no keyword list: it asks the same lexer that already decides
/// what is code everywhere else.
pub fn starts_in_code(line: &str, st: &mut LexState, hash_comment: bool) -> bool {
    // Read the state BEFORE walking, then advance the caller's state. The
    // first version walked a *copy* to avoid disturbing the caller, which
    // meant the state never advanced and every line "started in code" — the
    // docstring test caught it immediately.
    let started_in_literal = st.block_comment || st.ml != Ml::None || st.heredoc.is_some();
    walk_code(line, st, hash_comment, &mut |_| {});
    !started_in_literal
}

/// Count `{` and `}` outside strings/comments, single pass, no allocation.
/// Updates `st` (block-comment + multi-line literal tracking).
pub fn brace_counts(line: &str, st: &mut LexState, hash_comment: bool) -> (usize, usize) {
    let b = line.as_bytes();
    let mut open = 0usize;
    let mut close = 0usize;
    walk_code(line, st, hash_comment, &mut |i| match b[i] {
        b'{' => open += 1,
        b'}' => close += 1,
        _ => {}
    });
    (open, close)
}

/// Length-preserving mask: code bytes kept, string/comment bytes replaced
/// with spaces (so every byte offset in the original line stays valid).
pub fn masked_bytes(line: &str, st: &mut LexState, hash_comment: bool) -> Vec<u8> {
    let mut out = Vec::new();
    masked_bytes_into(line, st, hash_comment, &mut out);
    out
}

/// Allocation-free variant for hot loops: reuses `out` across lines.
///
/// The buffer is filled with spaces and code bytes are copied in place, so
/// byte offsets stay identical to the input line. A multi-byte code point may
/// be partially masked (only some of its bytes are inside a string/comment),
/// so callers that need text must use `from_utf8_lossy`; byte-level scanners
/// may read the buffer directly, since every identifier byte is ASCII.
pub fn masked_bytes_into(
    line: &str,
    st: &mut LexState,
    hash_comment: bool,
    out: &mut Vec<u8>,
) {
    let b = line.as_bytes();
    out.clear();
    out.resize(b.len(), b' ');
    walk_code(line, st, hash_comment, &mut |i| out[i] = b[i]);
}

/// Masked line as a `String` (only string/comment bytes become spaces, so
/// the result stays valid UTF-8 — non-ASCII source bytes are never masked).
pub fn masked_line(line: &str, st: &mut LexState, hash_comment: bool) -> String {
    String::from_utf8_lossy(&masked_bytes(line, st, hash_comment)).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask1(line: &str, hash: bool) -> String {
        let mut st = LexState::default();
        masked_line(line, &mut st, hash)
    }

    #[test]
    fn doc_comment_with_slash_and_one_backtick_is_not_a_regex() {
        // Regression (docs/forensics/2026-10-25-*): the regex-literal branch was
        // tested before the block-comment branch, and `regex_can_start` is true
        // at line start, so `/** ... */` was scanned as a regex. `scan_regex`
        // terminated at the first `/` inside the comment text, leaving a tail
        // with an odd number of backticks that opened a template literal.
        //
        //   zod src/v4/mini/in-out.ts:10 lost the next five definitions this way
        //
        // Both shapes below contain a `/` in the comment text and an odd number
        // of backticks. Neither may change the lexer state.
        for line in [
            "/** See `classic/in-out.ts`. */",
            "  /** @deprecated Use `z.core.$ZodIssue` from `@zod/core` instead. */",
            "/* A `path/to/file` reference */",
        ] {
            let mut st = LexState::new();
            let _ = masked_line(line, &mut st, false);
            assert_eq!(st.ml, Ml::None, "a doc comment must not open a literal: {line}");
            // The comment is prose: nothing after the opener is code.
            let m = masked_line(line, &mut st, false);
            assert!(
                !m.contains("in-out") && !m.contains("ZodIssue") && !m.contains("path"),
                "comment body leaked into code: {m:?}"
            );
            // The very next declaration classifies normally.
            let m = masked_line("export interface ZodError<T = unknown> {", &mut st, false);
            assert!(m.contains("ZodError"), "declaration after the comment was masked: {m:?}");
        }
    }

    #[test]
    fn a_real_regex_literal_still_opens_after_the_reorder() {
        // The fix must not cost regex literals: a `/*` is a comment, but `/` in
        // value position followed by anything else is still a regex, and a `{`
        // inside it must not count as a brace.
        //
        // Only the positions `regex_can_start` actually recognises are asserted
        // here. A regex after a bare keyword (`return /a{2}/;`) is NOT
        // recognised — that is a pre-existing gap, recorded rather than encoded
        // as expected behaviour.
        let cases = ["let x = /}/;", "let y = (/\\{)/;", "f(/,/);"];
        for line in cases {
            let (open, close) = brace_counts(line, &mut LexState::new(), false);
            assert_eq!(
                (open, close),
                (0, 0),
                "braces inside a regex must be blanked: {line}"
            );
            let m = mask1(line, false);
            assert!(
                !m.contains('{') && !m.contains('}'),
                "regex interior leaked into code: {line} -> {m:?}"
            );
        }
        // Division is still division: `a / b` must not swallow the rest.
        let (o, c) = brace_counts("let r = a / b;", &mut LexState::new(), false);
        assert_eq!((o, c), (0, 0));
        assert!(mask1("let r = a / b;", false).contains("a / b"),
                "division must not be blanked as a regex interior");
    }

    #[test]
    fn unpaired_backtick_in_prose_does_not_swallow_the_file() {
        // Regression: one stray backtick in a comment opened a template
        // literal that never closed, so every later definition in the file was
        // masked. `httpx/_urlparse.py` lost six definitions this way.
        // `hash_comment = false`, which is what the ingest path passes: the
        // lexer cannot tell a Python `#` comment from a C preprocessor line, so
        // a backtick inside a Python comment IS seen as an opener.
        let mut st = LexState::new();
        // One unpaired backtick on the opening line...
        let _ = masked_line("# prose mentioning a ` backtick", &mut st, false);
        assert_eq!(st.ml, Ml::Backtick, "the stray backtick opens a literal");
        // ...and then ordinary lines that contain no backtick, so the literal
        // never closes. Before the bound this ran to the end of the file.
        let mut abandoned = false;
        for _ in 0..(ML_MAX_LINES as usize + 10) {
            let _ = masked_line("more prose, no backtick here", &mut st, false);
            if st.ml == Ml::None {
                abandoned = true;
                break;
            }
        }
        assert!(abandoned,
                "an unterminated literal must be abandoned ({:?} after {} lines)",
                st.ml, st.ml_lines);
        // Definitions after the give-up point classify normally again.
        let m = masked_line("class ParseResult:", &mut st, false);
        assert!(m.contains("ParseResult"));
    }

    #[test]
    fn long_docstring_is_not_abandoned() {
        // The bound is backtick-only. A 60-line Python docstring is ordinary
        // and must keep its enclosing file's definitions visible.
        let mut st = LexState::new();
        let _ = masked_line("    \"\"\"", &mut st, false);
        assert_eq!(st.ml, Ml::TripleDq);
        for _ in 0..80 {
            let _ = masked_line("    doc line", &mut st, false);
        }
        assert_eq!(st.ml, Ml::TripleDq, "docstrings are not bounded");
        let _ = masked_line("    \"\"\"", &mut st, false);
        assert_eq!(st.ml, Ml::None);
        let m = masked_line("    def after(self):", &mut st, false);
        assert!(m.contains("def"));
    }

    #[test]
    fn short_multiline_literal_is_still_honoured() {
        // The bound must not break legitimate strings.
        let mut st = LexState::new();
        let _ = masked_line("s = `first", &mut st, false);
        assert_eq!(st.ml, Ml::Backtick);
        let m = masked_line("still inside", &mut st, false);
        assert!(!m.contains("still"));
        let m = masked_line("last`", &mut st, false);
        assert_eq!(st.ml, Ml::None);
        let _ = m;
    }

    #[test]
    fn brace_in_normal_string_blanked() {
        let m = mask1(r#"let s = "a { b } c";"#, false);
        assert!(!m.contains('{') && !m.contains('}'));
        assert_eq!(m.len(), r#"let s = "a { b } c";"#.len());
    }

    #[test]
    fn brace_in_line_comment_blanked() {
        assert!(!mask1("code(); // has { brace", false).contains('{'));
        assert!(!mask1("code() # has { brace", true).contains('{'));
        assert!(mask1("#define X { 1 }", false).contains('{'));
    }

    #[test]
    fn brace_in_block_comment_spans_lines() {
        let mut st = LexState::default();
        assert!(!masked_line("a /* open { ", &mut st, false).contains('{'));
        assert!(st.block_comment);
        assert!(!masked_line("still { inside", &mut st, false).contains('{'));
        let l3 = masked_line("} close */ real { ", &mut st, false);
        assert!(!st.block_comment);
        assert!(l3.contains('{'));
    }

    #[test]
    fn regex_interior_is_blanked() {
        // A brace inside a regex literal is text, not structure.
        assert!(!mask1(r"let re = /\{/;", false).contains('{'));
        assert!(!mask1(r"if (/}/.test(s)) {", false).contains('}'));
        assert!(!mask1(r"x = /[{}]/;", false).contains('{'));
        // Division is untouched.
        assert!(mask1("let half = total / 2; fn real() {", false).contains('{'));
        assert!(mask1("a / b / c", false).chars().filter(|c| *c == '/').count() == 2);
    }

    #[test]
    fn regex_escape_and_class() {
        // `\/` is an escaped slash, not the end of the regex.
        assert!(!mask1(r"re = /a\/b{/;", false).contains('{'));
        // A slash inside a character class does not end the regex.
        assert!(!mask1(r"re = /[/]{/;", false).contains('{'));
    }

    #[test]
    fn heredoc_body_is_blanked() {
        // Ruby/shell heredoc: braces inside the body are text, not code.
        let mut st = LexState::default();
        assert!(!masked_line("TEMPLATE = <<~HTML", &mut st, true).contains('{'));
        assert!(st.heredoc.is_some());
        assert!(!masked_line("<div>{ not code }</div>", &mut st, true).contains('{'));
        assert!(st.heredoc.is_some());
        let last = masked_line("HTML", &mut st, true);
        assert!(st.heredoc.is_none());
        assert!(!last.contains('{'));
        // After the terminator, normal lexing resumes.
        assert!(masked_line("fn real() {", &mut st, false).contains('{'));
    }

    #[test]
    fn heredoc_terminator_requires_its_own_line() {
        let mut st = LexState::default();
        masked_line("x = <<-EOF", &mut st, true);
        // A line containing the tag among other text does not terminate.
        masked_line("EOF is not the terminator", &mut st, true);
        assert!(st.heredoc.is_some());
    }

    #[test]
    fn shifts_are_not_heredocs() {
        // Uppercase right operand with an identifier to its left: a shift.
        for line in [
            "let flags = mask << MASK;",
            "buf << data;",
            "x <<= 2;",
            "value = 1 << SHIFT;",
            "items << item",
        ] {
            let mut st = LexState::default();
            masked_line(line, &mut st, false);
            assert!(st.heredoc.is_none(), "must not be a heredoc: {line}");
        }
    }

    #[test]
    fn stream_output_is_not_a_heredoc() {
        // Regression: a SPACED `<<` with a QUOTED tag skipped the left-operand
        // test, so every C++ stream-output line opened a bogus heredoc and
        // masked the next 200 lines. fmtlib/fmt gtest.h lost 600 of 12,399
        // lines and 89 named functions.
        for line in [
            "  *os << \"NULL\";",
            "  *os << '}';",
            "  std::cout << \"x\";",
            "  buf << \"suffix\";",
            "  s << \"a\" << \"b\";",
            "  vec.push_back(x) << \"y\";",
            "  a[0] << \"y\";",
        ] {
            let mut st = LexState::default();
            let _ = masked_line(line, &mut st, false);
            assert!(st.heredoc.is_none(), "stream output must not open a heredoc: {line}");
            // And the next line is still code.
            assert!(
                starts_in_code("    } else {", &mut st, false),
                "the line after {line:?} must still be code"
            );
        }
        // Real heredocs still open, spaced or not, bare or quoted.
        //
        // NOT asserted: `cat << EOF`. A bare uppercase tag after a command word
        // is indistinguishable by position from `x << MASK`, and the existing
        // bare-tag rule resolves that ambiguity toward "shift". That false
        // negative predates this change and is recorded rather than hidden.
        for line in [
            "x = <<EOF",
            "x = <<-EOF",
            "x = <<~EOF",
            "x = <<'EOF'",
            "x = <<\"EOF\"",
            "x = <<<EOF",
            "sql = <<~SQL",
        ] {
            let mut st = LexState::default();
            let _ = masked_line(line, &mut st, false);
            assert!(st.heredoc.is_some(), "must still open a heredoc: {line}");
        }
    }

    #[test]
    fn heredoc_opener_forms() {
        for line in ["x = <<EOF", "x = <<-EOF", "x = <<~EOF", "x = <<'EOF'", "x = <<\"EOF\"", "x = <<<EOF"] {
            let mut st = LexState::default();
            masked_line(line, &mut st, true);
            assert!(st.heredoc.is_some(), "should open: {line}");
        }
    }

    #[test]
    fn heredoc_countdown_releases_state() {
        // A false opener that never terminates must not swallow the rest of
        // the file: after HEREDOC_MAX_LINES the state is abandoned.
        let mut st = LexState::default();
        masked_line("x = <<EOF", &mut st, true);
        assert!(st.heredoc.is_some());
        for _ in 0..(HEREDOC_MAX_LINES + 2) {
            masked_line("{ still inside? }", &mut st, false);
        }
        assert!(st.heredoc.is_none(), "countdown must release the state");
        assert!(masked_line("fn real() {", &mut st, false).contains('{'));
    }

    #[test]
    fn python_triple_quote_spans_lines() {
        let mut st = LexState::default();
        assert!(!masked_line("s = \"\"\"a { b", &mut st, true).contains('{'));
        assert_eq!(st.ml, Ml::TripleDq);
        let l2 = masked_line("c } d\"\"\"", &mut st, true);
        assert_eq!(st.ml, Ml::None);
        assert!(!l2.contains('}'));
    }

    #[test]
    fn python_triple_single() {
        let mut st = LexState::default();
        assert!(!masked_line("s = '''x { y", &mut st, true).contains('{'));
        assert_eq!(st.ml, Ml::TripleSq);
        let l2 = masked_line("z } w'''", &mut st, true);
        assert_eq!(st.ml, Ml::None);
        assert!(!l2.contains('}'));
    }

    #[test]
    fn backtick_spans_lines() {
        let mut st = LexState::default();
        assert!(!masked_line("const t = `a { b", &mut st, false).contains('{'));
        assert_eq!(st.ml, Ml::Backtick);
        let l2 = masked_line("c } d`;", &mut st, false);
        assert_eq!(st.ml, Ml::None);
        assert!(!l2.contains('}'));
    }

    #[test]
    fn csharp_verbatim() {
        let mut st = LexState::default();
        assert!(!masked_line("var s = @\"a { b\"\"c", &mut st, false).contains('{'));
        assert_eq!(st.ml, Ml::Verbatim);
        let l2 = masked_line("d } e\";", &mut st, false);
        assert_eq!(st.ml, Ml::None);
        assert!(!l2.contains('}'));
    }

    #[test]
    fn rust_raw_string() {
        assert!(!mask1(r##"let s = r#"a { b } c"#;"##, false).contains('{'));
        assert!(!mask1(r###"let s = r##"a } b { c"##;"###, false).contains('{'));
    }

    #[test]
    fn rust_lifetime_not_a_string() {
        let m = mask1("fn f<'a>(x: &'a str) -> &'a str { x }", false);
        assert!(m.contains('{') && m.contains('}'));
    }

    #[test]
    fn rust_char_literal_brace() {
        assert!(!mask1("let c = '{';", false).contains('{'));
    }

    #[test]
    fn python_fstring_and_single_quote() {
        let fstr = ["p = f", "'", "{x} {y}", "'"].concat();
        assert!(!mask1(&fstr, true).contains('{'));
        let qstr = ["q = ", "'", "a { b } c", "'"].concat();
        assert!(!mask1(&qstr, true).contains('{'));
    }

    /// V99: a `''` pair never masks past its own line.
    ///
    /// Nix opens an indented string with `x = ''` and Ruby writes the empty
    /// string with the same bytes, so at end of line the two are identical. The
    /// asymmetry decides the rule: opening in Ruby blanks the rest of the FILE
    /// (1,596 of sinatra/base.rb's 2,174 lines vanished), while not opening in
    /// Nix merely tokenizes a string interior. The lexer takes the Ruby reading.
    #[test]
    fn double_quote_pair_never_spans_lines() {
        let mut st = LexState::default();
        // Ruby: `doodle = ''` is a complete empty string.
        masked_line("  doodle = ''", &mut st, true);
        assert!(st.ml == Ml::None, "an empty `\'\'` must not open a literal");
        // Nothing after it is masked.
        assert!(masked_line("  def two", &mut st, true).contains("two"));
        // A Nix-style opener is likewise closed on its own line, so a Nix
        // module body is indexed as code (the documented Nix limitation).
        let mut st2 = LexState::default();
        masked_line("  text = ''", &mut st2, true);
        assert!(st2.ml == Ml::None);
        assert!(masked_line("    function inner() {", &mut st2, true).contains("inner"));
    }

    #[test]
    fn brace_counts_single_pass() {
        let mut st = LexState::default();
        assert_eq!(brace_counts("fn f() { let s = \"{\"; }", &mut st, false), (1, 1));
        let mut st2 = LexState::default();
        assert_eq!(brace_counts("/* { */ }", &mut st2, false), (0, 1));
    }

    #[test]
    fn deterministic_same_input() {
        let a = mask1("fn f() { let s = \"x{}\"; }", false);
        let b = mask1("fn f() { let s = \"x{}\"; }", false);
        assert_eq!(a, b);
    }
}
