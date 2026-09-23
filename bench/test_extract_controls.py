#!/usr/bin/env python3
"""M3: negative controls for deterministic_verify claim extraction.

Two controls, both must fail if the extractor/verifier is broken:
1. wrong-format control — factually correct answer in an unusual citation
   format must still extract the same (symbol?, file, line) facts.
2. wrong-file control — right format, wrong file/line must NOT verify.

Run: python3 bench/test_extract_controls.py
Exit 0 = controls pass; nonzero = extractor/verifier is biased or broken.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from deterministic_verify import extract_facts, verify_facts


def _check(cond, msg):
    if not cond:
        print(f"FAIL: {msg}")
        return False
    print(f"ok: {msg}")
    return True


def main():
    ok = True

    # --- Control 1: format symmetry -----------------------------------------
    # Same factual claim, three citation styles → same core fact.
    a_style = "scan_delimiters is defined at structural.rs:603"
    b_style = "scan_delimiters — structural.rs:603"
    c_style_header = (
        "Helpers in crates/reliary-search/src/structural.rs:\n"
        "1. scan_delimiters — line 603\n"
        "2. find_top_level_eq — line 837"
    )
    fa, fb, fc = extract_facts(a_style), extract_facts(b_style), extract_facts(c_style_header)

    ok &= _check(
        ("scan_delimiters", "structural.rs", 603) in fa,
        f"A-style extracts symbol claim: {sorted(fa)}",
    )
    ok &= _check(
        ("scan_delimiters", "structural.rs", 603) in fb,
        f"B-style em-dash extracts symbol claim: {sorted(fb)}",
    )
    ok &= _check(
        ("scan_delimiters", "structural.rs", 603) in fc
        and ("find_top_level_eq", "structural.rs", 837) in fc,
        f"C-style bare lines inherit header file with symbols: {sorted(fc)}",
    )

    # Range expansion
    fr = extract_facts("compat.rs (lines 45-46, 70-71)")
    ok &= _check(
        ("", "compat.rs", 45) in fr
        and ("", "compat.rs", 46) in fr
        and ("", "compat.rs", 70) in fr
        and ("", "compat.rs", 71) in fr,
        f"line ranges expand: {sorted(fr)}",
    )

    # Methods paren form associates symbol
    fm = extract_facts("find_enclosing (brace_graph.rs:37)")
    ok &= _check(
        ("find_enclosing", "brace_graph.rs", 37) in fm,
        f"methods paren associates symbol: {sorted(fm)}",
    )

    # --- Control 2: wrong-file must not verify -------------------------------
    # Fake occurrence table: symbol exists at structural.rs:603 only.
    occ = {
        ("scan_delimiters", "structural.rs", 603),
        ("scan_delimiters", "structural.rs", 604),
        ("find_enclosing", "brace_graph.rs", 37),
    }
    correct = extract_facts("scan_delimiters is defined at structural.rs:603")
    v, f, _ = verify_facts(correct, occ, tol=1)
    ok &= _check(v == 1 and f == 0, f"correct fact verifies (v={v}, f={f})")

    wrong_file = extract_facts("scan_delimiters is defined at ingest.rs:603")
    v, f, _ = verify_facts(wrong_file, occ, tol=1)
    ok &= _check(v == 0 and f == 1, f"wrong file fails (v={v}, f={f})")

    wrong_line = extract_facts("scan_delimiters is defined at structural.rs:999")
    v, f, _ = verify_facts(wrong_line, occ, tol=1)
    ok &= _check(v == 0 and f == 1, f"wrong line fails (v={v}, f={f})")

    # File-only claim at a line with NO occurrence must fail (old "file exists" bug)
    wrong_line_only = extract_facts("structural.rs:999")
    v, f, _ = verify_facts(wrong_line_only, occ, tol=1)
    ok &= _check(v == 0 and f == 1, f"file-only wrong line fails (v={v}, f={f})")

    # File-only claim at a real occurrence line verifies
    right_line_only = extract_facts("structural.rs:603")
    v, f, _ = verify_facts(right_line_only, occ, tol=1)
    ok &= _check(v == 1 and f == 0, f"file-only right line verifies (v={v}, f={f})")

    # Symmetric recall proxy: C-style bare lines yield facts that match GT facts
    gt = extract_facts("scan_delimiters (structural.rs:603)")
    ans = extract_facts(
        "Helpers in structural.rs:\n- scan_delimiters — line 603"
    )
    # GT has symbol claim; answer has file-only from bare line + maybe symbol from dash
    # At minimum the file:line portion must overlap for recall
    gt_fl = {(b, l) for (_, b, l) in gt}
    ans_fl = {(b, l) for (_, b, l) in ans}
    ok &= _check(
        gt_fl <= ans_fl or (gt_fl & ans_fl),
        f"C-style file:line overlaps GT (gt={gt_fl}, ans={ans_fl})",
    )

    if not ok:
        print("\n1 or more controls FAILED — extractor/verifier not fair.")
        return 1
    print("\nAll controls passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
