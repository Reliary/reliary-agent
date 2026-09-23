"""V78 Phase 1 negative controls for the broadened claim extractor.

Each control documents what fails if the corresponding guard is removed:
1. Hyphen-path words must not become symbols (`reliary-search` → `reliary`).
2. file:line attaches a symbol only from the immediate 24-char window —
   a sticky `current` from an unrelated earlier match produces phantoms.
3. File-only claims are locations, not symbol-exists claims.
4. Prose forms (`file.rs at line N`) extract; English words do not.
5. Wrong-file claims extract with the wrong file (verify must penalize).
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from deterministic_verify import extract_facts  # noqa: E402


def test_hyphen_path_is_not_a_symbol():
    """`reliary-search/src/...` must not yield symbol `reliary`.

    Negative control: the pre-V78 SYM_DASH_FILE_LINE group treats the
    hyphen-path stem as a symbol, so `reliary at structural.rs:16` appears
    as a claim — a phantom that can never verify (no occurrence row).
    Real symbols precede the citation (sym at path), not follow it.
    """
    facts = extract_facts(
        "classify_structural at reliary-search/src/structural.rs:16"
    )
    syms = {s for s, _, _ in facts if s}
    assert "reliary" not in syms, f"hyphen-path stem leaked as symbol: {facts}"
    assert ("classify_structural", "structural.rs", 16) in facts, facts


def test_file_line_does_not_inherit_sticky_symbol():
    """file:line only attaches a symbol from the immediate window.

    Negative control: a sticky `current` from an earlier unrelated match
    (e.g. a code fence symbol 100+ chars away) attaches to every later
    file:line, producing `wrong_symbol at some.rs:1`.
    """
    text = "```classify_structural\n...\n...\nAlso see usage in ingest.rs:181"
    facts = extract_facts(text)
    # ingest.rs:181 has no adjacent symbol → must extract as file-only
    assert ("", "ingest.rs", 181) in facts or not any(
        b == "ingest.rs" and ln == 181 for _, b, ln in facts if _
    ), f"phantom sticky symbol attached: {facts}"


def test_file_only_claim_has_empty_symbol():
    """A bare `file.rs:181` claim is a location, not a symbol claim.

    Negative control: treating the basename as a symbol invents
    `ingest`/`structural` as symbol-exists claims.
    """
    facts = extract_facts("See ingest.rs:181 for the call")
    file_claims = [(b, ln) for s, b, ln in facts if not s]
    assert ("ingest.rs", 181) in file_claims, f"file-only claim missing: {facts}"
    # basename must not become a symbol claim
    assert "ingest" not in {s for s, _, _ in facts if s}, f"basename leaked: {facts}"


def test_prose_line_form_extracts():
    """`structural.rs at line 16` must extract as a location claim.

    Negative control: without LINE_IN_FILE, grep answers written in prose
    score zero — the pre-V78 format bias.
    """
    facts = extract_facts("The helper lives in structural.rs at line 16")
    assert ("structural.rs", 16) in {(b, ln) for _, b, ln in facts}, facts


def test_wrong_file_claims_with_wrong_file():
    """A correct symbol at the wrong file extracts as (sym, wrong_file, line).

    Negative control: if the extractor silently dropped mismatches or
    remapped to the GT file, wrong answers would score as verified.
    """
    facts = extract_facts("classify_structural is defined at ingest.rs:181")
    assert ("classify_structural", "ingest.rs", 181) in facts, facts


def _enclosing(tmp_path):
    """Fixture: fn outer (call at line 3), fn other (call at line 7)."""
    src = tmp_path / "sample.rs"
    src.write_text(
        "// header\n"
        "pub fn outer() {\n"
        "    do_thing();\n"
        "    do_other();\n"
        "}\n"
        "\n"
        "pub fn other() {\n"
        "    do_thing();\n"
        "}\n"
    )
    return str(tmp_path)


def test_enclosing_function_claim_verifies(tmp_path):
    """`outer at sample.rs:3` verifies because line 3 is inside fn outer.

    Negative control: without the enclosing-function rule, every caller
    citation whose symbol is the enclosing fn (not the callee on the call
    line) is scored false — the q2 measurement bug.
    """
    from deterministic_verify import _is_enclosing_function
    corpus = _enclosing(tmp_path)
    assert _is_enclosing_function(corpus, "sample.rs", 3, "outer", {})
    assert _is_enclosing_function(corpus, "sample.rs", 8, "other", {})


def test_non_enclosing_function_claim_fails(tmp_path):
    """`other at sample.rs:3` fails — line 3 is inside outer, not other.

    Negative control: a loose rule that accepts any function in the file
    would mark wrong-caller citations verified and inflate precision.
    """
    from deterministic_verify import _is_enclosing_function
    corpus = _enclosing(tmp_path)
    assert not _is_enclosing_function(corpus, "sample.rs", 3, "other", {})
    assert not _is_enclosing_function(corpus, "sample.rs", 8, "outer", {})


def test_enclosing_function_requires_call_site(tmp_path):
    """`outer at sample.rs:5` fails — line 5 is `}` (not a call site).

    Negative control: accepting any line inside the function would let
    "fn outer — sample.rs:any" verify without evidence of a call.
    """
    from deterministic_verify import _is_enclosing_function
    corpus = _enclosing(tmp_path)
    assert not _is_enclosing_function(corpus, "sample.rs", 5, "outer", {})


def test_english_prose_does_not_become_symbol():
    """English words between citations must not become claims.

    Negative control: the pre-V77 bare-symbol extractor counted
    'appearing'/'before'/'finds' as claims.
    """
    facts = extract_facts("appearing before finds in structural.rs:16")
    syms = {s for s, _, _ in facts if s}
    for prose in ("appearing", "before", "finds"):
        assert prose not in syms, f"{prose!r} leaked: {facts}"


def test_adjacent_symbol_attaches():
    """`classify_structural at file.rs:314` attaches the real symbol.

    Negative control: if the adjacency window shrinks to 0 or the symbol
    filter drops snake_case names, precision-recall both collapse on
    correctly formatted answers.
    """
    facts = extract_facts("classify_structural at structural.rs:314")
    assert ("classify_structural", "structural.rs", 314) in facts, facts


def test_capitalized_english_word_is_not_a_symbol():
    """Sentence-initial `Struct defined at file.rs:16` must not claim `Struct`.

    Negative control: the stop-word check was case-sensitive, so a
    capitalized prose word at a sentence start became a symbol claim that
    no answer could match (q1 recall 0.75 instead of 1.00).
    """
    facts = extract_facts("Struct defined at structural.rs:16")
    assert "Struct" not in {s for s, _, _ in facts if s}, facts
    assert ("", "structural.rs", 16) in facts, facts


def test_grouped_callers_inherit_header_file():
    """`test_foo (989), test_bar (1024)` under a file header claims both.

    Negative control: this is the exact form A/C emit for many callers in
    one file; without it, correct grouped citations score as misses.
    """
    text = (
        "Callers in crates/reliary-search/src/structural.rs:\n"
        "- test_rust_fn_def (989), test_python_def (1024)\n"
    )
    facts = extract_facts(text)
    assert ("test_rust_fn_def", "structural.rs", 989) in facts, facts
    assert ("test_python_def", "structural.rs", 1024) in facts, facts


def test_grouped_caller_does_not_invent_symbols():
    """`file.rs (line 5)` must not turn the basename into a symbol claim.

    Negative control: a loose grouped rule would claim `file` as a symbol
    on every `name (file.rs:12)` mention.
    """
    facts = extract_facts("see structural.rs (line 5) for context")
    syms = {s for s, _, _ in facts if s}
    assert "structural" not in syms, f"basename leaked as symbol: {facts}"


def test_multi_citation_line_credits_subject_symbol():
    """`X is called from a.rs:1, b.rs:2` credits X at both sites.

    Negative control: without this, a correct caller list that states the
    symbol once and then bare locations scores recall 0 — understating the
    tool that produced it.
    """
    text = "write_vocab is called from convert.py:936, convert.py:9251"
    facts = extract_facts(text)
    assert ("write_vocab", "convert.py", 936) in facts, facts
    assert ("write_vocab", "convert.py", 9251) in facts, facts


def test_single_citation_prose_is_still_file_only():
    """`See ingest.rs:181` (one citation) must stay a file-only claim.

    Negative control: the multi-citation rule must not fire on a single
    prose citation, or `See` would become a symbol.
    """
    facts = extract_facts("See ingest.rs:181 for the call")
    assert ("ingest.rs", 181) in {(b, l) for _, b, l in facts}, facts
    assert "See" not in {s for s, _, _ in facts if s}, facts
