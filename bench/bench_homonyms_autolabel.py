#!/usr/bin/env python3
"""bench_homonyms_autolabel.py — heuristic use_label classifier for homonym bench hits.

Given a file_path and line number, returns one of 8 use_labels based on the source
line content. This is the auto-labeler for classifying the HITS returned by
find_references — NOT for classifying the manually-labeled anchors (those are
already labeled in homonyms.json).

The auto-labeler is intentionally simple: pattern-match on the single line content.
It's expected to mis-classify ~10-15% of hits (acceptable for an IR metric). The
error rate is acceptable because we're measuring RANKING quality, not classification
accuracy.

Grammar-free design (Arc 38):
- No AST, no parser.
- Uses brace counting for struct/enum/class body detection (works for Rust/Go/Java).
- Uses indentation for Python class body detection.
- Multi-language keyword support: fn/def/function/class/struct/let/var/const.
- Universal patterns: `.name(`, `::name`, `name:` (in struct body), `name(`+`{` etc.
- No per-language configuration needed.

Usage:
    from bench_homonyms_autolabel import autolabel
    label = autolabel("/tmp/tokio-corpus/tokio/src/foo.rs", 42)
    label = autolabel_with_ctx(line_text, ctx_lines)  # if you have context
"""
import re
from typing import List, Optional


# The 8 categories:
# field_access     = struct/class field (e.g. self.name)
# method_call      = calling a function on a receiver
# local_var        = local variable (let/const bound)
# param            = function parameter
# module_name      = namespace/module reference
# type_name        = type/struct/enum usage
# function_def     = function definition site
# import_or_use    = import/use statement


def _get_line(file_path: str, line_no: int) -> Optional[str]:
    """Read a specific line from a file, returning None on error."""
    try:
        with open(file_path, errors="ignore") as f:
            for i, raw in enumerate(f, 1):
                if i == line_no:
                    return raw.rstrip('\n')
        return None
    except Exception:
        return None


def _in_struct_body(ctx_lines: Optional[list]) -> bool:
    """Determine if the target line is inside a struct/enum/class body.

    Handles brace-based blocks (Rust/Go/Java) AND indentation-based
    blocks (Python).

    Args:
        ctx_lines: list of lines BEFORE the target line (most recent last).

    Returns True if the target is inside a struct/enum/class body.
    """
    if not ctx_lines:
        return False

    # Strategy 1: brace tracking (Rust/Go/Java).
    for ctx in reversed(ctx_lines):
        opens = ctx.count('{')
        closes = ctx.count('}')
        if opens > closes:
            stripped = ctx.lstrip()
            if re.match(r'^(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|class)\s+\w+', stripped):
                return True
            return False
        if closes > opens:
            return False

    # Strategy 2: indentation tracking (Python).
    # Walk backwards looking for `class X:` or `@dataclass` decorator. Stop at
    # a line at the same indentation as the target (means we've left the scope).
    target_indent = None
    for ctx in reversed(ctx_lines):
        stripped_ctx = ctx.strip()
        if not stripped_ctx:
            continue
        if stripped_ctx.startswith('@dataclass'):
            return True
        ctx_indent = len(ctx) - len(ctx.lstrip())
        first_word_match = re.match(r'^(\w+)', stripped_ctx)
        if first_word_match and first_word_match.group(1) == 'class':
            return True
        # Set/track target indent if not yet established.
        if target_indent is None:
            target_indent = ctx_indent
            continue
        # If this line is less-indented than the target, we've left the body.
        if ctx_indent < target_indent:
            return False

    return False


def autolabel_with_ctx(line_content: str, ctx_lines: Optional[list] = None) -> str:
    """Classify the semantic role of a name occurrence at a given line.

    Args:
        line_content: the source line text.
        ctx_lines: list of preceding source lines (most recent last), used for
            brace-depth-based field detection inside struct/class bodies.

    Returns:
        One of: field_access, method_call, local_var, param,
                module_name, type_name, function_def, import_or_use
    """
    if line_content is None:
        return "type_name"

    # `raw` keeps leading whitespace; `line` is stripped for shape matching.
    raw = line_content
    line = line_content.strip()

    # Comment detection (universal: //, /*, *, # for Python/Shell, etc.)
    if line.startswith('//') or line.startswith('#') or line.startswith('/*') or line.startswith('*'):
        return "type_name"

    # Function/method definition (multi-language keywords: fn/def/function).
    if re.search(r'\b(?:fn|def|function)\s+\w+', line):
        return "function_def"

    # Arrow function definition (JS/TS): NAME = (args) => { or NAME = args =>
    if re.search(r'\b\w+\s*=\s*(?:\([^)]*\)\s*)?=>', line):
        return "function_def"

    # Trait impl definition: impl Trait for Type (Rust)
    if re.match(r'^impl\b', line) and 'fn ' not in line:
        return "function_def"

    # Class/struct definitions (NOT including `mod` — that's an import).
    if re.search(r'\b(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|type|class)\s+\w+', line):
        return "function_def"

    # Import / use statement (multi-language).
    if re.match(r'^(?:use|import|from|extern\s+crate|mod)\s', line):
        return "import_or_use"

    # Let / const / var / assignment binding (local variable).
    # `let X`, `const X`, `var X` (Rust/Python/JS), `X = ...` (Python).
    if re.search(r'\blet\s+(?:mut\s+)?\w+', line):
        return "local_var"
    if re.search(r'^\s*(?:const|var)\s+\w+', line):
        return "local_var"
    # Python-style assignment inside a function body: `    x = 5`
    # Use `raw` to preserve indentation.
    if re.match(r'^\s{2,}\w+\s*=', raw) and not re.search(r'\blet\s+', line):
        return "local_var"

    # Inside a struct/enum body? Lines like `name: Type` or `value: 0,` are fields.
    if _in_struct_body(ctx_lines):
        # Match `name:` (identifier followed by colon).
        if re.search(r'^\s*[a-z_][\w]*\s*:', line):
            return "field_access"

    # Struct field: `name: Type,` or `name: Type` (identifier followed by Capital type)
    if re.search(r'[a-z_]\w*\s*:\s*[A-Z]', line) and not re.search(r'\bfn\s', line):
        return "field_access"

    # Method call: .name( — receiver.method(
    if re.search(r'\.\w+\s*\(', line):
        return "method_call"

    # Self.field — field access (only if no ( follows)
    if re.search(r'\bself\.\w+', line):
        return "field_access"

    # Function call (no receiver): name( at column 0 or after =
    if re.search(r'(?:^|[=,(])\s*\w+\s*\(', line):
        return "method_call"

    # Type annotation: : Type, or -> Type
    if re.search(r'[:>]\s*[A-Z]\w*(?:<.*>)?', line):
        return "type_name"

    # Module path: crate::, super::, self::
    if re.search(r'\b(?:crate|super|self)::', line):
        return "module_name"

    # Function parameter: fn foo(param: Type) or (self: Pin<&mut Self>,
    if re.search(r'\(.*\w+\s*:', line) and re.search(r'\)', line):
        return "param"

    # Generic fallback: if it starts with a capital letter, likely a type
    return "type_name"


def autolabel(file_path: str, line_no: int, corpus_root: str = "",
              line_content: Optional[str] = None) -> str:
    """Classify the semantic role of a name occurrence at a specific location.

    Args:
        file_path: absolute path to the source file
        line_no: 1-indexed line number
        corpus_root: unused, kept for API compat
        line_content: if provided, skip reading the file (for performance)

    Returns:
        One of: field_access, method_call, local_var, param,
                module_name, type_name, function_def, import_or_use
    """
    if line_content is None:
        line_content = _get_line(file_path, line_no)
    if line_content is None:
        return "type_name"  # conservative fallback

    # Read preceding lines as context for brace-depth detection
    ctx_lines: Optional[list] = None
    try:
        with open(file_path, errors='ignore') as f:
            all_lines = f.readlines()
        if 0 < line_no - 1 <= len(all_lines):
            ctx_lines = [l.rstrip('\n') for l in all_lines[max(0, line_no - 11):line_no - 1]]
    except Exception:
        ctx_lines = None

    return autolabel_with_ctx(line_content, ctx_lines)


# Self-test
if __name__ == "__main__":
    tests = [
        ("fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output>", "function_def"),
        ("    self.inner.poll(cx)", "method_call"),
        ("let name = String::new()", "local_var"),
        ("pub struct Sender<T>", "function_def"),
        ("    value: T,", "field_access"),
        ("use crate::runtime;", "import_or_use"),
        ("self.shared.ref_count_tx.fetch_add(1, Relaxed)", "method_call"),
        (": Pin<&mut Self>", "type_name"),
        ("    fn clone(&self) -> Self {", "function_def"),
    ]
    correct = 0
    for line, expected in tests:
        got = autolabel_with_ctx(line)
        status = "OK" if got == expected else "FAIL"
        if got == expected:
            correct += 1
        print(f"  {status:40s} | {line[:50]}")
    print(f"\nautolabel: {correct}/{len(tests)} self-test cases")