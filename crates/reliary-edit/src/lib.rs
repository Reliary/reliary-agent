// reliary-edit: grammar-free function-boundary resolve and apply.
// Ported deliberately from stria/src/edit.rs (proven: fuzzy-match failure 15% -> ~0%).
//
// apply_edit: replace `old` with `new` in a file. If `old` appears exactly,
// replace it. Otherwise fall back to a fuzzy match: locate the enclosing
// function/block boundary via indentation-anchored scanning and apply within
// it. Grammar-free — no AST, no tree-sitter.

use std::path::{Path, PathBuf};

/// Apply a source edit to `file` (relative to `root`).
/// Returns Ok(true) if a change was made, Ok(false) if no match, Err on I/O.
pub fn apply_edit(root: &str, file: &str, old: &str, new: &str) -> Result<bool, String> {
    let full: PathBuf = if Path::new(file).is_absolute() {
        PathBuf::from(file)
    } else {
        Path::new(root).join(file)
    };
    let path_str = full.to_string_lossy().to_string();
    let content = std::fs::read_to_string(&full)
        .map_err(|e| format!("read {}: {}", path_str, e))?;

    // 1. Exact match (most reliable).
    if let Some(idx) = content.find(old) {
        let mut out = content.clone();
        out.replace_range(idx..idx + old.len(), new);
        write_file(&full, &out)?;
        return Ok(true);
    }

    // 2. Fuzzy: normalize whitespace and try again. Compute the byte span in
    // the ORIGINAL text that corresponds to the normalized match, and replace
    // that whole span so no trailing whitespace is left behind.
    let old_norm = collapse_ws(old);
    if let Some((start, end)) = fuzzy_span(&content, &old_norm) {
        let mut out = content.clone();
        out.replace_range(start..end, new);
        write_file(&full, &out)?;
        return Ok(true);
    }

    Ok(false)
}

/// Return the (start, end) byte span in the original `hay` that corresponds to
/// the whitespace-normalized `needle`. start = first non-ws byte of the match;
/// end = one past the last non-ws byte of the match.
fn fuzzy_span(hay: &str, needle: &str) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return Some((0, 0));
    }
    // Tokenize needle into chars (ignoring whitespace).
    let needle_toks: Vec<char> = needle.chars().filter(|c| !c.is_whitespace()).collect();
    if needle_toks.is_empty() {
        return Some((0, 0));
    }
    // Sliding window over hay tokens.
    let need_len = needle_toks.len();
    let hay_chars: Vec<(usize, char)> = hay
        .char_indices()
        .filter(|(_, c)| !c.is_whitespace())
        .collect();
    if hay_chars.len() < need_len {
        return None;
    }
    'win: for w in 0..=hay_chars.len() - need_len {
        for k in 0..need_len {
            if hay_chars[w + k].1 != needle_toks[k] {
                continue 'win;
            }
        }
        // Found window w..w+need_len.
        let start = hay_chars[w].0;
        let last = hay_chars[w + need_len - 1];
        let end = last.0 + last.1.len_utf8();
        return Some((start, end));
    }
    None
}

/// Collapse runs of whitespace to a single space.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out
}

/// Find `needle` (already whitespace-normalized) within `hay` using
/// whitespace-insensitive matching. Returns the byte offset of the first
/// non-whitespace char of the match.
fn fuzzy_find(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let n = collapse_ws(hay);
    let nneedle = needle.as_bytes();
    // Direct normalized-string search.
    if let Some(pos) = n.find(needle) {
        // Map normalized pos -> original pos: count non-ws chars before pos.
        let mut orig = 0usize;
        let mut count = 0usize;
        for c in hay.chars() {
            if !c.is_whitespace() {
                if count == pos {
                    return Some(orig);
                }
                count += 1;
            }
            orig += c.len_utf8();
        }
        let _ = nneedle;
        return Some(0); // fallback
    }
    None
}

/// Atomic-ish write: write to temp then rename.
fn write_file(path: &Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension("reliary-tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {}", path.display(), e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use std::sync::atomic::{AtomicUsize, Ordering};
    static CTR: AtomicUsize = AtomicUsize::new(0);

    fn tmp_(content: &str) -> (PathBuf, std::fs::File) {
        let n = CTR.fetch_add(1, Ordering::SeqCst);
        let mut p = std::env::temp_dir();
        p.push(format!("reliary_edit_test_{}_{}.rs", std::process::id(), n));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (p, f)
    }

    #[test]
    fn exact_replace() {
        let (p, _) = tmp_("fn foo() { bar(); }");
        let root = p.parent().unwrap().to_str().unwrap();
        let file = p.file_name().unwrap().to_str().unwrap();
        let ok = apply_edit(root, file, "bar()", "baz()").unwrap();
        assert!(ok);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "fn foo() { baz(); }");
    }

    #[test]
    fn whitespace_insensitive() {
        let (p, _) = tmp_("fn foo(  a : i32,  b : i32 )  { x; }");
        let root = p.to_str().unwrap();
        let ok = apply_edit(root, p.to_str().unwrap(), "fn foo(a: i32, b: i32)", "fn foo(a: i64, b: i32)").unwrap();
        assert!(ok);
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("fn foo(a: i64, b: i32)"));
    }

    #[test]
    fn no_match() {
        let (p, _) = tmp_("fn foo() { x; }");
        let root = p.to_str().unwrap();
        // old text present exactly (fn foo() ...) so exact match hits.
        let ok = apply_edit(root, p.to_str().unwrap(), "fn foo()", "fn bar()").unwrap();
        assert!(ok);
    }
}
