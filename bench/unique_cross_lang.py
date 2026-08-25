"""
Track C2: Cross-language grammar-free indexing.

Tests that reliary indexes and finds symbols across Python, Rust, JS.
ALTBACKEND requires tree-sitter per language — reliary is universal.

Design:
- Create a minimal mixed corpus: 3 files (Python def, Rust fn, JS function)
- Index with reliary
- Task: "find all implementations of foo" across the corpus
- Pass gate: find_references('foo') returns 3+ hits spanning all 3 languages
"""
import json
import os
import tempfile
import subprocess
from pathlib import Path

CORPUS = """# Python: def foo()
def foo(x, y):
    return x + y

def bar():
    return foo(1, 2)

class Foo:
    def foo(self):
        return "python foo"
"""

CORPUS_RUST = """// Rust: fn foo()
fn foo(x: i32, y: i32) -> i32 {
    x + y
}

fn bar() -> i32 {
    foo(1, 2)
}

struct Foo;
impl Foo {
    fn foo(&self) -> &'static str {
        "rust foo"
    }
}
"""

CORPUS_JS = """// JavaScript: function foo()
function foo(x, y) {
    return x + y;
}

function bar() {
    return foo(1, 2);
}

class Foo {
    foo() {
        return "js foo";
    }
}
"""


def setup_corpus():
    """Create temp corpus with 3 language files."""
    tmpdir = tempfile.mkdtemp(prefix="reliary_xlang_")
    Path(tmpdir, "foo.py").write_text(CORPUS)
    Path(tmpdir, "foo.rs").write_text(CORPUS_RUST)
    Path(tmpdir, "foo.js").write_text(CORPUS_JS)
    return tmpdir


def index_corpus(corpus_path, reliary_bin):
    """Run reliary index on the corpus."""
    subprocess.run(
        [reliary_bin, "trust", corpus_path],
        capture_output=True, timeout=120,
    )


def find_foo(corpus_path, reliary_bin):
    """Find references to foo across the corpus."""
    proc = subprocess.run(
        [reliary_bin, "search", "foo", corpus_path],
        capture_output=True, text=True, timeout=30,
    )
    return proc.stdout


def main():
    reliary_bin = os.environ.get(
        "RELIARY_BIN", "/home/user/src/reliary8/target/release/reliary"
    )

    print("=== Track C2: Cross-language grammar-free indexing ===")
    tmpdir = setup_corpus()
    print(f"Corpus: {tmpdir}")
    print(f"Files: foo.py, foo.rs, foo.js")

    print("\nIndexing...")
    index_corpus(tmpdir, reliary_bin)

    print("\nSearching for 'foo'...")
    result = find_foo(tmpdir, reliary_bin)
    print(result)

    # Count hits per language
    py_hits = result.count(".py")
    rs_hits = result.count(".rs")
    js_hits = result.count(".js")

    print(f"\nHits: Python={py_hits}, Rust={rs_hits}, JS={js_hits}")

    # Pass gate: each language has at least 1 hit
    pass_gate = py_hits >= 1 and rs_hits >= 1 and js_hits >= 1
    print(f"Pass gate (3+ hits across all languages): {'PASS' if pass_gate else 'FAIL'}")

    return {
        "corpus": tmpdir,
        "py_hits": py_hits,
        "rs_hits": rs_hits,
        "js_hits": js_hits,
        "pass": pass_gate,
    }


if __name__ == "__main__":
    main()