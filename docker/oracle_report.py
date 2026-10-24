#!/usr/bin/env python3
"""Blind-oracle report: ctags defs vs reliary occurrence is_def=1 rows.

Parses RAW ctags output. Every tab-separated extension field is scanned for
`kind:<word>` and `line:<n>` (with the numeric address field as fallback).
Two earlier stages silently corrupted the truth and were removed:

  * `awk '{print $1"\\t"$(NF-1)"\\t"$NF}'` kept the last two fields of each
    ctags line; whenever a scope/signature field trailed (methods carry
    `implementation:`, fields carry `struct:`, variants carry `enum:`),
    that projection emitted `line:N` plus the scope and lost BOTH kind and
    line. 52% of lines on the blind tokio run were mangled this way.
  * the inline kind filter matched `kind:(\\w)` -- ONE character -- against a
    set of short letters, dropping every long kind name whose first letter
    was outside the set: enum, enumerator, interface, variable.

Usage: oracle_report.py <ctags_defs> <our_defs> <report_out>
"""
import re
import sys

# Long kind names that count as definitions for recall purposes.
ALLOWED_KINDS = {
    "class",
    "define",
    "enum",
    "enumerator",
    "field",
    "function",
    "interface",
    "macro",
    "method",
    "module",
    "namespace",
    "prototype",
    "struct",
    "typedef",
    "union",
    "variable",
}

_KIND_RE = re.compile(r"^kind:(\w+)$")
_LINE_RE = re.compile(r"^line:(\d+)$")
_ADDR_RE = re.compile(r"^(\d+);")

# Known-gap names carried over from the old protocol: shapes deliberately
# not indexed (function pointers, operator overloads, bare #if ...). The old
# report silently removed them from the recall denominator; we now report
# both raw recall and gap-excluded recall so the carve-out is visible.
GAPS = ("fp_handler", "signal_cb", "operator", "BARE_IF", "use_proto", "extra_fn")


def parse_ctags(path):
    """Return (sites, per_kind, excluded, raw_lines, unparsed).

    sites: [(name, kind, file, line), ...] limited to ALLOWED_KINDS, one
    entry PER DEF SITE. The old report keyed on (name, kind) pairs, which
    collapsed repeated names and cross-file defs into single entries and
    inflated recall; file+line now identify a site.
    per_kind: {kind: count} (hits filled by the caller).
    excluded: {kind: count} for parsed-but-not-counted kinds.
    """
    sites = []
    per_kind = {}
    excluded = {}
    raw_lines = 0
    unparsed = 0
    for line in open(path, errors="replace"):
        raw_lines += 1
        parts = line.rstrip("\n").split("\t")
        if len(parts) < 2:
            unparsed += 1
            continue
        name = parts[0]
        fpath = parts[1]
        kind = None
        ln = None
        for p in parts[2:]:
            m = _KIND_RE.match(p)
            if m:
                kind = m.group(1)
            m = _LINE_RE.match(p)
            if m:
                ln = int(m.group(1))
        if ln is None and len(parts) >= 3:
            m = _ADDR_RE.match(parts[2])
            if m:
                ln = int(m.group(1))
        if not name or kind is None or ln is None:
            unparsed += 1
            continue
        name = _normalize_name(name)
        if not name:
            unparsed += 1
            continue
        if kind in ALLOWED_KINDS:
            per_kind.setdefault(kind, [0, 0])
            per_kind[kind][0] += 1
            sites.append((name, kind, fpath, ln))
        else:
            excluded[kind] = excluded.get(kind, 0) + 1
    return sites, per_kind, excluded, raw_lines, unparsed


def parse_ours(path):
    """phrase -> {file: [lines]} (our_defs rows are phrase\\tfile:line)."""
    ours = {}
    for line in open(path, errors="replace"):
        parts = line.rstrip("\n").split("\t")
        if len(parts) < 2:
            continue
        phrase, loc = parts[0], parts[1]
        m = re.search(r"^(.*):(\d+)$", loc)
        if not m:
            continue
        ours.setdefault(phrase.lower(), {}).setdefault(m.group(1), []).append(
            int(m.group(2)))
    return ours


def stem_proxy(name):
    """Cheap porter stand-in: the name plus suffix-stripped variants.
    Identical list to the forensics tool so both report one number."""
    out = {name}
    for suf in ("ization", "ational", "fulness", "iveness", "tional",
                "lessli", "ation", "alism", "aliti", "iviti", "fulli",
                "enci", "anci", "abli", "izer", "ator", "alli", "ingly",
                "edly", "ies", "ied", "ing", "ed", "er", "es", "s"):
        if name.endswith(suf) and len(name) - len(suf) >= 3:
            out.add(name[: -len(suf)])
            if suf in ("ies", "ied"):
                out.add(name[:-3] + "i")
    return out


def stem_pair(a, b):
    """True when a and b plausibly share a porter stem (stored phrases are
    stemmed; ctags names are surface forms). Kept identical to the
    forensics tool's `is_stem_pair` so both report the same number."""
    if a == b:
        return True
    if not a or not b or a[0] != b[0] or abs(len(a) - len(b)) > 4:
        return False
    if a.startswith(b) or b.startswith(a):
        return True
    return bool(stem_proxy(a) & stem_proxy(b))


def _normalize_name(name):
    """Strip leading identifier sigils ctags keeps but our tokenizer drops.

    TypeScript/CSS allow `$name`; our scanner splits on non-alphanumeric
    bytes, so the index stores `name`. Both sides must compare the same
    token for a definition to be found — this is a measurement
    normalization, not a scoring rule: a user querying `$ZSF` tokenizes to
    `ZSF` as well, so the tool is self-consistent."""
    return name.lstrip("$@#")


def main():
    ctags_path, ours_path, report_path = sys.argv[1:4]
    sites, per_kind, excluded, raw_lines, unparsed = parse_ctags(ctags_path)
    ours = parse_ours(ours_path)
    # Reverse index for the stem-tolerant tier: first 4 chars -> phrases.
    by_prefix = {}
    for p, files in ours.items():
        by_prefix.setdefault(p[:4], []).append((p, files))
    hit = 0
    hit_stem = 0
    miss = []
    gapped = 0
    for name, kind, fpath, ln in sites:
        if any(g in name for g in GAPS):
            gapped += 1
            continue
        lower = name.lower()
        # occurrence lines are 0-based; ctags lines are 1-based.
        our_line = ln - 1
        def near(lines):
            return any(abs(o - our_line) <= 1 for o in lines)
        if near(ours.get(lower, {}).get(fpath, [])):
            hit += 1
            per_kind[kind][1] += 1
            continue
        # Stem-tolerant tier (reported separately; strict stays comparable).
        # Our phrases are porter-stemmed (`drained` -> `drain`), so a stored
        # variant at the same file+line is still a found def site.
        if any(stem_pair(lower, p) and near(files.get(fpath, []))
               for p, files in by_prefix.get(lower[:4], [])):
            hit_stem += 1
            continue
        miss.append(f"{name} [{kind}] {fpath}:{ln}")
    total_defs = gapped + hit + hit_stem + len(miss)
    strict = hit / total_defs if total_defs else 1.0
    tolerant = (hit + hit_stem) / total_defs if total_defs else 1.0
    lines = [
        f"ctags_defs={total_defs} gapped={gapped} "
        f"ours_defs={sum(len(ls) for fs in ours.values() for ls in fs.values())}",
        f"hit={hit} miss={len(miss)} recall={strict:.3f}",
        f"stem_tolerant_hits={hit_stem} stem_tolerant_recall={tolerant:.3f}",
        f"ctags_raw_lines={raw_lines} unparsed={unparsed}",
        "per-kind (count hit miss_rate):",
    ]
    for kind in sorted(per_kind):
        count, hits = per_kind[kind]
        mr = (count - hits) / count if count else 0.0
        lines.append(f"  {kind:12} {count:6} {hits:6} {mr:.3f}")
    if excluded:
        lines.append("excluded kinds (parsed, not counted as defs):")
        for kind in sorted(excluded):
            lines.append(f"  {kind:12} {excluded[kind]:6}")
    for m in sorted(miss)[:200]:
        lines.append(f"MISS {m}")
    text = "\n".join(lines) + "\n"
    with open(report_path, "w") as f:
        f.write(text)
    print("\n".join(lines[: 4 + 1 + len(per_kind)]))
    if excluded:
        print("excluded:", ", ".join(f"{k}={v}" for k, v in sorted(excluded.items())))


if __name__ == "__main__":
    main()
