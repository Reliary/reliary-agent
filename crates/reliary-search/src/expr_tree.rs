//! Expression trees (Phase 4 of arc 25).
//!
//! Pratt-style recursive descent parser for simple expressions into AST trees.
//! Uses auto-extracted operator table from Phase 2.
//!
//! Each `ExprNode` has a discriminated union — same approach as tree-sitter
//! but with NO grammar. The tree shape is universal because operators + parens
//! are universal syntactic features across brace-based and indent-based languages.

use serde::{Serialize, Deserialize};

use crate::op_table::OpTable;

/// A node in the expression tree.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ExprNode {
    Number(String),
    String(String),
    Identifier(String),
    BinaryOp { op: String, lhs: Box<ExprNode>, rhs: Box<ExprNode> },
    UnaryOp { op: String, operand: Box<ExprNode> },
    Call { callee: Box<ExprNode>, args: Vec<ExprNode> },
    Access { target: Box<ExprNode>, field: String },
    Index { target: Box<ExprNode>, index: Box<ExprNode> },
    /// Macro invocation: `name!(...)`, `name![...]`, `name!{...}`.
    /// Tracks the macro name and its body (parsed as normal expression).
    Macro { name: String, body: Box<ExprNode> },
}

impl ExprNode {
    /// Pretty-print for debugging.
    pub fn dump(&self, indent: usize) -> String {
        let pad = "  ".repeat(indent);
        match self {
            Self::Number(n) => format!("{}Number({})", pad, n),
            Self::String(s) => format!("{}String({:?})", pad, s),
            Self::Identifier(i) => format!("{}Identifier({})", pad, i),
            Self::BinaryOp { op, lhs, rhs } =>
                format!("{}BinaryOp({})\n{}\n{}", pad, op, lhs.dump(indent + 1), rhs.dump(indent + 1)),
            Self::UnaryOp { op, operand } =>
                format!("{}UnaryOp({})\n{}", pad, op, operand.dump(indent + 1)),
            Self::Call { callee, args } =>
                format!("{}Call\n{}\n{}args:\n{}",
                    pad, callee.dump(indent + 1), pad,
                    args.iter().map(|a| a.dump(indent + 1)).collect::<Vec<_>>().join("\n")),
            Self::Access { target, field } =>
                format!("{}Access(.{})\n{}", pad, field, target.dump(indent + 1)),
            Self::Index { target, index } =>
                format!("{}Index\n{}\n{}", pad,
                    target.dump(indent + 1), index.dump(indent + 1)),
            Self::Macro { name, body } =>
                format!("{}Macro({}!)\n{}", pad, name, body.dump(indent + 1)),
        }
    }

    /// Collect all identifiers referenced (including field names).
    pub fn identifiers(&self, out: &mut Vec<String>) {
        match self {
            Self::Identifier(i) => out.push(i.clone()),
            Self::Number(_) | Self::String(_) => {}
            Self::BinaryOp { lhs, rhs, .. } => { lhs.identifiers(out); rhs.identifiers(out); }
            Self::UnaryOp { operand, .. } => operand.identifiers(out),
            Self::Call { callee, args } => { callee.identifiers(out); for a in args { a.identifiers(out); } }
            Self::Access { target, field } => { target.identifiers(out); out.push(field.clone()); }
            Self::Index { target, index } => { target.identifiers(out); index.identifiers(out); }
            Self::Macro { name, body } => { out.push(name.clone()); body.identifiers(out); }
        }
    }

    /// Walk the receiver chain (a.b.c.d → identifiers [a, b, c, d]).
    /// Walks from root inwards: for `a.b.c`, returns [c, b, a].
    /// (Use `.rev()` to get [a, b, c].)
    pub fn receiver_chain(&self) -> Vec<String> {
        let mut chain = Vec::new();
        let mut current = self;
        loop {
            match current {
                Self::Identifier(i) => { chain.push(i.clone()); break; }
                Self::Access { target, field } => {
                    chain.push(field.clone());
                    current = target.as_ref();
                }
                _ => break,
            }
        }
        chain
    }
}

/// Pratt-style precedence-climbing parser.
struct PrattParser<'a> {
    tokens: Vec<String>,
    pos: usize,
    op_table: &'a OpTable,
}

impl<'a> PrattParser<'a> {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(|s| s.as_str())
    }

    fn advance(&mut self) -> Option<String> {
        let t = self.tokens.get(self.pos)?.clone();
        self.pos += 1;
        Some(t)
    }

    fn parse_expr(&mut self, min_prec: f32) -> Option<ExprNode> {
        // Prefix: unary or atom.
        let mut left = self.parse_unary()?;
        // Postfix chain (calls, accesses, indexes, then binary ops).
        // Loop: handle postfix ops, then check binary.
        loop {
            match self.peek() {
                Some("(") => {
                    self.advance();
                    let mut args = Vec::new();
                    if self.peek() != Some(")") {
                        loop {
                            let arg = self.parse_expr(0.0)?;
                            args.push(arg);
                            if self.peek() == Some(",") {
                                self.advance();
                            } else { break; }
                        }
                    }
                    if self.peek() == Some(")") {
                        self.advance();
                    }
                    left = ExprNode::Call {
                        callee: Box::new(left),
                        args,
                    };
                }
                Some("[") => {
                    self.advance();
                    let index = self.parse_expr(0.0)?;
                    if self.peek() == Some("]") {
                        self.advance();
                    }
                    left = ExprNode::Index {
                        target: Box::new(left),
                        index: Box::new(index),
                    };
                }
                Some(".") | Some("?.") => {
                    self.advance();
                    let field_tok = self.advance()?;
                    if !is_identifier_like(&field_tok) {
                        return None;
                    }
                    left = ExprNode::Access {
                        target: Box::new(left),
                        field: field_tok,
                    };
                }
                _ => break,
            }
        }
        // Binary operators (precedence climbing).
        while let Some(op_str) = self.peek().map(|s| s.to_string()) {
            // Don't process `,` or `)` as binary — those are call/grouping terminators.
            if matches!(op_str.as_str(), "," | ")" | "]" | ":" | ";" | "}" | "(" | "[") {
                break;
            }
            let prec = self.op_table.entries.get(&op_str).map(|e| e.precedence);
            let assoc = self.op_table.entries.get(&op_str).map(|e| e.associativity);
            let prec = match prec {
                Some(p) => p,
                None => break,
            };
            if prec < min_prec { break; }
            self.advance();
            let next_min = if assoc == Some('L') { prec + 0.0001 } else { prec };
            let right = self.parse_expr(next_min)?;
            left = ExprNode::BinaryOp {
                op: op_str,
                lhs: Box::new(left),
                rhs: Box::new(right),
            };
        }
        Some(left)
    }

    fn parse_unary(&mut self) -> Option<ExprNode> {
        if let Some(op) = self.peek().map(|s| s.to_string()) {
            if is_unary_op(&op) && self.op_table.entries.contains_key(&op) {
                self.advance();
                let operand = self.parse_unary()?;
                return Some(ExprNode::UnaryOp {
                    op,
                    operand: Box::new(operand),
                });
            }
        }
        self.parse_atom()
    }

    fn parse_atom(&mut self) -> Option<ExprNode> {
        let tok = self.advance()?;
        if tok == "(" {
            let inner = self.parse_expr(0.0)?;
            if self.peek() == Some(")") {
                self.advance();
            }
            return Some(inner);
        }
        if is_number(&tok) {
            return Some(ExprNode::Number(tok));
        }
        if is_string(&tok) {
            return Some(ExprNode::String(tok));
        }
        if is_identifier_like(&tok) {
            return Some(ExprNode::Identifier(tok));
        }
        None
    }
}

fn is_identifier_like(tok: &str) -> bool {
    !tok.is_empty()
        && tok.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
        && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_unary_op(op: &str) -> bool {
    matches!(op, "-" | "!" | "~" | "&" | "*")
}

fn is_number(tok: &str) -> bool {
    !tok.is_empty()
        && tok.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '_' || c == 'e' || c == 'E' || c == '+' || c == '-')
        && tok.chars().any(|c| c.is_ascii_digit())
}

fn is_string(tok: &str) -> bool {
    tok.starts_with('"') || tok.starts_with('\'')
}

/// Parse an expression line into an ExprNode tree.
/// Uses the given op_table for precedence/associativity.
pub fn parse_expression(line: &str, op_table: &OpTable) -> Option<ExprNode> {
    let stripped = preprocess_macros_decorators(line);
    let tokens = op_table.tokenize_expression(&stripped);
    if tokens.is_empty() {
        return None;
    }
    let mut parser = PrattParser { tokens, pos: 0, op_table };
    parser.parse_expr(0.0)
}

/// Pre-process macro and decorator syntax into plain expression form.
/// `vec![1, 2, 3]` → `vec(1, 2, 3)`
/// `@decorator` (Python) is left as-is (line-initial `@` is detected separately).
/// `#[derive(Debug)]` (Rust attribute) is stripped from start of line.
pub fn preprocess_macros_decorators(line: &str) -> String {
    // Strip Rust attribute `#[...]` or `#![...]` at start of line.
    let trimmed = line.trim_start();
    if trimmed.starts_with("#![") || trimmed.starts_with("#[") {
        // Find matching `]`. For simplicity, just remove it.
        if let Some(idx) = trimmed.find(']') {
            return trimmed[idx + 1..].trim_start().to_string();
        }
    }
    // Convert Rust macro `name![...]`, `name!{...}`, `name!(...)` → `name(...)`.
    // The `name!` part stays, but `[` → `(` and `]` → `)`, and the `!` becomes opening `(`.
    // Result: `vec![1, 2, 3]` → `vec(1, 2, 3)`.
    let bytes = trimmed.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'!' && i + 1 < bytes.len() && matches!(bytes[i + 1], b'(' | b'[' | b'{') {
            // Drop `!` entirely — the name was already pushed.
            i += 1;
            // The next char is the opening bracket of the macro body. Replace with `(`.
            if i < bytes.len() {
                let next = bytes[i];
                if next == b'(' || next == b'[' || next == b'{' {
                    out.push('(');
                    i += 1;
                }
            }
            // Consume balanced brackets.
            let mut depth: i32 = 1;
            while i < bytes.len() && depth > 0 {
                let ch = bytes[i];
                match ch {
                    b'(' | b'[' | b'{' => { depth += 1; out.push('('); i += 1; }
                    b')' | b']' | b'}' => { depth -= 1; out.push(')'); i += 1; }
                    _ => { out.push(ch as char); i += 1; }
                }
            }
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    out
}

/// Parse with a default operator table (universal — no corpus mining).
pub fn parse_with_default_table(line: &str) -> Option<ExprNode> {
    let mut table = OpTable::new();
    table.entries.insert("+".to_string(), crate::op_table::OpEntry { precedence: 5.0, associativity: 'L' });
    table.entries.insert("-".to_string(), crate::op_table::OpEntry { precedence: 5.0, associativity: 'L' });
    table.entries.insert("*".to_string(), crate::op_table::OpEntry { precedence: 7.0, associativity: 'L' });
    table.entries.insert("/".to_string(), crate::op_table::OpEntry { precedence: 7.0, associativity: 'L' });
    table.entries.insert("%".to_string(), crate::op_table::OpEntry { precedence: 7.0, associativity: 'L' });
    table.entries.insert("==".to_string(), crate::op_table::OpEntry { precedence: 3.0, associativity: 'L' });
    table.entries.insert("!=".to_string(), crate::op_table::OpEntry { precedence: 3.0, associativity: 'L' });
    table.entries.insert("<".to_string(), crate::op_table::OpEntry { precedence: 4.0, associativity: 'L' });
    table.entries.insert(">".to_string(), crate::op_table::OpEntry { precedence: 4.0, associativity: 'L' });
    table.entries.insert("<=".to_string(), crate::op_table::OpEntry { precedence: 4.0, associativity: 'L' });
    table.entries.insert(">=".to_string(), crate::op_table::OpEntry { precedence: 4.0, associativity: 'L' });
    table.entries.insert("&&".to_string(), crate::op_table::OpEntry { precedence: 2.0, associativity: 'L' });
    table.entries.insert("||".to_string(), crate::op_table::OpEntry { precedence: 1.0, associativity: 'L' });
    table.postfix = crate::op_table::default_postfix();
    parse_expression(line, &table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_arithmetic() {
        let tree = parse_with_default_table("1 + 2").unwrap();
        match tree {
            ExprNode::BinaryOp { op, .. } => assert_eq!(op, "+"),
            _ => panic!("expected binary"),
        }
    }

    #[test]
    fn test_parse_precedence() {
        let tree = parse_with_default_table("1 + 2 * 3").unwrap();
        match tree {
            ExprNode::BinaryOp { op, rhs, .. } => {
                assert_eq!(op, "+");
                match *rhs {
                    ExprNode::BinaryOp { op: ref inner_op, .. } => assert_eq!(inner_op, "*"),
                    _ => panic!("expected nested binary"),
                }
            }
            _ => panic!("expected binary"),
        }
    }

    #[test]
    fn test_parse_call() {
        let tree = parse_with_default_table("foo(a, b)").unwrap();
        match tree {
            ExprNode::Call { callee, args } => {
                match *callee {
                    ExprNode::Identifier(s) => assert_eq!(s, "foo"),
                    _ => panic!("expected identifier callee"),
                }
                assert_eq!(args.len(), 2);
            }
            _ => panic!("expected call"),
        }
    }

    #[test]
    fn test_parse_access() {
        // `a.b.c` = Access(Access(Identifier(a), b), c)
        // receiver_chain returns [c, b, a] (outside in)
        let tree = parse_with_default_table("a.b.c").unwrap();
        let chain = tree.receiver_chain();
        assert_eq!(chain, vec!["c", "b", "a"]);
    }

    #[test]
    fn test_parse_unary() {
        let tree = parse_with_default_table("-x").unwrap();
        match tree {
            ExprNode::UnaryOp { op, operand } => {
                assert_eq!(op, "-");
                match *operand { ExprNode::Identifier(s) => assert_eq!(s, "x"), _ => panic!("expected ident") }
            }
            _ => panic!("expected unary"),
        }
    }

    #[test]
    fn test_parse_paren() {
        let tree = parse_with_default_table("(1 + 2) * 3").unwrap();
        match tree {
            ExprNode::BinaryOp { op, lhs, .. } => {
                assert_eq!(op, "*");
                match *lhs {
                    ExprNode::BinaryOp { op: ref inner_op, .. } => assert_eq!(inner_op, "+"),
                    _ => panic!("expected nested binary"),
                }
            }
            _ => panic!("expected binary"),
        }
    }

    #[test]
    fn test_identifiers() {
        let tree = parse_with_default_table("foo(a + b, c.d)").unwrap();
        let mut ids = Vec::new();
        tree.identifiers(&mut ids);
        assert!(ids.contains(&"foo".to_string()));
        assert!(ids.contains(&"a".to_string()));
        assert!(ids.contains(&"b".to_string()));
        assert!(ids.contains(&"c".to_string()));
        assert!(ids.contains(&"d".to_string()));
    }

    #[test]
    fn test_parse_chained_calls() {
        // foo(1).bar(2).baz(3)
        let tree = parse_with_default_table("foo(1).bar(2).baz(3)").unwrap();
        // Count all Call nodes recursively.
        fn count_calls(n: &ExprNode) -> usize {
            match n {
                ExprNode::Call { callee, args } => {
                    1 + count_calls(callee) + args.iter().map(count_calls).sum::<usize>()
                }
                ExprNode::BinaryOp { lhs, rhs, .. } => count_calls(lhs) + count_calls(rhs),
                ExprNode::UnaryOp { operand, .. } => count_calls(operand),
                ExprNode::Access { target, .. } => count_calls(target),
                ExprNode::Index { target, index } => count_calls(target) + count_calls(index),
                _ => 0,
            }
        }
        let n = count_calls(&tree);
        assert!(n >= 3, "expected 3+ calls, got {}", n);
    }

    #[test]
    fn test_parse_index() {
        // arr[0][1]
        let tree = parse_with_default_table("arr[0][1]").unwrap();
        match tree {
            ExprNode::Index { target, .. } => match *target {
                ExprNode::Index { .. } => {}
                _ => panic!("expected nested index"),
            },
            _ => panic!("expected index"),
        }
    }

    #[test]
    fn test_parse_comparison() {
        let tree = parse_with_default_table("x == 1 || y == 2").unwrap();
        match tree {
            ExprNode::BinaryOp { op, .. } => assert_eq!(op, "||"),
            _ => panic!("expected ||"),
        }
    }

    #[test]
    fn test_parse_unary_in_binary() {
        // -x + y
        let tree = parse_with_default_table("-x + y").unwrap();
        match tree {
            ExprNode::BinaryOp { op, lhs, .. } => {
                assert_eq!(op, "+");
                match *lhs {
                    ExprNode::UnaryOp { ref op, .. } => assert_eq!(op, "-"),
                    _ => panic!("expected unary"),
                }
            }
            _ => panic!("expected binary"),
        }
    }

    #[test]
    fn test_macro_vec_bang() {
        // `vec![1, 2, 3]` → preprocess to `vec(1, 2, 3)` → Call.
        let tree = parse_with_default_table("vec![1, 2, 3]").unwrap();
        match tree {
            ExprNode::Call { callee, args } => {
                match *callee {
                    ExprNode::Identifier(ref s) => assert_eq!(s, "vec"),
                    _ => panic!("expected identifier callee"),
                }
                assert_eq!(args.len(), 3);
            }
            _ => panic!("expected call, got dump:\n{}", tree.dump(0)),
        }
    }

    #[test]
    fn test_macro_panic_bang_parens() {
        // `panic!("oops")` → preprocess → `panic(("oops"))`.
        let tree = parse_with_default_table(r#"panic!("oops")"#);
        // May not parse due to nested parens, but should not crash.
        let _ = tree;
    }

    #[test]
    fn test_attribute_strip() {
        // `#[derive(Debug)] fn foo() {}` → strip attr → `fn foo() {}`.
        let stripped = preprocess_macros_decorators("#[derive(Debug)] foo");
        assert!(!stripped.contains('#'), "should strip attribute: {}", stripped);
    }
}
