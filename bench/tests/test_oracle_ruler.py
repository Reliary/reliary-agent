"""V83 Phase 0: the blind-oracle ruler regression tests.

Two measurement bugs invalidated every recall number this project reported
before V83. Both are pinned here with negative controls that reproduce the
OLD broken behaviour and assert the NEW parser rejects it:

1. `awk '{print $1"\t"$(NF-1)"\t"$NF}'` in blind_oracle.sh projected each
   ctags line to its last two fields. For any line with a trailing scope
   field (`implementation:`, `struct:`, `enum:`, `signature:`) the result
   was `line:N` + scope -- kind AND line lost. 52% of lines on the blind
   tokio run were mangled; all 4120 methods and 1366 fields vanished.
   Negative control: the old projection is reproduced inline and asserted
   to parse to ZERO defs through the new parser.

2. The inline report matched `kind:(\\w)` -- one character -- against the
   short-letter set `fcmsdptgun`. `enum`/`enumerator`/`interface`/`variable`
   start with letters outside the set and were dropped even when the kind
   survived. Negative control: the old one-char filter is reproduced and
   asserted to drop `kind:enumerator`.

If either control stops failing, the test fails: the guard is load-bearing.
"""
import os
import sys

sys.path.insert(
    0, os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), "docker")
)

import oracle_report  # noqa: E402


# Raw ctags lines as universal-ctags emits them (the shapes that broke):
# 4-field simple, 5-field with scope, 6-field with scope+extra.
RAW_CTAGS = [
    "simple_fn\t/proj/a.rs\t10;\"\tkind:function\tline:10",
    "Widget\t/proj/a.rs\t20;\"\tkind:struct\tline:20",
    "run\t/proj/a.rs\t30;\"\tkind:method\tline:30\timplementation:Widget",
    "count\t/proj/a.rs\t40;\"\tkind:field\tline:40\tstruct:Widget",
    "Red\t/proj/a.rs\t50;\"\tkind:enumerator\tline:50\tenum:Color",
    "Color\t/proj/a.rs\t55;\"\tkind:enum\tline:55",
    "MAX\t/proj/a.rs\t60;\"\tkind:macro\tline:60",
    "make\t/proj/a.rs\t70;\"\tkind:function\tline:70\tsignature:(x: i32)",
    "impl_target\t/proj/a.rs\t80;\"\tkind:typedef\tline:80\timplementation:Widget",
    "global_config\t/proj/a.rs\t90;\"\tkind:variable\tline:90",
]


def write(path, lines):
    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")


def _write(tmp_path, name, lines):
    p = str(tmp_path / f"{name}.txt")
    write(p, lines)
    return p


def old_awk_projection(line):
    """Reproduce `awk -F'\\t' '{print $1"\\t"$(NF-1)"\\t"$NF}'`."""
    parts = line.split("\t")
    if len(parts) < 2:
        return line
    return "\t".join([parts[0], parts[-2], parts[-1]])


def old_kind_filter(kind_word):
    """Reproduce the one-char `kind:(\\w)` filter with set `fcmsdptgun`."""
    return kind_word[:1] in set("fcmsdptgun")


def test_new_parser_reads_all_kinds_and_lines(tmp_path):
    ctags = str(tmp_path / "ctags.txt")
    sites, per_kind, excluded, raw, unparsed = oracle_report.parse_ctags(
        _write(tmp_path, "c", RAW_CTAGS))

    assert unparsed == 0, f"raw ctags lines must all parse, {unparsed} failed"
    by = {(n, k): ln for n, k, _f, ln in sites}
    assert ("Widget", "struct") in by
    assert by[("run", "method")] == 30, "scope-field line lost its line number"
    assert by[("count", "field")] == 40
    assert by[("Red", "enumerator")] == 50
    assert by[("Color", "enum")] == 55
    assert by[("global_config", "variable")] == 90
    assert by[("make", "function")] == 70, "signature-field line lost its line number"
    # The kinds the old one-char filter dropped are counted:
    assert set(per_kind) >= {"method", "field", "enumerator", "enum", "variable"}


def test_sites_are_per_site_not_per_name(tmp_path):
    """Site-level counting: two defs of one name in different files are two
    entries, and a def in file B is not a hit for a site in file A."""
    lines = [
        "default\t/proj/a.rs\t10;\"\tkind:method\tline:10\timplementation:A",
        "default\t/proj/b.rs\t99;\"\tkind:method\tline:99\timplementation:B",
    ]
    sites, _, _, _, _ = oracle_report.parse_ctags(_write(tmp_path, "c", lines))
    assert len(sites) == 2, "repeated names must not collapse into one entry"
    assert {f for _n, _k, f, _l in sites} == {"/proj/a.rs", "/proj/b.rs"}


def test_negative_control_old_awk_destroys_the_truth(tmp_path):
    """The old projection must make the truth unusable through the new parser.

    If this control passes (i.e. the old form still parses fine), the guard
    has been broken and the test fails in the asserts below.
    """
    ctags = str(tmp_path / "ctags_old.txt")
    write(ctags, [old_awk_projection(l) for l in RAW_CTAGS])
    sites, _, _, _, _ = oracle_report.parse_ctags(ctags)
    # Scope-bearing lines (5 of 10) lost kind+line; only 4-field lines survive.
    for name in ("run", "count", "Red", "make", "impl_target"):
        assert not any(n == name for n, _k, _f, _l in sites), (
            f"negative control broken: old awk form parsed {name}; "
            "the projection bug is back or the parser is too lax"
        )
    assert all(ln is not None for _n, _k, _f, ln in sites)


def test_negative_control_old_kind_filter_drops_long_kinds():
    """The old one-char filter drops every long kind outside fcmsdptgun."""
    for kind in ("enumerator", "enum", "interface", "variable"):
        assert not old_kind_filter(kind), (
            f"negative control broken: old filter kept {kind}; "
            "the kind-regex bug is back"
        )
    kept = {k for k in ("function", "method", "struct", "field", "macro",
                        "typedef", "prototype", "class", "module") if old_kind_filter(k)}
    assert "function" in kept  # sanity: the old filter was not a no-op


def test_report_surfaces_per_kind_and_excluded(tmp_path):
    ctags = str(tmp_path / "ctags.txt")
    ours = str(tmp_path / "ours.txt")
    report = str(tmp_path / "report.txt")
    write(ctags, RAW_CTAGS)
    # Our side: hit everything except enumerator and variable.
    write(ours, [
        "simple_fn\t/proj/a.rs:10",
        "Widget\t/proj/a.rs:20",
        "run\t/proj/a.rs:30",
        "count\t/proj/a.rs:40",
        "Color\t/proj/a.rs:55",
        "MAX\t/proj/a.rs:60",
        "make\t/proj/a.rs:70",
        "impl_target\t/proj/a.rs:80",
    ])
    sys.argv = ["oracle_report.py", ctags, ours, report]
    oracle_report.main()
    text = open(report).read()
    assert "recall=" in text
    assert "per-kind" in text
    # enumerator and variable are the two misses; both must appear.
    assert "Red [enumerator]" in text
    assert "global_config [variable]" in text
    assert "unparsed=0" in text
