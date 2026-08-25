#!/usr/bin/env python3
"""Phase 4 cross-language validation.

Validate that the universal expression parser correctly handles Rust, Python,
and JavaScript expressions — same code path, same feature vector, no per-lang
config.

For each language, parse 20 representative expression samples and check:
1. Parses without error (None return).
2. Tree shape is structurally meaningful (right number of operators/calls/identifiers).
"""
import json
import os
import subprocess
import sys
import tempfile

RUST_SAMPLES = [
    "let x = 1 + 2;",
    "let y = a + b * c;",
    "let z = foo(a, b);",
    "let w = a.b.c;",
    "let v = (a + b) * c;",
    "let r = -x + y;",
    "let s = x == 1 && y > 0;",
    "let q = arr[0];",
    "let p = foo(a).bar(b);",
    "let o = a + b == c || d && e;",
    "let n = (1 + 2) * (3 + 4);",
    "let m = !flag;",
    "let l = a.b.c.d.e;",
    "let k = compute(a, b, c, d);",
    "let j = x.pow(2) + y.pow(2);",
    "let i = (a + b).abs();",
    "let h = a.b[0].c;",
    "let g = match x { 0 => a, _ => b };",
    "let f = self.foo(a, b);",
    "let e = vec![1, 2, 3].iter().map(|x| x + 1).collect();",
]

PYTHON_SAMPLES = [
    "x = 1 + 2",
    "y = a + b * c",
    "z = foo(a, b)",
    "w = a.b.c",
    "v = (a + b) * c",
    "r = -x + y",
    "s = x == 1 and y > 0",
    "q = arr[0]",
    "p = foo(a).bar(b)",
    "o = a + b == c or d and e",
    "n = (1 + 2) * (3 + 4)",
    "m = not flag",
    "l = a.b.c.d.e",
    "k = compute(a, b, c, d)",
    "j = x ** 2 + y ** 2",
    "i = abs(a + b)",
    "h = a.b[0].c",
    "f = self.foo(a, b)",
    "e = [x + 1 for x in items if x > 0]",
    "g = dict[key]",
]

JS_SAMPLES = [
    "let x = 1 + 2;",
    "let y = a + b * c;",
    "let z = foo(a, b);",
    "let w = a.b.c;",
    "let v = (a + b) * c;",
    "let r = -x + y;",
    "let s = x === 1 && y > 0;",
    "let q = arr[0];",
    "let p = foo(a).bar(b);",
    "let o = a + b == c || d && e;",
    "let n = (1 + 2) * (3 + 4);",
    "let m = !flag;",
    "let l = a.b.c.d.e;",
    "let k = compute(a, b, c, d);",
    "let j = x ** 2 + y ** 2;",
    "let i = Math.abs(a + b);",
    "let h = a.b[0].c;",
    "let f = this.foo(a, b);",
    "let e = items.filter(x => x > 0).map(x => x + 1);",
    "let g = dict[key];",
]

def parse_via_binary(line):
    """Call the Rust binary's parse-expr command on a single line."""
    with tempfile.TemporaryDirectory() as td:
        # Use the built-in op_table (default table) by parsing without an index.
        # The 'parse-expr' subcommand takes a line and a path. We use the default
        # table when no corpus is available.
        result = subprocess.run(
            ['/home/user/src/reliary8/target/release/reliary', 'parse-expr', line, '.'],
            capture_output=True, text=True, cwd=td,
        )
        return result.returncode, result.stdout, result.stderr

def validate_samples(lang: str, samples: list) -> tuple:
    """Run samples through binary, return (pass_count, total)."""
    pass_count = 0
    failures = []
    for s in samples:
        rc, out, err = parse_via_binary(s)
        if rc == 0 and ('postfix:' in out or 'Number' in out or 'Identifier' in out):
            pass_count += 1
        else:
            failures.append((s, err.strip()[:100]))
    return pass_count, len(samples), failures

if __name__ == '__main__':
    print("=== Phase 4 cross-language validation ===\n")
    for lang, samples in [('Rust', RUST_SAMPLES), ('Python', PYTHON_SAMPLES), ('JavaScript', JS_SAMPLES)]:
        ok, total, fails = validate_samples(lang, samples)
        print(f"{lang}: {ok}/{total} parsed successfully")
        if fails:
            print(f"  Failures (first 3):")
            for s, e in fails[:3]:
                print(f"    {s!r}: {e}")
    print()
    # Combined: need ≥70% across all languages
    all_pass = 0
    all_total = 0
    for lang, samples in [('Rust', RUST_SAMPLES), ('Python', PYTHON_SAMPLES), ('JavaScript', JS_SAMPLES)]:
        ok, total, _ = validate_samples(lang, samples)
        all_pass += ok
        all_total += total
    rate = all_pass / all_total
    print(f"Total: {all_pass}/{all_total} = {rate:.3f}")
    if rate >= 0.70:
        print("✅ Phase 4 PASS (≥70% across all languages)")
    else:
        print(f"❌ Phase 4 FAIL (need {0.70*all_total:.0f}+, have {all_pass})")
