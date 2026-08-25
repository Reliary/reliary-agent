#!/usr/bin/env python3
"""Test cases for the autolabeler (fixture for manual review).

Each tuple: (line_text, expected_label, optional_context_lines).

Context lines are 0-5 lines BEFORE the target. They help the
brace-depth-based classifier distinguish struct fields from local vars
when the field's type isn't a Capital type identifier.
"""
from typing import List, Optional, Tuple

# Format: (line_text, expected_label, context_lines_or_None, file_path_or_None)
TestCase = Tuple[str, str, Optional[List[str]], Optional[str]]

# Line-context-only tests (no file path needed — synthetic)
SYNTHETIC_TESTS: List[TestCase] = [
    # Function definitions (top priority)
    ("fn poll_write(&mut self, cx: &mut Context, buf: &[u8]) -> Poll<io::Result<usize>>", "function_def", None, None),
    ("    fn poll_write_vectored(&mut self, cx: &mut Context, bufs: &[IoSlice]) -> Poll<io::Result<usize>>", "function_def", None, None),
    ("pub fn spawn<F>(future: F) -> JoinHandle<F::Output>", "function_def", None, None),
    ("    pub(crate) fn close(&mut self) {", "function_def", None, None),
    ("impl<T> AsyncWrite for &mut T", "function_def", None, None),
    ("pub struct JoinHandle<T>(...)", "function_def", None, None),
    ("impl<T> Future for JoinHandle<T>", "function_def", None, None),
    ("fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {", "function_def", None, None),

    # Method calls
    ("        Pin::new(&mut *self.write.lock()).poll_write(cx, buf)", "method_call", None, None),
    ("SendTimeoutError::Closed(..) => \"Closed(..)\".fmt(f),", "method_call", None, None),
    ("self.deref().fmt(fmt)", "method_call", None, None),
    ("            fmt.debug_struct(\"JoinHandle\").finish()", "method_call", None, None),
    ("self.shared.ref_count_tx.fetch_add(1, Relaxed)", "method_call", None, None),

    # Imports
    ("use tokio::sync::Mutex;", "import_or_use", None, None),

    # Let bindings
    ("let mut park = CachedParkThread::new();", "local_var", None, None),

    # Module path
    ("crate::runtime::context", "module_name", None, None),

    # Type annotation
    (": Pin<&mut Self>", "type_name", None, None),

    # Doc comments
    ("    /// Like [`poll_write`], except that it writes from a slice of buffers.", "type_name", None, None),

    # Struct fields with Capital type — easy case
    ("                value: 0,", "field_access", None, None),  # tricky: lowercase RHS

    # Self.field access (no parens)
    ("    self.inner", "field_access", None, None),

    # Type annotation in fn signature
    ("pub fn foo(x: i32, y: String) -> bool {", "function_def", None, None),
]


# Real-world tests that need file context (brace depth)
REAL_TESTS: List[TestCase] = [
    # Inside a struct, line `value: 0,` is a field
    ("                value: 0,",
     "field_access",
     ["pub struct WatchSender {",
      "    shared: Arc<Shared>,",
      "    state: State,",
      "    ref_count_tx: Option<Sender<()>>,",
      "    initial_value: T,"],
     None),

    # Inside a struct, line `is_def: false,` is a field
    ("                is_def: false,",
     "field_access",
     ["pub struct MyStruct {",
      "    name: String,",
      "    count: u32,"],
     None),

    # Inside a let block, line `value: 0,` is NOT a field
    ("    let value: 0,",
     "local_var",
     ["fn foo() {",
      "    let x = 5;",
      "    if true {"],
     None),

    # Function call with self.method()
    ("    self.inner.park()",
     "method_call",
     ["fn foo(&self) {",
      "    let x = 5;"],
     None),
]


def run_synthetic_tests(autolabel_fn) -> List[Tuple[str, str, str, str]]:
    """Run synthetic tests. Returns list of (status, line, expected, got)."""
    results = []
    for line, expected, ctx, path in SYNTHETIC_TESTS:
        got = autolabel_fn(line)
        status = "OK" if got == expected else "FAIL"
        results.append((status, line[:50], expected, got))
    return results


def run_real_tests(autolabel_fn) -> List[Tuple[str, str, str, str]]:
    """Run real tests with context."""
    results = []
    for line, expected, ctx, path in REAL_TESTS:
        if path:
            pass
        got = autolabel_fn_with_ctx(line, ctx)
        status = "OK" if got == expected else "FAIL"
        results.append((status, line[:50], expected, got))
    return results


def autolabel_fn_with_ctx(line: str, ctx: Optional[List[str]]) -> str:
    """Placeholder. Implemented in bench_homonyms_autolabel."""
    raise NotImplementedError("Use the autolabel_with_ctx function")


if __name__ == "__main__":
    print("This is a test fixture file. Import it from test scripts.")