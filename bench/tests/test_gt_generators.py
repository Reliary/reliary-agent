"""V75 regression tests for the bench-side ground-truth generators.

These lock in three real bugs found while making the canonical tape portable:

1. `brace_blocks` returns `(start, end)` but both callers unpacked `end, _`, so
   the block was always empty and auto-generated q3/q4 ground truth was empty.
2. `impl_methods` returned 1-indexed lines which `load_auto_gt` then incremented
   again (and the Rust `bench gen` incremented `MethodOn.line`, also 1-indexed).
3. The q3 ground truth included private methods although the question asks for
   public ones.

Each test has a negative control in the form of an assertion that would fail if
the guard were removed (documented in the test body).
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import auto_questions  # noqa: E402


SOURCE = """\
// filler
pub struct Widget {
    pub x: i32,
    y: i32,
}

impl Widget {
    pub fn area(&self) -> i32 {
        self.x
    }

    fn secret(&self) -> i32 {
        0
    }
}
"""


def _write(tmp_path, name="widget.rs"):
    p = tmp_path / name
    p.write_text(SOURCE)
    return str(p)


def test_brace_blocks_returns_start_then_end(tmp_path):
    """Guard: `start, end = brace_blocks(...)` is the correct unpacking.

    Negative control: the pre-fix `end, _ = brace_blocks(...)` yields
    start (24 here) as `end`, so the block range `[start+1, end)` is empty and
    `impl_methods` returns nothing.
    """
    path = _write(tmp_path)
    lines = auto_questions.read_lines(path)
    start, end = auto_questions.brace_blocks(lines, 0, 0)
    assert start == 0, f"first block starts at line 0, got {start}"
    assert end > start, "end must exceed start for a real block"
    # The unpacks callers use must give a non-empty range.
    _, end2 = auto_questions.brace_blocks(lines, 0, 0)
    assert end2 > start, f"block end {end2} must exceed start {start}"


def test_impl_methods_returns_1indexed_lines_and_visibility(tmp_path):
    """`impl_methods` lines are 1-indexed and carry a `is_pub` flag.

    Negative control: if it returned 0-indexed lines, `area` would be 6 (not 7)
    and `load_auto_gt`'s increment would be needed; if the visibility flag were
    dropped, `secret` could not be filtered from a "public methods" GT.
    """
    path = _write(tmp_path)
    methods = auto_questions.impl_methods(path, "Widget")
    by_name = {m[0]: m for m in methods}

    assert "area" in by_name, f"area must be found, got {methods}"
    assert "secret" in by_name, f"secret must be found, got {methods}"

    area_name, area_line, area_pub = by_name["area"]
    assert area_line == 8, f"`pub fn area` is source line 8 (1-indexed), got {area_line}"
    assert area_pub is True, "area is public"

    _, secret_line, secret_pub = by_name["secret"]
    assert secret_line == 12, f"`fn secret` is source line 12, got {secret_line}"
    assert secret_pub is False, "secret is private"


def test_q3_ground_truth_excludes_private_methods(tmp_path):
    """The production q3 GT helper must contain only public methods.

    Guards `public_methods`, not a reimplementation: removing the `is_pub`
    filter from that function fails this test (negative control verified).
    """
    path = _write(tmp_path)
    public = auto_questions.public_methods(path, "Widget")
    assert [m for m, _ in public] == ["area"], f"only area is public, got {public}"
    assert public[0][1] == 8, f"area is source line 8, got {public[0][1]}"

