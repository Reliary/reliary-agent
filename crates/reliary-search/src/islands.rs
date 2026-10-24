//! Islands step 2 (SEGMENTER_SPIKE.md) — NOT wired into ingest.
//!
//! Klein & Manning CCM premise: definition headers are constituents with
//! predictable *interior category patterns* and sharp linear contexts.
//! This module counts interior patterns over bare categories — never
//! token values — and tests whether def-line shapes are the frequent
//! ones (the naturalness claim) without using any labels.

/// Bare token categories. Delimiter identity is structural (block
/// structure needs `(`/`{`/`:` distinguished); word *values* never enter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cat {
    Word,
    Num,
    OpenParen,
    CloseParen,
    OpenBrace,
    CloseBrace,
    OpenBracket,
    CloseBracket,
    Colon,
    Semi,
    Comma,
    Other,
}

/// Categorize a line into bare categories. Input should be the same
/// masked text the index tokenizes (`tokenizable_code` output): strings
/// and comments stripped, `#define` rebuilt as NAME + body. Quotes never
/// surface as structure.
pub fn categorize(masked: &str) -> Vec<Cat> {
    categorize_raw(masked)
}

/// Header-span pattern: the category sequence truncated after (and
/// including) the first structural delimiter — OpenParen, OpenBrace,
/// Colon, or OpenBracket. This is the CCM constituent interior plus
/// its boundary context: `unsigned long f(int x) { return 0; }` and
/// `int g(void);` share interiors with different boundaries, while
/// whole-line patterns would treat every body as a unique shape.
pub fn header_pattern(masked: &str) -> Vec<Cat> {
    // Bracket-region abstraction + angle-aware `:` truncation.
    // Balanced `<...>` regions (generics, bounds) are skipped: they hold
    // type arguments, never the defined name — `impl<T: Clone> Bar<T> {`
    // and `class Klass {` share the interior [W,W,{]. A `<` with no
    // matcher before the next anchor/end is an operator, kept as Other.
    // All positional (bracket matching); no token values read.
    let cats = categorize_raw(masked);
    let bytes = masked.as_bytes();
    // Skip spans: byte ranges inside balanced <...> regions (shared
    // helper — names never live inside angle brackets either).
    let skip = angle_skip_spans(masked);
    let mut out = Vec::new();
    let mut ci = 0usize;
    let mut i = 0usize;
    while i < bytes.len() && ci < cats.len() {
        let b = bytes[i];
        if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
            i += 1;
            continue;
        }
        let tok_start = i;
        // Consume the token starting at i to stay aligned with cats.
        if b.is_ascii_alphabetic() || b == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
        } else if b.is_ascii_digit() {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        } else {
            i += 1;
        }
        // Tokens fully inside a skipped <...> region vanish (generics).
        if (tok_start..i).all(|k| skip[k]) {
            ci += 1;
            continue;
        }
        let c = cats[ci];
        ci += 1;
        out.push(c);
        if matches!(c, Cat::OpenParen | Cat::OpenBrace | Cat::OpenBracket | Cat::Colon) {
            break;
        }
    }
    out
}

fn categorize_raw(masked: &str) -> Vec<Cat> {
    let bytes = masked.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphabetic() || b == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push(Cat::Word);
        } else if b.is_ascii_digit() {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            out.push(Cat::Num);
        } else {
            match b {
                b'(' => out.push(Cat::OpenParen),
                b')' => out.push(Cat::CloseParen),
                b'{' => out.push(Cat::OpenBrace),
                b'}' => out.push(Cat::CloseBrace),
                b'[' => out.push(Cat::OpenBracket),
                b']' => out.push(Cat::CloseBracket),
                b':' => out.push(Cat::Colon),
                b';' => out.push(Cat::Semi),
                b',' => out.push(Cat::Comma),
                b' ' | b'\t' | b'\r' | b'\n' => {}
                _ => out.push(Cat::Other),
            }
            i += 1;
        }
    }
    out
}

/// Count interior patterns over a corpus of lines. Returns
/// (pattern, count) sorted by count desc, pattern asc (deterministic).
pub fn count_patterns<'a, I>(lines: I) -> Vec<(Vec<Cat>, usize)>
where
    I: Iterator<Item = &'a str>,
{
    let mut counts: std::collections::BTreeMap<Vec<Cat>, usize> = Default::default();
    for line in lines {
        let pat = header_pattern(line);
        if pat.is_empty() {
            continue;
        }
        *counts.entry(pat).or_insert(0) += 1;
    }
    let mut out: Vec<(Vec<Cat>, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Generalize a header pattern: collapse the leading Word run to a
/// single Word (modifier tolerance: `int f(`, `static int f(`,
/// `export const x = (` share one interior). Calls and defs share
/// generalized interiors by design — boundary context + segment
/// position disambiguate at typing time (CCM: interior + context).
/// Pure shape op: no values read, no lists.
pub fn generalize(pat: &[Cat]) -> Vec<Cat> {
    let lead = pat.iter().take_while(|c| **c == Cat::Word).count();
    if lead <= 1 {
        return pat.to_vec();
    }
    let mut out = vec![Cat::Word];
    out.extend_from_slice(&pat[lead..]);
    out
}

/// A selected island: a generalized interior pattern worth storing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Island {
    pub pattern: Vec<Cat>,
    pub count: usize,
}

/// Greedy MDL island selection. Gain of storing a pattern with count c
/// and category-length L, given M distinct patterns observed and K
/// already selected:
///   gain = c * ln(M) - [L * ln(A) + c * ln(K+1)]
/// (background cost per line vs pattern codelength over an A-category
/// alphabet plus per-use index cost). Candidates considered in
/// count*len desc, pattern asc order; added while gain > 0.
/// Deterministic; zero tuned thresholds — every term is a codelength.
pub fn select_islands(ranked: &[(Vec<Cat>, usize)], distinct: usize) -> Vec<Island> {
    const ALPHABET: f64 = 12.0; // Cat has 12 variants
    let log_m = (distinct.max(1)) as f64;
    let log_m = log_m.ln();
    let log_a = ALPHABET.ln();
    let mut order: Vec<&(Vec<Cat>, usize)> = ranked.iter().collect();
    order.sort_by(|a, b| {
        (b.1 * b.0.len())
            .cmp(&(a.1 * a.0.len()))
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut selected = Vec::new();
    for (pat, c) in order {
        let gen = generalize(pat);
        // V83 B10: a delimiter-free pattern (`[Word]`, i.e. every pure
        // word-run line) carries zero shape information — it matches
        // `open Stdune`, `end`, `include struct` alike and "defines" the
        // last word of any such line. That is not an island; skip it.
        if gen.iter().all(|cat| *cat == Cat::Word) {
            continue;
        }
        if selected.iter().any(|isl: &Island| isl.pattern == gen) {
            continue;
        }
        let l = gen.len() as f64;
        let k = selected.len() as f64 + 1.0;
        let gain = *c as f64 * log_m - (l * log_a + *c as f64 * k.ln());
        if gain > 0.0 {
            selected.push(Island { pattern: gen, count: *c });
        }
    }
    selected
}

/// Byte ranges inside balanced <...> regions (generics, bounds).
/// Shared by header_pattern (abstraction) and name_before_anchor
/// (names never live inside angle brackets). A `<` with no matcher
/// before the next anchor/end is an operator, not a region start.
///
/// Blind-gate fix: `:` is NOT a region breaker. Rust generics carry
/// bounds (`Foo<T: Bound>`, `impl<T: Clone>`) — breaking at `:` made
/// the bound's colon the header anchor, so every bounded generic
/// typed its `T` instead of its name (tokio: ~80 misses). `a < b:`
/// comparisons still work: no `>` follows, so no region forms.
/// `(`, `{`, `[`, `;` still break (call/group/array/statement
/// delimiters outrank `<`; `a < b; c > d` must not fuse).
pub fn angle_skip_spans(masked: &str) -> Vec<bool> {
    let bytes = masked.as_bytes();
    let mut skip = vec![false; bytes.len()];
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let mut depth = 1i32;
            let mut j = i + 1;
            while j < bytes.len() && depth > 0 {
                if bytes[j] == b'<' {
                    depth += 1;
                } else                 if bytes[j] == b'>' {
                    depth -= 1;
                } else if matches!(bytes[j], b'(' | b'{' | b'[' | b';') {
                    break;
                }
                j += 1;
            }
            if depth == 0 {
                let end = j.min(bytes.len());
                if end > i {
                    skip[i..end].fill(true);
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    skip
}

/// Last identifier before the first structural anchor delimiter
/// (`(`, `{`, `:`, `[`), or the last word if no anchor. Angle-bracket
/// regions are skipped (the name is never `T` in `Foo<T> {`).
/// A trailing adjacent punctuation run joins the name (`operator==`):
/// adjacency (no space) is the positional signal. Positional only:
/// values are picked, never scored.
pub fn name_before_anchor(masked: &str) -> Option<String> {
    let bytes = masked.as_bytes();
    let skip = angle_skip_spans(masked);
    // Leading qualifier-group skip (blind-gate find): a `word(...)`
    // group at line start is a visibility/attribute qualifier
    // (`pub(crate)`, `pub(in path)`) when more header follows it —
    // the signature anchor lies AFTER the group. But when the group
    // ENDS the header (`foo(x);`, `if (x) {`), it IS the anchor
    // (call or control line — legacy behavior preserved).
    // Positional: single leading word + balanced group + anchor/word
    // after the matching paren. No word lists.
    let mut base = 0usize;
    {
        let t = masked.trim_start();
        let lead_end = t
            .bytes()
            .position(|c| !(c.is_ascii_alphanumeric() || c == b'_'))
            .unwrap_or(t.len());
        if lead_end > 0 && lead_end < t.len() && t.as_bytes()[lead_end] == b'(' {
            // Single word then `(`: find its matching `)`.
            let mut depth = 0i32;
            let tb = t.as_bytes();
            let mut j = lead_end;
            let mut close = None;
            while j < tb.len() {
                if tb[j] == b'(' {
                    depth += 1;
                } else if tb[j] == b')' {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(j);
                        break;
                    }
                }
                j += 1;
            }
            if let Some(c) = close {
                let after = &t[c + 1..];
                let after_trim = after.trim_start();
                // A WORD follows the group (`) struct X {`, `) WORD`):
                // the group is a qualifier (visibility/attribute) and
                // the signature anchor lies after it — skip. A bare
                // anchor follows (`method() {`, `if (x) {`, `foo(x);`):
                // the group IS the header (def, control, or call) —
                // legacy behavior preserved. Terminator/EOL: anchor too.
                if let Some(f) = after_trim.bytes().next() {
                    if f.is_ascii_alphabetic() || f == b'_' {
                        base = masked.len() - t.len() + c + 1;
                    }
                }
            }
        }
    }
    // Find the anchor: first (, {, :, [ outside skipped regions.
    let mut anchor = bytes.len();
    let mut ai = base;
    while ai < bytes.len() {
        let b = bytes[ai];
        if !skip[ai] && (b == b'(' || b == b'{' || b == b':' || b == b'[') {
            anchor = ai;
            break;
        }
        ai += 1;
    }
    // V84 P-F: a type-declaration clause (`extends ...` / `implements ...`)
    // at angle depth 0 terminates the NAME region. `interface Foo extends
    // Bar {` has the same shape as a plain declaration, so the last word
    // before the anchor is the SUPERCLASS (`Bar`) — 379 zod interface sites
    // were bound to their parent type. The declared name is the last word
    // BEFORE the clause. Angles are tracked so `Foo<T extends U>` (clause
    // inside the parameter list) is untouched.
    let name_region_end = {
        let mut cut = anchor;
        let mut angle: i32 = 0;
        let mut k = base;
        while k < anchor {
            match bytes[k] {
                b'<' => angle += 1,
                b'>' if angle > 0 => angle -= 1,
                _ => {}
            }
            if angle == 0 && k > base && (bytes[k - 1] == b' ' || bytes[k - 1] == b'\t') {
                for kw in [b"extends ".as_slice(), b"implements ".as_slice()] {
                    if bytes[k..].starts_with(kw) {
                        cut = k;
                        break;
                    }
                }
                if cut != anchor {
                    break;
                }
            }
            k += 1;
        }
        cut
    };

    // Last word before the anchor, outside skipped regions.
    let mut last: Option<(usize, usize)> = None;
    let mut i = base;
    while i < anchor && i < name_region_end {
        let b = bytes[i];
        if (b.is_ascii_alphabetic() || b == b'_') && !skip[i] {
            let s = i;
            while i < anchor
                && i < name_region_end
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                && !skip[i]
            {
                i += 1;
            }
            last = Some((s, i));
        } else {
            i += 1;
        }
    }
    let (s, e) = last?;
    // Extend through an adjacent trailing OPERATOR run: `operator==(` ->
    // `operator==`. Adjacency (no space) is the signal; the run stops at
    // brackets, terminators, and whitespace — so `Foo<` never forms
    // (the `<` opens a skipped region) and `b;` never forms either.
    // Set is punctuation classes, never words: no values read.
    let mut end = e;
    while end < anchor
        && matches!(
            bytes[end],
            b'=' | b'!' | b'&' | b'|' | b'+' | b'-' | b'*' | b'/' | b'%' | b'^' | b'~' | b'?' | b'.'
        )
    {
        end += 1;
    }
    // IMPL-FOR (`impl Drop for BlockingPool`): a name preceded on the
    // line by ` for ` (spaced, before the name) is the trait-impl
    // ARGUMENT, not an introduction — the trait and the block are the
    // structure. The evidence precedes because extraction stops AT the
    // qualifier position... here the name is LAST, so `for` already
    // passed. ` for ` is positional (spaced, same-line); a line-start
    // `for` (`for i in range():`) lacks the leading space only when
    // unindented — indented loop headers water their `range`-shaped
    // callee too, which is also correct (it's a use).
    {
        let b2 = masked.as_bytes();
        let mut j = s;
        while j > 0 && (b2[j - 1] == b' ' || b2[j - 1] == b'\t') {
            j -= 1;
        }
        let preceded = j > 0 && end == e && matches!(b2[j - 1], b'.' | b':' | b'>');
        let rest = &masked[end..];
        let followed = rest.starts_with("::");
        let is_impl_arg = masked[..e].contains(" for ");
        if (preceded || followed || is_impl_arg) && end == e {
            // end == e gates all: names WITH a trailing operator run
            // (`operator==`) are overloads, never qualifiers/args.
            return None;
        }
    }
    // Numbers never qualify (a trailing number is a value, not a name).
    let name = masked[s..end].to_string();
    if name.bytes().next().map(|b| b.is_ascii_digit()).unwrap_or(true) {
        // Name starts with a digit: fall back to the bare word only if
        // the word itself is non-numeric, else no name.
        let word = masked[s..e].to_string();
        if word.bytes().next().map(|b| b.is_ascii_digit()).unwrap_or(true) {
            return None;
        }
        return Some(word);
    }
    Some(name)
}

/// Block-scoped backward vocabulary: a stack of per-depth word sets.
/// A name is visible if present in ANY set on the current descent path
/// (a use in an inner block of an outer def waters). Callers keep the
/// stack; classification borrows it — no per-line union clone. The old
/// `chained: HashSet = stack.flat_map(cloned).collect()` was O(L*V)
/// String copies per file (the #1 flag-path cost; ~10^8 clones on 880
/// files). `any(contains)` over a depth-≤~10 stack is the same predicate
/// without the copy.
pub type Backward = [std::collections::HashSet<String>];

/// V83 B7: repeat-definition evidence. A name already present in the
/// backward vocabulary (a use or an earlier def) types again only when
/// its header carries a declaration word before the extracted name:
/// `fn spawn(`, `mod sys {`, `macro_rules! x {`, `def dup(self):`,
/// `class B:`, `const EXECUTOR ...`. Calls and uses carry none, so
/// `alpha();` after `fn alpha()` waters, and a second `describe("x", () => {`
/// in JS never types. `mod` and the trailing `!` of `macro_rules!` are
/// handled here; everything else reuses the shared declaration predicate.
/// Positional/word-class: no statement about program meaning.
#[inline]
pub fn repeat_def_evidence(masked: &str, name: &str) -> bool {
    let Some(pos) = masked.rfind(name) else {
        return false;
    };
    let before = masked[..pos].trim_end();
    // Word-boundary `mod` (`hotmod sys` is not a mod line).
    if before == "mod" || before.ends_with(" mod") {
        return true;
    }
    let before = before.trim_end_matches('!').trim_end();
    // `let f x =` / `let rec f x =` (OCaml/ML equations) carry `let`
    // (and `rec`) rather than an English declaration word.
    if before == "let" || before.ends_with(" let") || before.ends_with(" let rec") {
        return true;
    }
    crate::structural::has_decl_keyword_before(before)
        || crate::structural::has_type_keyword_before(before)
}

/// Is `w` visible in the block-scoped stack (present in any frame)?
#[inline]
pub fn backward_contains(backward: &Backward, w: &str) -> bool {
    backward.iter().any(|s| s.contains(w))
}

/// One-shot header analysis (A2b). The old classify path recomputed
/// `header_pattern` (categorize + angle skip) 4-5× per line — twice
/// inside `header_anchor` alone. This computes pattern, its
/// generalization, and the anchor once; callers thread the view.
pub struct HeaderView {
    pub general: Vec<Cat>,
    pub anchor: Option<Anchor>,
}

pub fn analyze_header(masked: &str) -> HeaderView {
    let pat = header_pattern(masked);
    let general = generalize(&pat);
    let anchor = match pat.last() {
        Some(Cat::OpenParen) => Some(Anchor::Paren),
        Some(Cat::OpenBrace) => Some(Anchor::Brace),
        Some(Cat::Colon) => Some(Anchor::Colon),
        Some(Cat::OpenBracket) => Some(Anchor::Bracket),
        _ => None,
    };
    HeaderView { general, anchor }
}

/// Typing rule with a precomputed header view. A masked line is a
/// definition iff:
/// (1) its generalized header matches a selected island (common shapes,
///     position-independent: prototypes included), or
/// (2) geometric fallback: the line opens a DP segment AND ends with a
///     block-open delimiter (`{`/`:`) — rare shapes with sharp
///     boundaries, no per-shape content.
/// Returns the positional name when def.
/// V83 B8: declaration-line route. The first identifier whose immediately
/// preceding word is a declaration keyword (`class Basic < Base`,
/// `module Rack`, `def dup`, `mod sys`, `struct Foo;`) AND whose following
/// non-space byte is a declaration delimiter class (`( < > : { ; = !` or
/// EOL) is the defined name. Two Ruby/other-language holes this closes:
/// `class Basic < AbstractHandler` had no `(`/`{`/`:` anchor, so the
/// island route named the SUPERCLASS (last word); `module Rack` had no
/// anchor at all and never typed. The delimiter-after-name check keeps
/// prose (`The class Bar is used`) out: a word follows the name there.
/// Positional: keyword-word check + next-byte class; no values.
fn decl_line_name(masked: &str) -> Option<String> {
    let bytes = masked.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let s = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
            {
                i += 1;
            }
            let e = i;
            let prefix = masked[..s].trim_end();
            let prefix = prefix.trim_end_matches('!');
            // V83 B8 guard: a declarator keyword IMMEDIATELY preceded by a
            // type-operator byte is TYPE syntax, not a declaration:
            // `cell: &'a Cell<*const T>,` (`const` describes the pointer),
            // `foo: &'static str,`, `p: fn(...)`. Naming `T`/`str` from
            // those cost two real field defs on the tokio corpus (`cell`,
            // `map`). Only the keywords that appear inside Rust/C types
            // need the check; `fn` from function-pointer types is caught by
            // the same `(` case.
            let type_syntax = ["const", "static", "void", "fn"].iter().any(|kw| {
                prefix
                    .strip_suffix(kw)
                    .and_then(|rest| rest.as_bytes().last())
                    .map(|b| matches!(b, b'*' | b'&' | b'<' | b'(' | b'\''))
                    .unwrap_or(false)
            });
            if !prefix.is_empty()
                && !type_syntax
                && crate::structural::has_decl_keyword_before(prefix)
            {
                let mut j = e;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                let ok = j >= bytes.len()
                    || matches!(
                        bytes[j],
                        b'(' | b'<' | b'>' | b':' | b'{' | b';' | b'=' | b'!' | b'.'
                    );
                if ok {
                    return Some(masked[s..e].to_string());
                }
            }
            continue;
        }
        i += 1;
    }
    None
}

pub fn island_def_view(
    masked: &str,
    view: &HeaderView,
    seg_start: bool,
    islands: &[Island],
) -> Option<String> {
    // V83 B8: declaration lines name the first identifier after the
    // keyword (`class Basic < Base` -> Basic, not Base). Before the
    // island route: a no-anchor inheritance line otherwise names the
    // superclass through the last-word fallback.
    if let Some(n) = decl_line_name(masked) {
        return Some(n);
    }
    if islands.iter().any(|isl| {
        isl.pattern == view.general
            && isl.pattern.iter().any(|cat| *cat != Cat::Word)
    }) {
        RULE1_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return name_before_anchor(masked);
    }
    let t = masked.trim_end();
    if seg_start && (t.ends_with('{') || t.ends_with(':')) {
        RULE2_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return name_before_anchor(masked);
    }
    None
}

/// Instrumentation: rule-1 (island match) vs rule-2 (segment fallback)
/// hit counts. Lets the DP-gate decision be measured, not assumed.
pub static RULE1_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static RULE2_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn rule_hit_counts() -> (u64, u64) {
    (
        RULE1_HITS.load(std::sync::atomic::Ordering::Relaxed),
        RULE2_HITS.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Typing rule. A masked line is a definition iff:
/// (1) its generalized header matches a selected island (common shapes,
///     position-independent: prototypes included), or
/// (2) geometric fallback: the line opens a DP segment AND ends with a
///     block-open delimiter (`{`/`:`) — rare shapes with sharp
///     boundaries, no per-shape content.
/// Returns the positional name when def.
pub fn island_def(masked: &str, seg_start: bool, islands: &[Island]) -> Option<String> {
    let view = analyze_header(masked);
    island_def_view(masked, &view, seg_start, islands)
}

/// Shadow classifier (step 4): value-free per-line classification for
/// side-by-side audit against `classify_structural`. NOT wired into
/// ingest. Callers feed lines in order with the project's islands and
/// the file's DP segment starts.
///
/// Rule: island-or-fallback gate (above), then new-name introduction —
/// the candidate name must be absent from the backward vocabulary
/// (words seen on earlier lines). Binding a new name IS the
/// definition; `if (x)` reuses `x`, calls reuse their callee. First
/// forward use per file is the bounded miss (recorded, not hidden).
/// Tag by anchor: `(` at depth>0 -> method(3), `(` at depth 0 -> fn(1),
/// otherwise type(2). Returns (name, tag).
/// `guard` is the optional corpus IDF guard (None = allow; the novelty
/// rule still applies). Thread it from the index; never from a list.
pub fn classify_island_line(
    masked: &str,
    seg_start: bool,
    depth: i32,
    backward: &Backward,
    islands: &[Island],
    guard: Option<&DfGuard>,
) -> Option<(String, u8)> {
    classify_island_line_view(
        masked,
        &analyze_header(masked),
        seg_start,
        depth,
        backward,
        islands,
        guard.map(|g| g as &dyn DfLookup),
    )
}

/// A2b: classifier core taking a precomputed [`HeaderView`] and a
/// deferred [`DfLookup`] (so the JIT path resolves doc frequency only
/// for the rare anchorless candidate, not every token of every line).
pub fn classify_island_line_view(
    masked: &str,
    view: &HeaderView,
    seg_start: bool,
    depth: i32,
    backward: &Backward,
    islands: &[Island],
    guard: Option<&dyn DfLookup>,
) -> Option<(String, u8)> {
    classify_island_line_view_ext(masked, view, seg_start, depth, backward, islands, guard)
}

/// V83 B4b: extended core taking the per-file def-indent ledger. When the
/// name is already in the backward vocabulary, a REPEAT def may still
/// type iff the line opens a layout block (`:` terminal) and its leading
/// indent equals an indent at which this name was previously introduced
/// as a def in this file. Python/Ruby class bodies repeat `def dup` at
/// the same indent in every class; a use (`if dup():`, `dup()`, a call
/// inside a deeper block) is at a different indent or lacks the `:`
/// blocker. Positional: terminal byte class + indent equality; no values.
#[allow(clippy::too_many_arguments)]
pub fn classify_island_line_view_ext(
    masked: &str,
    view: &HeaderView,
    seg_start: bool,
    depth: i32,
    backward: &Backward,
    islands: &[Island],
    guard: Option<&dyn DfLookup>,
) -> Option<(String, u8)> {
    let name = island_def_view(masked, view, seg_start, islands)?;
    if backward_contains(backward, &name) && !repeat_def_evidence(masked, &name) {
        return None;
    }
    // IDF guard scope: anchorless headers only. Lone-word lines
    // (`where`, `else`, `return`) carry no structural evidence, so
    // corpus frequency decides. Anchored shapes (`get(`, `Foo {`)
    // carry their own evidence; common method names (get/new/set)
    // must still type. Scoping is structural, not a value list.
    if view.anchor.is_none() {
        if let Some(g) = guard {
            if !g.allows(&crate::stem_identifier(&name)) {
                return None;
            }
        }
    }
    let tag = if view.anchor == Some(Anchor::Paren) {
        if depth > 0 {
            3
        } else {
            1
        }
    } else if view.anchor.is_none() && masked.trim_end().ends_with(';') {
        // Declaration without body (`int f(void);`): a declaration IS
        // the definition site for location purposes (ctags agrees).
        1
    } else {
        2
    };
    Some((name, tag))
}

/// Raw-line entry: `raw` is the original source line, `masked` the
/// production-masked text. Two character-gated routes (no values):
/// `@`-led lines (decorators/annotations) are uses, never defs;
/// `#`-led lines route to the shipped preprocessor adjudicator
/// (defines name their name; all other directives are water).
///
/// Delegation order is ACYCLIC by construction: gates (-> core below).
/// `classify_island_line` never calls back into this function.
/// (A mutual-recursion incident hung the test runner; do not re-add.)
pub fn classify_island_line_raw(
    raw: &str,
    masked: &str,
    seg_start: bool,
    depth: i32,
    backward: &Backward,
    islands: &[Island],
    guard: Option<&DfGuard>,
) -> Option<(String, u8)> {
    classify_island_line_raw_view(
        raw,
        masked,
        &analyze_header(masked),
        seg_start,
        depth,
        backward,
        islands,
        guard.map(|g| g as &dyn DfLookup),
    )
}

/// A2b: raw-line entry taking a precomputed [`HeaderView`] and a
/// deferred [`DfLookup`]. `raw` is the original source line, `masked`
/// the production-masked text. Two character-gated routes (no values):
/// `@`-led lines (decorators/annotations) are uses, never defs;
/// `#`-led lines route to the shipped preprocessor adjudicator
/// (defines name their name; all other directives are water).
///
/// Delegation order is ACYCLIC by construction: gates (-> core below).
#[allow(clippy::too_many_arguments)]
pub fn classify_island_line_raw_view(
    raw: &str,
    masked: &str,
    view: &HeaderView,
    seg_start: bool,
    depth: i32,
    backward: &Backward,
    islands: &[Island],
    guard: Option<&dyn DfLookup>,
) -> Option<(String, u8)> {
    let t = raw.trim_start();
    if t.starts_with('@') {
        return None;
    }
    // V83 B6: control-flow headers are never definitions (legacy H4
    // parity). Their trailing `(`/`:` anchor makes `if cond():`/`while
    // (x) {`/`for (i) {` island-shaped, so the island arm typed the
    // condition word as a def. Positional: a leading control word
    // followed by a space, word-boundary checked.
    for kw in ["if", "while", "for", "match", "loop", "switch", "try",
               "catch", "else", "elif", "case", "unless", "until", "when",
               "return", "import"] {
        if let Some(rest) = t.strip_prefix(kw) {
            if rest.starts_with(' ') || rest.starts_with('\t') {
                return None;
            }
        }
    }
    if t.starts_with('#') {
        match crate::structural::classify_preprocessor_line(raw) {
            Some(crate::structural::Preproc::Def { name, .. }) => {
                let n = name.to_string();
                if backward_contains(backward, &n) {
                    return None;
                }
                if let Some(g) = guard {
                    if !g.allows(&crate::stem_identifier(&n)) {
                        return None;
                    }
                }
                return Some((n, 1));
            }
            _ => return None,
        }
    }
    // `=`-line rule (V66c, positional): a top-level ASSIGNMENT `=`
    // binds (a use), unless a depth-0 `=>` marks an arrow body (the
    // assignee IS the def). Comparisons (`==`, `<=`, `>=`, `!=`) are
    // not assignments: an `=` adjacent to another operator char is
    // part of a comparison token. Top-level = paren-depth 0.
    // `let y = foo(` waters; `const f = (x) => ...` types;
    // `let y = foo(x => x)` waters (arrow inside the call's parens);
    // `bool operator==(...)` types (comparison, not assignment).
    // `=`/`>` chars only.
    {
        let b = masked.as_bytes();
        let mut depth = 0i32;
        let mut has_assign = false;
        let mut has_arrow0 = false;
        let mut i = 0usize;
        while i < b.len() {
            match b[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' if depth > 0 => depth -= 1,
                b'=' if depth == 0 => {
                    let prev = if i > 0 { b[i - 1] } else { b' ' };
                    let next = if i + 1 < b.len() { b[i + 1] } else { b' ' };
                    if next == b'>' {
                        has_arrow0 = true;
                    } else if next != b'='
                        && prev != b'='
                        && prev != b'<'
                        && prev != b'>'
                        && prev != b'!'
                    {
                        has_assign = true;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if has_assign && !has_arrow0 {
            // V83 B1/B2: a declaration-assignment (`type X = ...`,
            // `const X = ...`, `static X: T = ...`, PascalCase or
            // SCREAMING_CASE bound name) is a symbol definition, not a
            // local binding. 381 type-alias + 69 module-constant sites
            // on blind tokio were watered here. Novelty still applies
            // (repeat defs need declaration evidence, same as B7).
            if let Some(n) = crate::structural::decl_assign_name(masked) {
                if !backward_contains(backward, n) || repeat_def_evidence(masked, n) {
                    return Some((n.to_string(), 2));
                }
            }
            // V83 B9: equation rule (`foo x = ...`, `let f x = ...`).
            // The SHAPE is its own declaration evidence: indent-0, a
            // lowercase first word, arguments before the `=`. Unlike a
            // bare call, no other language reads this shape as anything
            // but a binding-with-arguments (calls carry `(`/`[`; bindings
            // have the name last). Novelty therefore does NOT gate it:
            // a top-level equation whose name was already sighted in a
            // module export list or an import still defines it
            // (`arrayValues` in Data.Aeson.Parser.Internal).
            if let Some(n) = crate::structural::equation_name(masked, depth) {
                return Some((n.to_string(), 1));
            }
            // V84 P-D: module-scope point-free/constant definitions. Same
            // rule as the legacy classifier so both arms agree.
            if let Some(n) = crate::structural::pointfree_name(masked, depth) {
                return Some((n.to_string(), 1));
            }
            return None;
        }
    }
    // The `=`-rule block scopes its own `depth`; the parameter `depth`
    // (brace depth) is what the tag rule consumes, exactly as before.
    classify_island_line_view_ext(masked, view, seg_start, depth, backward, islands, guard)
}

/// IDF (keyword-noise) guard data: stem -> number of files containing
/// it, plus the total file count. A candidate def name occurring in
/// more than half the corpus files is structural noise (`else`,
/// `where`, `return`-shaped lines) — never a definition.
/// Majority (1/2) is the symmetry point, not a tuned threshold:
/// a name defined in most files is a contradiction (definitions
/// introduce names; corpus-wide names are vocabulary, not symbols).
pub struct DfGuard {
    /// stem -> doc frequency
    pub df: std::collections::HashMap<String, usize>,
    pub total_files: usize,
}

impl DfGuard {
    pub fn allows(&self, stem: &str) -> bool {
        if self.total_files == 0 {
            return true;
        }
        match self.df.get(stem) {
            // Majority of files contain it -> noise, never a def.
            Some(&d) => d * 2 <= self.total_files,
            // Unseen stem (e.g. JIT single-file context): allow; the
            // novelty rule still applies.
            None => true,
        }
    }
}

/// Deferred lookup so a caller that would otherwise scan every token of
/// every line to prebuild a map can instead resolve the doc frequency of
/// just the (rare) anchorless candidate name. Same predicate as
/// `DfGuard::allows`. Kept as a trait so the eager map and the lazy
/// resolver are interchangeable at every call site.
pub trait DfLookup {
    fn allows(&self, stem: &str) -> bool;
}

impl DfLookup for DfGuard {
    fn allows(&self, stem: &str) -> bool {
        DfGuard::allows(self, stem)
    }
}
/// Load the IDF guard from the index: stem -> files containing it
/// (via file_phrases), plus the source file count. Used at query/JIT
/// time to water corpus-wide noise names (`else`, `where`).
/// Majority rule; see DfGuard::allows.
pub fn load_df_guard(db: &rusqlite::Connection) -> DfGuard {
    use std::collections::HashMap;
    let mut df: HashMap<String, usize> = HashMap::new();
    let total_files: usize = db
        .query_row(
            "SELECT COUNT(*) FROM file_map WHERE is_source = 1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as usize)
        .unwrap_or(0);
    // phrase text per id, then df per phrase via file_phrases.
    // Two queries, both indexed; runs once per JIT build / query path
    // entry (callers cache per connection generation).
    let pairs: Vec<(String, usize)> = match db.prepare(
        "SELECT p.phrase, COUNT(DISTINCT f.file_id) FROM phrases p
         JOIN file_phrases f ON f.phrase_id = p.id
         GROUP BY p.id",
    ) {
        Ok(mut stmt) => match stmt.query_map([], |r| {
            let phrase: String = r.get(0)?;
            let c: i64 = r.get(1)?;
            Ok((phrase, c.max(0) as usize))
        }) {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    };
    for (phrase, c) in pairs {
        df.insert(phrase, c);
    }
    DfGuard { df, total_files }
}
/// Receiver test (B4): is the word at byte offset `start` reached
/// through a receiver/qualifier? Walks back over spaces/tabs in the
/// masked line; a `.` or `:` (covers `.`/`::`/`->`-tail/`a.b:`) means
/// the name is a use through qualification, never an introduction —
/// so it must not enter the backward vocabulary (it can't define, so
/// it must not vaccinate later lines either). Character-level only:
/// no words read, no lists. `start` must be a word-start offset.
pub fn is_receiver(masked: &str, start: usize) -> bool {
    let b = masked.as_bytes();
    if start == 0 || start > b.len() {
        return false;
    }
    let mut i = start;
    while i > 0 && (b[i - 1] == b' ' || b[i - 1] == b'\t') {
        i -= 1;
    }
    if i == 0 {
        return false;
    }
    matches!(b[i - 1], b'.' | b':')
}

/// Import-group test (blind-gate find): does this line OPEN a brace
/// group through a path qualifier (`use std::{`, `import a::{`)?
/// A `{` whose previous significant byte is `:` opens a name LIST,
/// not a body — bare-word lines inside such groups (`Arc,` continuations)
/// are re-exports, never introductions. `) {` (qualified defs),
/// word-`{` (enums/impls) are bodies, not lists. Character-level only.
pub fn opens_import_group(masked: &str) -> bool {
    let b = masked.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'{' {
            let mut j = i;
            while j > 0 && (b[j - 1] == b' ' || b[j - 1] == b'\t') {
                j -= 1;
            }
            if j >= 2 && b[j - 1] == b':' && b[j - 2] == b':' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Def-name to occurrence-token link. Exact match, or the def name is
/// the token plus a trailing operator run (`operator==` claims the
/// `operator` token scan_identifiers yields — punctuation never
/// survives tokenization). The remainder must start with a
/// non-identifier char, so `get` never claims `getters`.
/// Positional/shape only; no values.
pub fn def_name_matches_token(def_name: &str, token: &str) -> bool {
    if def_name == token {
        return true;
    }
    if def_name.len() > token.len() && def_name.starts_with(token) {
        let next = def_name[token.len()..].bytes().next().unwrap();
        return !next.is_ascii_alphanumeric() && next != b'_';
    }
    false
}
/// The structural anchor terminating the header span, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Paren,
    Brace,
    Colon,
    Bracket,
    None,
}

pub fn header_anchor(masked: &str) -> Option<Anchor> {
    let pat = header_pattern(masked);
    match pat.last() {
        Some(Cat::OpenParen) => Some(Anchor::Paren),
        Some(Cat::OpenBrace) => Some(Anchor::Brace),
        Some(Cat::Colon) => Some(Anchor::Colon),
        Some(Cat::OpenBracket) => Some(Anchor::Bracket),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimiter_free_patterns_never_select_or_type() {
        // V83 B10: `[Word]` (and any all-Word generalization) matches every
        // word-run line (`open Stdune`, `end`, prose) and "defined" its last
        // word — a phantom-def factory. It must never be selected.
        use super::*;
        let ranked = vec![
            (vec![Cat::Word], 100usize),
            (vec![Cat::Word, Cat::Word], 50),
            (vec![Cat::Word, Cat::OpenParen], 10),
        ];
        let selected = select_islands(&ranked, ranked.len());
        assert!(
            !selected.iter().any(|i| i.pattern.iter().all(|c| *c == Cat::Word)),
            "delimiter-free pattern must not be selected: {:?}",
            selected
        );
        assert!(selected.iter().any(|i| i.pattern == vec![Cat::Word, Cat::OpenParen]));
    }

    #[test]
    fn words_and_delims_categorized() {
        use Cat::*;
        assert_eq!(
            categorize("int foo(int x) {"),
            vec![Word, Word, OpenParen, Word, Word, CloseParen, OpenBrace]
        );
    }

    #[test]
    fn strings_masked_before_categorize() {
        // Contract: callers pass production-masked text. Masked input
        // contains no string contents, so no phantom delimiters surface.
        let masked = crate::structural::tokenizable_code("const s = \"foo(bar\";");
        let pat = categorize(&masked);
        assert!(!pat.contains(&Cat::OpenParen));
    }

    #[test]
    fn python_colon_shape() {
        use Cat::*;
        assert_eq!(
            categorize("def foo(x):"),
            vec![Word, Word, OpenParen, Word, CloseParen, Colon]
        );
    }

    #[test]
    fn counts_sorted_deterministic() {
        let lines = vec!["a b", "a b", "c"];
        let counts = count_patterns(lines.into_iter());
        assert_eq!(counts[0].1, 2);
        assert_eq!(counts.len(), 2);
    }
}
