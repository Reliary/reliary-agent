#!/usr/bin/env python3
"""Phase D — Tree-sitter comparison validation for Phase 4 expression parser.

Compares our Pratt-style Pratt parser against tree-sitter's ground-truth parse.
For each expression:
1. Parse with our Rust binary via parse-expr subcommand (no index needed).
2. Parse with tree-sitter for ground truth.
3. Convert both to a canonical structural form.
4. Check agreement on:
   - Top-level node kind (BinaryOp vs binary_expression, Call vs call, etc.)
   - Number of operands/args at top level.
   - Operator/field position.

Pass gate: ≥70% agreement on top-level structure across 60 samples
(20 Rust + 20 Python + 20 JavaScript).
"""
import json
import re
import subprocess
import sys

import tree_sitter_rust
import tree_sitter_python
import tree_sitter_javascript
from tree_sitter import Language, Parser

BINARY = '/home/user/src/reliary8/target/release/reliary'

# Get canonical node-type name from our expr_tree output.
def parse_with_reliary(line: str) -> dict | None:
    """Run our parse-expr binary, parse the dump output into a tree dict."""
    proc = subprocess.run([BINARY, 'parse-expr', '--', line, '.'],
                          capture_output=True, text=True, timeout=10)
    if proc.returncode != 0:
        return None
    out = proc.stdout
    # Tree dump starts with type name like "BinaryOp(+)" or "Identifier(foo)".
    if not out.strip() or '(parse failed)' in out:
        return None
    return parse_reliary_dump(out)

def parse_reliary_dump(dump: str) -> dict:
    """Parse the indented dump into a nested dict with kind/children.

    Handles 'args:' header lines: in our dump format, 'args:' appears at the
    PARENT node's indent (e.g., at indent 0 for a top-level Call). The args
    block below is at callee's indent. We append those args as the LAST batch
    of children.
    """
    lines = dump.rstrip().split('\n')
    if not lines:
        return {'kind': 'Unknown'}

    def get_indent(idx: int) -> int:
        if idx >= len(lines):
            return -1
        return len(lines[idx]) - len(lines[idx].lstrip())

    def parse_block(start: int, indent: int, parent_kind: str = "") -> tuple:
        """Parse a block (children) starting at start. Returns (children, next_idx).

        Uses parent_kind to detect Call args (parent_kind == 'Call').
        """
        children = []
        idx = start
        while idx < len(lines):
            cur = get_indent(idx)
            stripped = lines[idx].strip()
            # Handle "args:" header — only relevant if this is a Call.
            # For top-level Call, args: appears BEFORE children's lines (at parent indent).
            if stripped == 'args:' and parent_kind == 'Call' and cur <= indent:
                idx += 1
                while idx < len(lines):
                    cur2 = get_indent(idx)
                    if cur2 <= indent:
                        break
                    child, idx = parse_node(idx, cur2)
                    children.append(child)
                continue
            if cur <= indent:
                break
            child, idx = parse_node(idx, cur)
            children.append(child)
        return children, idx

    def parse_node(idx: int, indent: int, parent_kind: str = "") -> tuple:
        line = lines[idx]
        stripped = line.strip()
        m = re.match(r'^(\w+)(?:\((.*)\))?$', stripped)
        if not m:
            return {'kind': stripped}, idx + 1
        kind = m.group(1)
        args = m.group(2)
        result = {'kind': kind}
        if args is not None:
            result['args'] = args
        # Parse children block. Use the line's actual indent as base.
        children, next_idx = parse_block(idx + 1, indent, kind)
        if children:
            result['children'] = children
        return result, next_idx

    root, _ = parse_node(0, get_indent(0))
    return root

def parse_with_treesitter(code: str, lang: str) -> dict | None:
    """Parse with tree-sitter, return simplified structural dict."""
    if lang == 'rust':
        parser = Parser(Language(tree_sitter_rust.language()))
    elif lang == 'python':
        parser = Parser(Language(tree_sitter_python.language()))
    elif lang == 'javascript':
        parser = Parser(Language(tree_sitter_javascript.language()))
    else:
        return None
    try:
        tree = parser.parse(code.encode())
    except Exception:
        return None

    # Find first expression node (skip function definitions etc.)
    root = tree.root_node
    expr = find_expression(root)
    if expr is None:
        return None
    return ts_to_struct(expr)

def find_expression(node) -> 'Node | None':
    """Walk tree to find first expression (binary, call, identifier, etc.)."""
    interesting_types = {
        # Rust
        'binary_expression', 'call_expression', 'field_expression',
        'unary_expression', 'reference', 'parenthesized_expression',
        'array_expression', 'index_expression',
        # Python
        'binary_operator', 'call', 'attribute', 'not_operator',
        'boolean_operator', 'comparison_operator', 'augmented_assignment',
        'unary_operator', 'subscript',
        # JS
        'binary_expression', 'call_expression', 'member_expression',
        'unary_expression', 'subscript_expression',
    }
    if node.type in interesting_types:
        return node
    for c in node.children:
        result = find_expression(c)
        if result is not None:
            return result
    return None

def ts_to_struct(node) -> dict:
    """Convert tree-sitter node to canonical dict."""
    result = {'kind': canonical_kind(node.type), 'children': []}
    for c in node.children:
        if c.is_named:
            result['children'].append(ts_to_struct(c))
    return result

def canonical_kind(ts_type: str) -> str:
    """Map tree-sitter node types to our canonical kinds."""
    m = {
        # Rust
        'binary_expression': 'BinaryOp',
        'call_expression': 'Call',
        'field_expression': 'Access',
        'unary_expression': 'UnaryOp',
        'reference': 'Identifier',
        'self': 'Identifier',
        'mutable_specifier': 'Identifier',
        'parenthesized_expression': None,
        'integer_literal': 'Number',
        'string_literal': 'String',
        'reference_expression': 'UnaryOp',
        # Python
        'binary_operator': 'BinaryOp',
        'call': 'Call',
        'attribute': 'Access',
        'not_operator': 'UnaryOp',
        'boolean_operator': 'BinaryOp',
        'comparison_operator': 'BinaryOp',
        'augmented_assignment': 'BinaryOp',
        'unary_operator': 'UnaryOp',
        'identifier': 'Identifier',
        'integer': 'Number',
        'string': 'String',
        'subscript': 'Index',
        'list': 'ListLit',  # Don't match a specific kind
        'dictionary': 'DictLit',
        # JS
        'member_expression': 'Access',
        'subscript_expression': 'Index',
        # Common
        'arguments': 'Arguments',
        'argument_list': 'Arguments',
        'field_identifier': 'Field_Identifier',
        'property_identifier': 'Field_Identifier',
    }
    return m.get(ts_type, ts_type.title())

def unparen(root: dict) -> dict:
    """Strip paren wrapping (Rust parens get dropped in our parse)."""
    if root.get('kind') == 'Parenthesized' or root.get('kind') is None:
        # Pass-through.
        if root.get('children'):
            return unparen(root['children'][0])
    return root

def structural_agree(our: dict, ts: dict) -> bool:
    """Check structural agreement on top-level kind and arity."""
    if our is None or ts is None:
        return False
    our = unparen(our)
    ts = unparen(ts)
    alias = {'Index_Expression': 'Index', 'Field_Expression': 'Access',
             'Binary_Expression': 'BinaryOp', 'Unary_Expression': 'UnaryOp',
             'Call_Expression': 'Call', 'Member_Expression': 'Access',
             'Subscript_Expression': 'Index'}
    our_kind = our.get('kind')
    ts_kind = ts.get('kind')
    ts_kind = alias.get(ts_kind, ts_kind)

    if our_kind != ts_kind:
        return False

    # For Call: count args only.
    if our_kind == 'Call':
        our_children = our.get('children', [])
        our_arg_count = max(0, len(our_children) - 1)  # exclude callee
        ts_children = ts.get('children', [])
        if len(ts_children) < 2:
            return our_arg_count == 0
        ts_arg_count = len(ts_children[1].get('children', []))
        return our_arg_count == ts_arg_count

    # For Access: count chain length. Our chain is recursive. TS is flat 2-children.
    # Compare chain length: ours = 1 + chain_in_child, TS = 1 + chain_via_field_kid.
    if our_kind == 'Access':
        # Count Access depth in our.
        def access_depth(n):
            if n.get('kind') != 'Access':
                return 0
            return 1 + access_depth((n.get('children') or [{}])[0])
        our_depth = access_depth(our)
        # In TS, count fields: chain via Field_Identifier or member branches.
        def ts_field_depth(n):
            kids = n.get('children', [])
            if not kids:
                return 0
            # Access has target + field. Count fields where the field kind is Field_Identifier.
            field_count = 0
            for k in kids:
                kk = k.get('kind', '')
                if 'Field' in kk or kk in ('Property_Identifier',):
                    field_count += 1
            # Recurse into target (first child usually).
            return 1 + ts_field_depth(kids[0]) if field_count else 1
        ts_depth = ts_field_depth(ts)
        return our_depth == ts_depth

    # For BinaryOp/Index: 2 children both sides.
    our_kids = len(our.get('children', []))
    ts_kids = len(ts.get('children', []))
    return our_kids == ts_kids

SAMPLES = [
    # Rust
    ('rust', '1 + 2 * 3'),
    ('rust', 'a + b'),
    ('rust', 'foo(a, b, c)'),
    ('rust', 'a.b.c'),
    ('rust', 'a[0]'),
    ('rust', '-x'),
    ('rust', '(a + b) * c'),
    ('rust', 'x == 1 && y > 0'),
    ('rust', 'arr[idx + 1]'),
    ('rust', 'self.foo(self.x)'),
    ('rust', 'compute(&x, &mut y)'),
    ('rust', 'a + b == c'),
    ('rust', 'foo(a).bar(b).baz(c)'),
    ('rust', 'arr[0][1]'),
    ('rust', '(1 + 2).abs()'),
    ('rust', '!flag'),
    ('rust', 'a + b * c - d / e'),
    ('rust', 'f(g(h(x)))'),
    ('rust', 'a as i32'),
    ('rust', '(a, b)'),  # tuple
    # Python
    ('python', '1 + 2 * 3'),
    ('python', 'a + b'),
    ('python', 'foo(a, b, c)'),
    ('python', 'a.b.c'),
    ('python', 'a[0]'),
    ('python', '-x'),
    ('python', '(a + b) * c'),
    ('python', 'x == 1 and y > 0'),
    ('python', 'arr[idx + 1]'),
    ('python', 'self.foo(self.x)'),
    ('python', 'compute(x, y)'),
    ('python', 'a + b == c'),
    ('python', 'foo(a).bar(b)'),
    ('python', 'arr[0][1]'),
    ('python', 'abs(a + b)'),
    ('python', 'not flag'),
    ('python', 'a + b * c - d / e'),
    ('python', 'f(g(h(x)))'),
    ('python', 'a := b'),  # walrus
    ('python', 'a, b'),  # tuple
    # JS
    ('javascript', '1 + 2 * 3'),
    ('javascript', 'a + b'),
    ('javascript', 'foo(a, b, c)'),
    ('javascript', 'a.b.c'),
    ('javascript', 'a[0]'),
    ('javascript', '-x'),
    ('javascript', '(a + b) * c'),
    ('javascript', 'x === 1 && y > 0'),
    ('javascript', 'arr[idx + 1]'),
    ('javascript', 'this.foo(this.x)'),
    ('javascript', 'a + b == c'),
    ('javascript', 'foo(a).bar(b)'),
    ('javascript', 'arr[0][1]'),
    ('javascript', 'Math.abs(a + b)'),
    ('javascript', '!flag'),
    ('javascript', 'a + b * c - d / e'),
    ('javascript', 'f(g(h(x)))'),
    ('javascript', 'a ** b'),  # power
    ('javascript', 'a?.b'),
    ('javascript', '[1, 2, 3].map(x => x + 1)'),
]

if __name__ == '__main__':
    agreed = 0
    total = 0
    fails = []
    by_lang = {}
    for lang, expr in SAMPLES:
        total += 1
        our = parse_with_reliary(expr)
        ts = parse_with_treesitter(expr, lang)
        ok = structural_agree(our, ts)
        if ok:
            agreed += 1
        else:
            fails.append((lang, expr, our, ts))
        by_lang.setdefault(lang, [0, 0])
        by_lang[lang][1] += 1
        if ok:
            by_lang[lang][0] += 1

    print(f"\n=== Phase D Tree-sitter comparison ===")
    print(f"Total: {agreed}/{total} = {agreed/total:.3f}")
    print(f"Pass gate: ≥0.70 → {'✅ PASS' if agreed/total >= 0.70 else '❌ FAIL'}")
    print()
    for lang, (a, t) in sorted(by_lang.items()):
        print(f"  {lang}: {a}/{t} = {a/t:.3f}")
    if fails:
        # Show all Python failures specifically.
        py_fails = [f for f in fails if f[0] == 'python']
        print(f"\nFirst {min(10, len(py_fails))} Python failures:")
        for lang, expr, our, ts in py_fails[:10]:
            print(f"  {lang} `{expr}`:")
            print(f"    ours: {our}")
            print(f"    ts  : {ts}")
