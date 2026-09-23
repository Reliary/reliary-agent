#!/usr/bin/env python3
"""Deterministically obfuscate every non-reserved identifier in a source tree.

Purpose: test whether reliary's comprehension advantage is a *familiarity*
effect. Take a public repo the model has memorized (tokio), rename every
user identifier to a stable pseudonym, and re-run the identical bench. If
the advantage is familiarity-driven, the original corpus should favour grep
(the model overrides tool output with priors) while the obfuscated corpus
should favour reliary.

Grammar-free: a single `[A-Za-z_][A-Za-z0-9_]*` token scan over raw text,
with a reserved set of language keywords + primitives so that Rust structure
(`impl X {`, `pub fn`, `#[derive(Default)]`) survives — that structure is what
the question generator and the grammar-free index rely on. Everything else
(type names, function names, std types, module names) is renamed, including
occurrences inside comments and string literals.

Deterministic: identifiers are sorted and each maps to a fixed pseudonym
derived from SHA-256(ident + salt), so the same input always yields the same
output tree (byte-identical), and a *different* salt yields a different but
equally valid tree.

Usage:
  python3 bench/obfuscate_corpus.py SRC DST [--salt tokio-v1] [--ext .rs ...]
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import sys

# Language keywords / primitives the *structure* depends on. Renaming these
# would destroy the grammar-free detection's ability to classify definitions,
# so they are preserved. Everything here is a language token, not a
# user-chosen name — preserving them does not leak corpus identity.
RESERVED = {
    # declarations / control flow
    "fn", "pub", "struct", "enum", "union", "impl", "trait", "type", "mod",
    "use", "crate", "super", "self", "Self", "as", "in", "if", "else",
    "match", "loop", "while", "for", "return", "let", "mut", "const",
    "static", "ref", "move", "dyn", "where", "async", "await", "unsafe",
    "extern", "break", "continue", "box", "yield", "do", "default",
    "true", "false", "main", "derive", "cfg", "allow", "warn", "deny",
    "macro", "macro_rules", "test", "bench", "no_std", "no_mangle",
    # primitives
    "u8", "u16", "u32", "u64", "u128", "usize",
    "i8", "i16", "i32", "i64", "i128", "isize",
    "f32", "f64", "bool", "char", "str", "String",
    # generator anchors
    "Default", "Option", "Some", "None", "Result", "Ok", "Err",
}

IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
BASE36 = "0123456789abcdefghijklmnopqrstuvwxyz"


def pseudonym(ident: str, salt: str, used: set) -> str:
    """Fixed, collision-free pseudonym that preserves the identifier's *shape*.

    Deterministic in (ident, salt). Two signals survive obfuscation so the
    grammar-free generator and detector see the same structure:
      - case class: leading `Z` if the original starts uppercase, else `z`
        (keeps the PascalCase type signal and the `^[A-Z]` generator test)
      - underscore segmentation: one pseudonym segment per `_`-separated part
        (keeps the snake_case function signal the q1 generator prefers)
    Only lexical identity is destroyed. A numeric suffix disambiguates the
    (astronomically unlikely) hash collision.
    """
    digest = hashlib.sha256(f"{salt}\x00{ident}".encode()).digest()
    n = int.from_bytes(digest[:16], "big")
    parts = ident.split("_")
    segs = []
    for k, part in enumerate(parts):
        head = "Z" if part[:1].isupper() else "z"
        n = int.from_bytes(
            hashlib.sha256(f"{salt}\x01{ident}\x01{k}".encode()).digest()[:8], "big")
        seg = [head]
        for _ in range(6):
            seg.append(BASE36[n % 36])
            n //= 36
        segs.append("".join(seg))
    name = "_".join(segs)
    k = 0
    while name in used:
        k += 1
        name = f"{name}_{k}"
    used.add(name)
    return name


def obfuscate_text(text: str, mapping: dict, salt: str, used: set,
                   preserved: set = None, invent: bool = True) -> str:
    """Rewrite identifiers using `mapping`.

    `preserved` tokens (keywords, directory names) are never rewritten.
    `invent=False` (prose/markup files) only rewrites identifiers already known
    from code — it never mints a pseudonym for an English word.
    """
    keep = RESERVED if preserved is None else preserved

    def repl(m):
        tok = m.group(0)
        if tok in keep:
            return tok
        name = mapping.get(tok)
        if name is None:
            if not invent:
                return tok
            name = pseudonym(tok, salt, used)
            mapping[tok] = name
        return name

    return IDENT.sub(repl, text)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("src")
    ap.add_argument("dst")
    ap.add_argument("--salt", default="rel8-obfuscate-v1")
    ap.add_argument("--ext", nargs="*", default=None,
                    help="only rewrite these extensions (default: all text files)")
    ap.add_argument("--map-out", default=None,
                    help="write the identifier + file mapping as JSON (for "
                         "translating questions across corpora)")
    args = ap.parse_args()

    if os.path.exists(args.dst):
        print(f"refusing to overwrite existing {args.dst}", file=sys.stderr)
        return 2

    mapping: dict = {}
    used: set = set()
    file_map: dict = {}
    n_files = 0
    n_bytes = 0

    # Directory names are preserved (only file *stems* are renamed). They must
    # therefore never enter the identifier map, or question prose that embeds a
    # path (`tokio/src/sync/`) would be corrupted by identifier substitution.
    dir_names = set()
    for root, dirs, _files in os.walk(args.src):
        dirs[:] = [d for d in dirs if d not in (".git", ".reliary", "target", "node_modules")]
        for d in dirs:
            dir_names.add(d)
    preserved = RESERVED | dir_names

    # `--ext` selects which files are rewritten at all; `CODE_EXT` (fixed)
    # decides which may MINT new pseudonyms. Prose/markup files (.md) are
    # rewritten with known identifiers only, so an English word that merely
    # looks like an identifier ("Where", "List") is never renamed.
    CODE_EXT = {".rs", ".c", ".h", ".cc", ".cpp", ".hpp", ".go", ".py",
                ".toml", ".json", ".yaml", ".yml", ".lock", ".cfg"}
    selected = set(args.ext) if args.ext else None

    def process(fn):
        return selected is None or os.path.splitext(fn)[1] in selected

    def invent_for(fn):
        return os.path.splitext(fn)[1] in CODE_EXT

    def walk_files():
        for root, dirs, files in os.walk(args.src):
            dirs[:] = [d for d in dirs if d not in
                       (".git", ".reliary", "target", "node_modules")]
            for fn in files:
                yield root, fn

    # Pass 1: learn every identifier from the code files first, so a prose file
    # encountered early in the walk can still be rewritten consistently.
    for root, fn in walk_files():
        if not process(fn) or not invent_for(fn):
            continue
        try:
            with open(os.path.join(root, fn), encoding="utf-8") as fh:
                obfuscate_text(fh.read(), mapping, args.salt, used, preserved, True)
        except (UnicodeDecodeError, OSError):
            continue

    # Pass 2: write every file with the complete mapping.
    for root, fn in walk_files():
        sp = os.path.join(root, fn)
        rel = os.path.relpath(root, args.src)
        out_root = os.path.join(args.dst, rel) if rel != "." else args.dst
        os.makedirs(out_root, exist_ok=True)
        stem, ext = os.path.splitext(fn)
        if process(fn):
            # Seed the file pseudonym with the FULL relative path, not just the
            # stem: `mod.rs`/`lib.rs`/`main.rs` exist in dozens of directories,
            # and stem-only seeding collapsed them all onto one base name with
            # numeric suffixes (a 25-way collision in tokio), destroying the
            # agent's ability to distinguish files.
            rel_stem = os.path.join(rel, stem) if rel != "." else stem
            new_fn = pseudonym("file:" + rel_stem, args.salt, used) + ext
        else:
            new_fn = fn
        rel_orig = os.path.join(rel, fn) if rel != "." else fn
        rel_new = os.path.join(rel, new_fn) if rel != "." else new_fn
        file_map[rel_orig] = rel_new
        if not process(fn):
            shutil.copy2(sp, os.path.join(out_root, new_fn))
            continue
        try:
            with open(sp, encoding="utf-8") as fh:
                text = fh.read()
        except (UnicodeDecodeError, OSError):
            shutil.copy2(sp, os.path.join(out_root, new_fn))
            continue
        out = obfuscate_text(text, mapping, args.salt, used,
                             preserved, invent_for(fn))
        with open(os.path.join(out_root, new_fn), "w", encoding="utf-8") as fh:
            fh.write(out)
        n_files += 1
        n_bytes += len(out)

    if args.map_out:
        with open(args.map_out, "w") as fh:
            json.dump({"idents": mapping, "files": file_map, "salt": args.salt}, fh)
        print(f"mapping -> {args.map_out}")

    print(f"obfuscated {n_files} files, {n_bytes} bytes, "
          f"{len(mapping)} distinct identifiers -> {args.dst}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
