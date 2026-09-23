"""V77 regression tests for the q4/q9 measurement fixes.

1. `extract_bare_symbols` must not count English prose words as symbol
   claims (q4 forensic: precision 0.25 was an artifact — "appearing",
   "before", "finds" were scored as claims; all 8 real helpers verified).
2. File-citation spans (`op_table.rs:27`) are locations, not symbols —
   their basenames must not become claims.
3. q9 GT must list only in-scope (crates/reliary-search) struct names —
   the question scopes to that crate, and bare-symbol GT extraction would
   otherwise require out-of-crate names in every answer.

Each test's negative control is documented in its body: remove the guard
and the assertion fails.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from deterministic_verify import extract_bare_symbols  # noqa: E402
from reliary_judge_gt import GROUND_TRUTH  # noqa: E402


def test_prose_words_are_not_claims():
    """English prose must not enter the symbol-claim set.

    Negative control: the pre-V77 extractor (no `_`-required / internal-cap
    filters) returns {'appearing', 'before', 'finds', 'handles', 'scan_delimiters'},
    so this assertion fails.
    """
    text = (
        "It handles block nesting before scanning. The helper appearing "
        "first finds tokens: scan_delimiters."
    )
    syms = extract_bare_symbols(text)
    assert "scan_delimiters" in syms, f"real snake_case helper kept, got {syms}"
    for prose in ("appearing", "before", "finds", "handles", "block", "nesting"):
        assert prose not in syms, f"prose word {prose!r} leaked into claims"


def test_file_citations_are_not_claims():
    """Citation spans are locations — basenames must not become claims.

    Negative control: without the location-strip step, "op_table" and
    "full_file" (snake_case, not stopwords) appear in the claim set.
    """
    syms = extract_bare_symbols("OpEntry at op_table.rs:27 and FileInfo (full_file.rs:27)")
    assert "op_table" not in syms, f"citation basename leaked, got {syms}"
    assert "full_file" not in syms, f"citation basename leaked, got {syms}"
    assert "opentry" in syms, f"PascalCase symbol kept, got {syms}"
    assert "fileinfo" in syms, f"PascalCase symbol kept, got {syms}"


def test_pascal_case_requires_internal_capital():
    """Sentence-initial English capitals are not symbols.

    Negative control: the pre-V77 PASCAL filter keeps any [A-Z] word, so
    'Verified' and 'Std' would appear in the set.
    """
    syms = extract_bare_symbols("Verified impls: OpEntry and Std types")
    assert "opentry" in syms, f"real PascalCase symbol kept, got {syms}"
    assert "verified" not in syms, f"sentence-initial English leaked, got {syms}"
    assert "std" not in syms, f"abbreviation-without-internal-cap leaked, got {syms}"


def test_q9_ground_truth_is_in_scope_bare_list():
    """q9 GT: bare name list, all five structs in crates/reliary-search.

    Negative control: the pre-V77 GT text (workspace-wide, includes
    DeadConfig/MaxwellGate from other crates) fails both the bare-list
    shape check and the out-of-crate exclusion.
    """
    gt = GROUND_TRUTH["q9_consume_method_impls"]
    syms = extract_bare_symbols(gt)
    expected = {"opentry", "optable", "fileinfo", "scopetypemap", "linedelimiters"}
    assert expected <= syms, f"missing in-scope structs: {expected - syms}, got {syms}"
    # No file:line in GT → sym_level scoring path (bare-symbol matching).
    assert ".rs:" not in gt, "q9 GT must be a bare list so scoring is symbol-level"
    # Out-of-crate names must not be required of any answer.
    for out_of_scope in ("deadconfig", "maxwellgate"):
        assert out_of_scope not in syms, f"out-of-crate {out_of_scope} in GT"


def test_q9_gt_symbols_match_audit_set():
    """The five names must survive extraction with no extras.

    Negative control: adding prose around the list (e.g. "These structs
    derive Default") injects 'structs'/'derive' as claims unless they are
    filtered — this asserts the extraction is clean on the shipped text.
    """
    syms = extract_bare_symbols(GROUND_TRUTH["q9_consume_method_impls"])
    expected = {"opentry", "optable", "fileinfo", "scopetypemap", "linedelimiters"}
    assert syms == expected, f"GT must extract exactly the audit set, got {syms}"
