#!/usr/bin/env python3
"""Run autolabeler tests against real source files from tokio, hyper, and Python.

This validates that the autolabeler doesn't overfit to one corpus.
"""
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

# Real test cases extracted from tokio + hyper corpora. (path, line, expected).
TOKIO_TESTS = [
    # function_def
    ("/tmp/tokio-corpus/tokio/src/blocking.rs", 56, "function_def"),
    ("/tmp/tokio-corpus/tokio/src/fs/file.rs", 728, "function_def"),
    ("/tmp/tokio-corpus/tokio/src/runtime/scheduler/multi_thread/park.rs", 76, "function_def"),
    ("/tmp/tokio-corpus/tokio/src/sync/semaphore.rs", 963, "function_def"),
    ("/tmp/tokio-corpus/tokio/src/sync/oneshot.rs", 947, "function_def"),
    ("/tmp/tokio-corpus/tokio/src/sync/mpsc/chan.rs", 246, "function_def"),
    ("/tmp/tokio-corpus/tokio/src/sync/mpsc/bounded.rs", 478, "function_def"),

    # method_call
    ("/tmp/tokio-corpus/tokio/src/io/async_write.rs", 161, "method_call"),
    ("/tmp/tokio-corpus/tokio/src/io/util/mem.rs", 143, "method_call"),
    ("/tmp/tokio-corpus/tokio/src/io/util/buf_writer.rs", 64, "method_call"),
    ("/tmp/tokio-corpus/tokio/src/io/util/buf_stream.rs", 127, "method_call"),
    ("/tmp/tokio-corpus/tokio/src/io/stdout.rs", 184, "method_call"),
    ("/tmp/tokio-corpus/tokio/src/io/stderr.rs", 135, "method_call"),

    # import_or_use (mod declarations count as imports in Rust)
    ("/tmp/tokio-corpus/tokio/src/lib.rs", 526, "import_or_use"),
]

HYPER_TESTS = [
    # function_def
    ("/tmp/hyper-corpus/src/proto/h2/client.rs", 150, "function_def"),
    ("/tmp/hyper-corpus/src/proto/h1/decode.rs", 241, "function_def"),
    ("/tmp/hyper-corpus/src/client/conn/http1.rs", 100, "function_def"),
]


def read_line(path: str, line: int) -> str:
    try:
        with open(path, errors='ignore') as f:
            for i, raw in enumerate(f, 1):
                if i == line:
                    return raw.rstrip('\n')
    except Exception:
        return ""
    return ""


def test_via_python_autolabel(line_text: str, ctx_lines=None) -> str:
    """Use Python autolabeler. Writes ctx + line_text to temp file then calls autolabel()."""
    sys.path.insert(0, str(HERE))
    from bench_homonyms_autolabel import autolabel
    import tempfile, os
    full_lines = (ctx_lines or []) + [line_text]
    with tempfile.NamedTemporaryFile(mode='w', suffix='.rs', delete=False, dir='/tmp') as f:
        f.write('\n'.join(full_lines) + '\n')
        tmp_path = f.name
    try:
        return autolabel(tmp_path, len(full_lines))
    finally:
        try: os.unlink(tmp_path)
        except: pass


def run_tests(label: str, tests, classifier):
    correct = 0
    details = []
    for path, line_no, expected in tests:
        line_text = read_line(path, line_no)
        if not line_text:
            details.append(("MISS", path.split('/')[-1], line_no, expected, "no line", ""))
            continue
        ctx_lines = []
        try:
            with open(path, errors='ignore') as f:
                all_lines = f.readlines()
            start = max(0, line_no - 6)
            ctx_lines = [l.rstrip('\n') for l in all_lines[start:line_no - 1]]
        except Exception:
            pass

        got = classifier(line_text, ctx_lines)
        ok = got == expected
        if ok:
            correct += 1
        details.append(("OK" if ok else "FAIL", path.split('/')[-1], line_no, expected, got, line_text[:60]))
    total = len(tests)
    print(f"\n=== {label} ({correct}/{total} = {correct*100//total}%) ===")
    for d in details:
        if d[0] == "OK":
            print(f"  OK   {d[1]}:{d[2]} → {d[4]} | {d[5]}")
        else:
            print(f"  FAIL {d[1]}:{d[2]} exp={d[3]} got={d[4]} | {d[5]}")
    return correct, total


if __name__ == "__main__":
    print("=" * 60)
    print("Autolabeler Regression Test Suite")
    print("=" * 60)

    print("\n--- Python `autolabel` ---")
    run_tests("Tokio via Python autolabel", TOKIO_TESTS, lambda line, ctx: test_via_python_autolabel(line, ctx))
    run_tests("Hyper via Python autolabel", HYPER_TESTS, lambda line, ctx: test_via_python_autolabel(line, ctx))

    # Plus Python edge cases
    print("\n--- Python edge cases ---")
    import importlib
    python_cases = [
        ('    def poll_write(self, buf):', 'function_def', ['class Writer:', '    """Writer."""']),
        ('        self.writer.poll_write(buf)', 'method_call', ['    def write(self):']),
        ('    async def handshake(self, key):', 'function_def', ['class Client:']),
        ('    def __init__(self, value):', 'function_def', ['class Writer:', '    """Writer."""']),
        ('from typing import Optional', 'import_or_use', []),
        ('import asyncio', 'import_or_use', []),
        ('    x = 5', 'local_var', ['def foo():']),
        ('        value = compute(x)', 'local_var', ['def foo():']),
        ('    value: int = 0', 'field_access', ['@dataclass', 'class Writer:', '    """Writer."""']),
        ('        writer.poll_write(buf)', 'method_call', ['def write_all():']),
        ('        closure(buf)', 'method_call', ['    def adapter():']),
        ('        return self.value', 'field_access', ['    def get_value(self):']),
    ]
    sys.path.insert(0, str(HERE))
    from bench_homonyms_autolabel import autolabel_with_ctx
    correct = 0
    for line, expected, ctx in python_cases:
        got = autolabel_with_ctx(line, ctx)
        ok = got == expected
        if ok: correct += 1
        print(f"  {'OK' if ok else 'FAIL'} expected={expected:15s} got={got:15s} | {line[:50]}")
    print(f"\nPython edge cases: {correct}/{len(python_cases)} = {correct*100//len(python_cases)}%")