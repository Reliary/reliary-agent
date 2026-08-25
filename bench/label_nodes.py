#!/usr/bin/env python3
"""Label source-code lines with universal AST node types using tree-sitter.

Trains a logistic regression on STRUCTURAL FEATURES (no tokens used) to predict:
- DECLARATION / EXPRESSION / STATEMENT / PATTERN / TYPE

Tree-sitter is used ONE-TIME to generate labels. It is never deployed in the binary.

Output: examples.json with feature vectors and labels for training.
"""
import json
import os
import sys
import re
from pathlib import Path

import tree_sitter_rust
import tree_sitter_python
import tree_sitter_javascript
from tree_sitter import Language, Parser, Node

# Universal AST node types we classify.
LABELS_5 = ['DECLARATION', 'EXPRESSION', 'STATEMENT', 'PATTERN', 'TYPE']
LABELS_EXPR = ['CALL', 'BINARY', 'UNARY', 'ACCESS', 'LITERAL']


def get_parser(lang: str):
    if lang == 'rust':
        return Parser(Language(tree_sitter_rust.language()))
    if lang == 'python':
        return Parser(Language(tree_sitter_python.language()))
    if lang == 'javascript':
        return Parser(Language(tree_sitter_javascript.language()))
    if lang == 'go':
        import tree_sitter_go
        return Parser(Language(tree_sitter_go.language()))
    if lang == 'java':
        import tree_sitter_java
        return Parser(Language(tree_sitter_java.language()))
    if lang == 'ruby':
        import tree_sitter_ruby
        return Parser(Language(tree_sitter_ruby.language()))
    raise ValueError(lang)


def compute_features(line: str, line_idx: int, all_lines: list) -> list:
    """12-dimensional feature vector. ALL structural. NO tokens."""
    # 1. Brace depth at line start: count `{` minus `}` in lines ABOVE.
    brace_depth = 0
    paren_depth = 0
    for above in all_lines[:line_idx]:
        brace_depth += above.count('{') - above.count('}')
        paren_depth += above.count('(') - above.count(')')

    brace_depth = max(0, brace_depth)
    paren_depth = max(0, paren_depth)
    # Clamp depths.
    brace_depth = min(brace_depth, 10)
    paren_depth = min(paren_depth, 10)

    # Line features.
    brace_count = line.count('{') - line.count('}')
    paren_count = line.count('(') - line.count(')')
    bracket_count = line.count('[') - line.count(']')

    # Indent: leading whitespace.
    indent_match = re.match(r'^(\s*)', line)
    indent = len(indent_match.group(1)) if indent_match else 0
    # Use bucket: 0, +1, +2, +3, +4+
    indent_bucket = min(indent // 4, 4)

    # Trailing delimiter.
    trailing = '{' if line.strip().endswith('{') else (
        '(' if line.strip().endswith('(') else (
        ':' if line.strip().endswith(':') else (
        ';' if line.strip().endswith(';') else (
        ',' if line.strip().endswith(',') else (
        '}' if line.strip().endswith('}') else (
        ')' if line.strip().endswith(')') else '.'))))))

    # Has top-level `=`. Strip strings and parens content.
    stripped = re.sub(r'"[^"]*"', '', line)
    has_eq = int('=' in stripped and '==' not in stripped and '!=' not in stripped)

    # Starts with comment.
    starts_comment = int(line.strip().startswith(('//', '#', '/*', '*')))

    # First identifier capitalized.
    id_match = re.search(r'[A-Za-z_][A-Za-z0-9_]*', line)
    starts_cap = int(bool(id_match) and id_match.group(0)[0].isupper())

    # Operator count: punctuation between identifiers.
    op_count = 0
    pieces = re.findall(r'[A-Za-z_0-9]+|[^\w\s]', stripped)
    for i in range(len(pieces) - 1):
        a, b = pieces[i], pieces[i + 1]
        if re.match(r'^[A-Za-z_0-9]+$', a) and not re.match(r'^[A-Za-z_0-9]+$', b) and b not in '()[]{}.,;:':
            op_count += 1

    # Identifier count.
    id_count = len(re.findall(r'[A-Za-z_][A-Za-z0-9_]*', stripped))

    return [
        brace_depth,
        paren_depth,
        brace_count,
        paren_count,
        bracket_count,
        op_count,
        id_count,
        indent_bucket,
        ord(trailing),  # 9 possible values
        has_eq,
        starts_comment,
        starts_cap,
    ]


# Map tree-sitter node types to our 5 universal types.
# This is ONE-TIME mapping used only for training label generation.
NODE_TYPE_MAP = {
    'function_item': 'DECLARATION',
    'struct_item': 'TYPE',
    'enum_item': 'TYPE',
    'impl_item': 'DECLARATION',
    'trait_item': 'TYPE',
    'let_declaration': 'DECLARATION',
    'use_declaration': 'STATEMENT',
    'binary_expression': 'EXPRESSION',
    'call_expression': 'EXPRESSION',
    'field_expression': 'EXPRESSION',
    'if_expression': 'STATEMENT',
    'while_expression': 'STATEMENT',
    'for_expression': 'STATEMENT',
    'match_expression': 'STATEMENT',
    'block': 'STATEMENT',
    'match_arm': 'PATTERN',
    'identifier': 'EXPRESSION',  # Placeholder, refined below.
    'integer_literal': 'EXPRESSION',
    'string_literal': 'EXPRESSION',
    'type_identifier': 'TYPE',
    'reference': 'EXPRESSION',
    'return_expression': 'STATEMENT',
    'assignment_expression': 'STATEMENT',
}

# Sub-classify EXPRESSION into CALL/BINARY/UNARY/ACCESS/LITERAL
EXPR_SUBTYPE_MAP_RUST = {
    'call_expression': 'CALL',
    'binary_expression': 'BINARY',
    'field_expression': 'ACCESS',
    'integer_literal': 'LITERAL',
    'string_literal': 'LITERAL',
    'reference': 'UNARY',
}


def map_node_type(ts_type: str, lang: str) -> str:
    """Map a tree-sitter node type to our 5 universal labels."""
    m = NODE_TYPE_MAP  # All languages share now.
    return m.get(ts_type, 'EXPRESSION')  # Default to EXPRESSION.


def get_examples_from_file(filepath: str, lang: str) -> tuple:
    """Walk tree-sitter tree, extract (features, label) pairs for each leaf line."""
    with open(filepath, 'rb') as f:
        code = f.read()
    if not code.strip():
        return [], []
    try:
        parser = get_parser(lang)
        tree = parser.parse(code)
    except Exception:
        return [], []

    lines = code.decode('utf-8', errors='ignore').splitlines()
    x5, y5 = [], []
    x_expr, y_expr = [], []

    def visit(node: Node, last_visited_line: int = -1):
        # Process one node per LINE. If the node starts on a line we haven't
        # seen yet, emit example for that line.
        line = node.start_point[0]
        if line != last_visited_line and 0 <= line < len(lines):
            text = lines[line]
            ts_type = node.type
            label = map_node_type(ts_type, lang)
            feats = compute_features(text, line, lines)
            x5.append(feats)
            y5.append(LABELS_5.index(label))
            # Sub-classify expression.
            if label == 'EXPRESSION':
                expr_label = EXPR_SUBTYPE_MAP_RUST.get(ts_type)
                if expr_label:
                    x_expr.append(feats)
                    y_expr.append(LABELS_EXPR.index(expr_label))
        # Recurse.
        for c in node.children:
            visit(c, line)

    visit(tree.root_node)
    return (x5, y5), (x_expr, y_expr)


def train_logistic(X, y, n_classes: int) -> tuple:
    """Train multinomial logistic regression. Returns (weights, biases)."""
    import numpy as np
    X = np.array(X, dtype=np.float64)
    y = np.array(y, dtype=np.int64)

    # One-hot.
    Y = np.zeros((len(y), n_classes), dtype=np.float64)
    for i, label in enumerate(y):
        Y[i, label] = 1.0

    # Standardize features (zero mean, unit var).
    mean = X.mean(axis=0)
    std = X.std(axis=0) + 1e-8
    X_norm = (X - mean) / std

    # Train via gradient descent with adaptive step sizing.
    n_features = X_norm.shape[1]
    W = np.zeros((n_features, n_classes), dtype=np.float64)
    b = np.zeros(n_classes, dtype=np.float64)
    # Reset to a tiny LR but train longer. Arc 28 (lever 4): increase iterations
    # from 1000 to 5000 and use L-BFGS-like adaptive step via bounded momentum.
    lr = 0.05
    n_iters = 5000
    momentum = np.zeros_like(W)
    momentum_b = np.zeros_like(b)
    momentum_rate = 0.9
    prev_loss = float('inf')
    for it in range(n_iters):
        scores = X_norm @ W + b
        scores -= scores.max(axis=1, keepdims=True)
        exp_scores = np.exp(scores)
        probs = exp_scores / exp_scores.sum(axis=1, keepdims=True)
        grad_W = (X_norm.T @ (probs - Y)) / len(y)
        grad_b = (probs - Y).mean(axis=0)
        momentum = momentum_rate * momentum + lr * grad_W
        momentum_b = momentum_rate * momentum_b + lr * grad_b
        W -= momentum
        b -= momentum_b
    return W.tolist(), b.tolist(), mean.tolist(), std.tolist(), [float(x) for x in np.unique(y)]


def main():
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument('--output', required=True)
    args = ap.parse_args()

    all_x5, all_y5 = [], []
    all_x_expr, all_y_expr = [], []

    # Walk 5 Rust + 5 Python + 5 JS files (proven good coverage).
    corpora = [
        ('rust', '/tmp/tokio-corpus/tokio/src/runtime'),
        ('rust', '/tmp/tokio-corpus/tokio/src/sync'),
        ('python', '/usr/lib/python3/dist-packages'),
        ('javascript', '/tmp/hyper-corpus'),
    ]

    for lang, path in corpora:
        if not os.path.exists(path):
            continue
        count = 0
        for root, _, files in os.walk(path):
            for fname in sorted(files):
                if lang == 'rust' and not fname.endswith('.rs'): continue
                if lang == 'python' and not fname.endswith('.py'): continue
                if lang == 'javascript' and not (fname.endswith('.js') or fname.endswith('.mjs')): continue
                if count >= 15: break
                fp = os.path.join(root, fname)
                (x5, y5), (x_expr, y_expr) = get_examples_from_file(fp, lang)
                all_x5.extend(x5); all_y5.extend(y5)
                all_x_expr.extend(x_expr); all_y_expr.extend(y_expr)
                count += 1
            if count >= 15: break

    print(f"Got {len(all_x5)} examples for 5-class classifier")
    print(f"Got {len(all_x_expr)} examples for expression sub-classifier")
    if len(all_x5) == 0:
        print("No examples found!")
        sys.exit(1)

    # Train both classifiers.
    W5, b5, mean5, std5, classes5 = train_logistic(all_x5, all_y5, len(LABELS_5))
    output = {
        'labels_5': LABELS_5,
        'labels_expr': LABELS_EXPR,
        'weights_5': W5,
        'bias_5': b5,
        'mean_5': mean5,
        'std_5': std5,
        'n_features': 12,
    }
    if all_x_expr:
        We, be, meane, stde, classese = train_logistic(all_x_expr, all_y_expr, len(LABELS_EXPR))
        output['weights_expr'] = We
        output['bias_expr'] = be
        output['mean_expr'] = meane
        output['std_expr'] = stde

    with open(args.output, 'w') as f:
        json.dump(output, f, indent=2)
    print(f"Wrote {args.output}")


if __name__ == '__main__':
    main()
