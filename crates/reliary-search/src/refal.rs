//! Arc 27 Lever 4 — REFAL graph reduction / query language.
//!
//! REFAL (Recursive Functions Algorithmic Language) is a term-rewriting system
//! where trees are patterns and reductions are substitutions. We use it as a
//! query language on top of expression trees: pattern → template.
//!
//! Each ExprNode variant becomes a pattern. Wildcard `_` matches anything.
//! Captures `?name` bind sub-trees. Templates can reference captures.

use std::collections::HashMap;

use crate::expr_tree::ExprNode;

/// Pattern syntax (parsed from string).
/// `_` = wildcard, `?name` = capture, anything else = literal match.
#[derive(Debug, Clone, PartialEq)]
pub enum Pat {
    /// Matches anything, binds no name.
    Wildcard,
    /// Matches anything, binds to name.
    Capture(String),
    /// Literal identifier match.
    Ident(String),
    /// Literal number match.
    Number(String),
    /// Literal string match.
    String(String),
    /// Binary op: pat OP pat. OP is any binary operator string.
    Binary { op: String, lhs: Box<Pat>, rhs: Box<Pat> },
    /// Unary op.
    Unary { op: String, operand: Box<Pat> },
    /// Function call.
    Call { callee: Box<Pat>, args: Vec<Pat> },
    /// Field access: target.field_name.
    Access { target: Box<Pat>, field: String },
    /// Index: target[index].
    Index { target: Box<Pat>, index: Box<Pat> },
}

/// Bindings accumulated during a match.
pub type Bindings = HashMap<String, ExprNode>;

/// Parse a pattern from a string. Pattern syntax:
/// `Call(_, ?args)`       — call with wildcard callee, capture args
/// `BinaryOp(+) -> ...`   — binary op with literal +
/// `_ -> _`               — wildcard (matches anything)
/// `?x`                   — capture (binds ExprNode to "x")
pub fn parse_pattern(s: &str) -> Result<Pat, String> {
    let s = s.trim();
    let (pat, _) = parse_pat_token(s).map_err(|e| format!("parse_pattern: {}", e))?;
    Ok(pat)
}

fn parse_pat_token(s: &str) -> Result<(Pat, usize), String> {
    let s = s.trim_start();
    if s.is_empty() { return Err("empty pattern".to_string()); }

    // `_` is wildcard.
    if s.starts_with('_') && (s.len() == 1 || !is_ident_char(s.as_bytes()[1])) {
        return Ok((Pat::Wildcard, 1));
    }

    // `?name` is capture.
    if s.starts_with('?') {
        let bytes = s.as_bytes();
        let mut end = 1;
        while end < bytes.len() && is_ident_char(bytes[end]) { end += 1; }
        let name = s[1..end].to_string();
        return Ok((Pat::Capture(name), end));
    }

    // String literal "..."
    if s.starts_with('"') || s.starts_with('\'') {
        let quote = s.as_bytes()[0];
        let bytes = s.as_bytes();
        let mut end = 1;
        while end < bytes.len() && bytes[end] != quote {
            if bytes[end] == b'\\' && end + 1 < bytes.len() { end += 2; continue; }
            end += 1;
        }
        let content = s[1..end].to_string();
        if end < bytes.len() { end += 1; } // consume closing quote
        return Ok((Pat::String(content), end));
    }

    // Identifier (may include kind prefix like "Call" or "BinaryOp(+)").
    if is_ident_start(s.as_bytes()[0]) {
        let bytes = s.as_bytes();
        let mut end = 0;
        while end < bytes.len() && is_ident_char(bytes[end]) { end += 1; }
        let kind = s[..end].to_string();
        let rest = s[end..].trim_start();

        if let Some(after_paren) = rest.strip_prefix('(') {
            // Parse arguments.
            let mut args = Vec::new();
            let mut pos = 0;
            let bytes = after_paren.as_bytes();
            while pos < bytes.len() && bytes[pos] != b')' {
                let sub = &after_paren[pos..];
                let (pat, consumed) = parse_pat_token(sub)?;
                args.push(pat);
                pos += consumed;
                let sub2 = after_paren[pos..].trim_start();
                pos = sub2.as_ptr() as usize - after_paren.as_ptr() as usize;
                if after_paren[pos..].starts_with(',') {
                    pos += 1;
                    pos += after_paren[pos..].chars().take_while(|c| c.is_whitespace()).count();
                }
            }
            if pos < after_paren.len() && after_paren.as_bytes()[pos] == b')' { pos += 1; }
            return Ok((match_kind(&kind, args, &rest[pos..]), pos + (rest.len() - after_paren.len())));
        }

        // Identifier (no parens).
        return Ok((Pat::Ident(kind), end));
    }

    Err(format!("unexpected token at: {}", &s[..20.min(s.len())]))
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Extract op string from a Pat: literal if Ident/String, capture if Capture.
fn extract_op_string(pat: &Pat) -> String {
    match pat {
        Pat::Ident(s) | Pat::String(s) => s.clone(),
        Pat::Capture(_) => "<capture>".to_string(), // Will be bound at match time.
        _ => "?".to_string(),
    }
}

fn match_kind(kind: &str, args: Vec<Pat>, _rest: &str) -> Pat {
    match kind {
        "Call" | "call" => {
            // Call(callee, arg1, arg2, ...) → first arg is callee, rest are call args.
            if args.is_empty() {
                return Pat::Wildcard; // Shouldn't happen.
            }
            let mut iter = args.into_iter();
            let callee = iter.next().unwrap();
            let call_args: Vec<Pat> = iter.collect();
            Pat::Call { callee: Box::new(callee), args: call_args }
        }
        "Access" | "access" => {
            // Access(target, field) — but field is the LAST identifier of source.
            // For pattern purposes, treat second arg as field name (Ident).
            if args.len() >= 2 {
                let field = match &args[1] {
                    Pat::Ident(s) => s.clone(),
                    _ => return Pat::Wildcard,
                };
                Pat::Access { target: Box::new(args[0].clone()), field }
            } else { Pat::Wildcard }
        }
        "Index" | "index" => {
            if args.len() >= 2 {
                Pat::Index { target: Box::new(args[0].clone()), index: Box::new(args[1].clone()) }
            } else { Pat::Wildcard }
        }
        "BinaryOp" | "binary" => {
            if args.len() >= 3 {
                // BinaryOp(OP, lhs, rhs). OP can be Ident, String, or capture.
                Pat::Binary {
                    op: extract_op_string(&args[0]),
                    lhs: Box::new(args[1].clone()),
                    rhs: Box::new(args[2].clone()),
                }
            } else { Pat::Wildcard }
        }
        "UnaryOp" | "unary" => {
            if args.len() >= 2 {
                Pat::Unary {
                    op: extract_op_string(&args[0]),
                    operand: Box::new(args[1].clone()),
                }
            } else { Pat::Wildcard }
        }
        // Identifier literal at this position means "match an identifier with this text".
        _ => Pat::Ident(kind.to_string()),
    }
}

/// Try to match `pat` against `expr`, returning bindings if successful.
pub fn match_pattern(pat: &Pat, expr: &ExprNode) -> Option<Bindings> {
    let mut bindings = Bindings::new();
    if match_pat(pat, expr, &mut bindings) {
        Some(bindings)
    } else {
        None
    }
}

fn match_pat(pat: &Pat, expr: &ExprNode, bindings: &mut Bindings) -> bool {
    match pat {
        Pat::Wildcard => true,
        Pat::Capture(name) => {
            bindings.insert(name.clone(), expr.clone());
            true
        }
        Pat::Ident(s) => match expr {
            ExprNode::Identifier(actual) => actual == s,
            _ => false,
        }
        Pat::Number(s) => match expr {
            ExprNode::Number(actual) => actual == s,
            _ => false,
        },
        Pat::String(s) => match expr {
            ExprNode::String(actual) => actual == s,
            _ => false,
        },
        Pat::Binary { op, lhs, rhs } => match expr {
            ExprNode::BinaryOp { op: ref aop, lhs: ref alhs, rhs: ref arhs } => {
                // If op is <capture> placeholder, bind it from actual op.
                if op == "<capture>" {
                    bindings.insert("op".to_string(), ExprNode::String(aop.clone()));
                } else if aop != op {
                    return false;
                }
                match_pat(lhs, alhs, bindings) && match_pat(rhs, arhs, bindings)
            }
            _ => false,
        },
        Pat::Unary { op, operand } => match expr {
            ExprNode::UnaryOp { op: ref aop, operand: ref aopd } => {
                if op == "<capture>" {
                    bindings.insert("op".to_string(), ExprNode::String(aop.clone()));
                } else if aop != op {
                    return false;
                }
                match_pat(operand, aopd, bindings)
            }
            _ => false,
        },
        Pat::Call { callee, args } => match expr {
            ExprNode::Call { callee: acallee, args: aargs } => {
                match_pat(callee, acallee, bindings)
                    && args.len() == aargs.len()
                    && args.iter().zip(aargs.iter()).all(|(p, e)| match_pat(p, e, bindings))
            }
            _ => false,
        },
        Pat::Access { target, field } => match expr {
            ExprNode::Access { target: atarget, field: afield } => {
                afield == field && match_pat(target, atarget, bindings)
            }
            _ => false,
        },
        Pat::Index { target, index } => match expr {
            ExprNode::Index { target: atarget, index: aindex } => {
                match_pat(target, atarget, bindings) && match_pat(index, aindex, bindings)
            }
            _ => false,
        },
    }
}

/// Find all sub-trees matching `pat` inside `expr`.
pub fn find_all_matches(pat: &Pat, expr: &ExprNode) -> Vec<Bindings> {
    let mut results = Vec::new();
    walk(pat, expr, &mut results);
    results
}

/// One match result from querying a file.
#[derive(Clone, Debug)]
pub struct FileMatch {
    pub line: i32,
    pub expr_text: String,
    pub bindings: std::collections::BTreeMap<String, String>,
}

/// Query a file's source by parsing each line and running the pattern.
pub fn query_file(file_path: &str, pat: &Pat, op_table: &crate::op_table::OpTable) -> Vec<FileMatch> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut results = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_trim = line.trim();
        if line_trim.is_empty() || line_trim.starts_with("//") || line_trim.starts_with("#") {
            continue;
        }
        let parsed = match crate::expr_tree::parse_expression(line, op_table) {
            Some(p) => p,
            None => continue,
        };
        for binding in find_all_matches(pat, &parsed) {
            let mut ser_binding = std::collections::BTreeMap::new();
            for (k, v) in &binding {
                ser_binding.insert(k.clone(), expr_text(v));
            }
            results.push(FileMatch {
                line: (idx + 1) as i32,
                expr_text: line.trim().to_string(),
                bindings: ser_binding,
            });
        }
    }
    results
}

/// Compact string representation of an ExprNode for serialization.
fn expr_text(expr: &ExprNode) -> String {
    use crate::expr_tree::ExprNode;
    match expr {
        ExprNode::Number(n) => n.clone(),
        ExprNode::String(s) => s.clone(),
        ExprNode::Identifier(i) => i.clone(),
        ExprNode::BinaryOp { op, lhs, rhs } => {
            format!("({} {} {})", expr_text(lhs), op, expr_text(rhs))
        }
        ExprNode::UnaryOp { op, operand } => format!("({}{})", op, expr_text(operand)),
        ExprNode::Call { callee, args } => {
            let args_str: Vec<String> = args.iter().map(expr_text).collect();
            format!("{}({})", expr_text(callee), args_str.join(", "))
        }
        ExprNode::Access { target, field } => format!("{}.{}", expr_text(target), field),
        ExprNode::Index { target, index } => format!("{}[{}]", expr_text(target), expr_text(index)),
        ExprNode::Macro { name, body } => format!("{}!({})", name, expr_text(body)),
    }
}

fn walk(pat: &Pat, expr: &ExprNode, results: &mut Vec<Bindings>) {
    // Try matching at this node.
    if let Some(b) = match_pattern(pat, expr) {
        results.push(b);
    }
    // Recurse into children.
    match expr {
        ExprNode::BinaryOp { lhs, rhs, .. } => {
            walk(pat, lhs, results);
            walk(pat, rhs, results);
        }
        ExprNode::UnaryOp { operand, .. } => walk(pat, operand, results),
        ExprNode::Call { callee, args } => {
            walk(pat, callee, results);
            for a in args { walk(pat, a, results); }
        }
        ExprNode::Access { target, .. } => walk(pat, target, results),
        ExprNode::Index { target, index } => {
            walk(pat, target, results);
            walk(pat, index, results);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_call() -> ExprNode {
        ExprNode::Call {
            callee: Box::new(ExprNode::Identifier("foo".to_string())),
            args: vec![ExprNode::Number("1".to_string()), ExprNode::Number("2".to_string())],
        }
    }

    #[test]
    fn test_parse_wildcard() {
        let p = parse_pattern("_").unwrap();
        assert_eq!(p, Pat::Wildcard);
    }

    #[test]
    fn test_parse_capture() {
        let p = parse_pattern("?x").unwrap();
        assert_eq!(p, Pat::Capture("x".to_string()));
    }

    #[test]
    fn test_parse_call_pattern() {
        let p = parse_pattern("Call(_, ?args)").unwrap();
        match p {
            Pat::Call { callee, args } => {
                assert_eq!(*callee, Pat::Wildcard);
                assert_eq!(args.len(), 1);
                assert_eq!(args[0], Pat::Capture("args".to_string()));
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn test_match_call_pattern() {
        let pat = parse_pattern("Call(_, _, _)").unwrap();
        let expr = make_call();
        assert!(match_pattern(&pat, &expr).is_some());
    }

    #[test]
    fn test_match_call_pattern_no_args_fails() {
        // Call(_, _) expects 1 arg but our call has 2 args — should fail.
        let pat = parse_pattern("Call(_, _)").unwrap();
        let expr = make_call();
        assert!(match_pattern(&pat, &expr).is_none());
    }

    #[test]
    fn test_match_captures_callee() {
        // For Call, first positional arg = callee, rest = call args.
        let pat = parse_pattern("Call(?callee, _, _)").unwrap();
        let expr = make_call(); // foo(1, 2)
        let bindings = match_pattern(&pat, &expr).unwrap();
        assert!(bindings.contains_key("callee"));
    }

    #[test]
    fn test_match_binary() {
        let pat = parse_pattern("BinaryOp(?op, ?a, ?b)").unwrap();
        let expr = ExprNode::BinaryOp {
            op: "+".to_string(),
            lhs: Box::new(ExprNode::Number("1".to_string())),
            rhs: Box::new(ExprNode::Number("2".to_string())),
        };
        let bindings = match_pattern(&pat, &expr).unwrap();
        assert!(bindings.contains_key("op"));
        assert!(bindings.contains_key("a"));
        assert!(bindings.contains_key("b"));
        let op_value = bindings.get("op").unwrap();
        match op_value {
            ExprNode::String(s) => assert_eq!(s, "+"),
            _ => panic!("expected string capture for op"),
        }
    }

    #[test]
    fn test_find_all_calls() {
        let pat = parse_pattern("Call(_, _)").unwrap();
        let expr = ExprNode::Call {
            callee: Box::new(ExprNode::Call {
                callee: Box::new(ExprNode::Identifier("outer".to_string())),
                args: vec![ExprNode::Number("1".to_string())],
            }),
            args: vec![ExprNode::Number("2".to_string())],
        };
        let matches = find_all_matches(&pat, &expr);
        // Outer call + inner call.
        assert!(matches.len() >= 2);
    }

    #[test]
    fn test_parse_access_pattern() {
        let p = parse_pattern("Access(_, bar)").unwrap();
        match p {
            Pat::Access { field, .. } => assert_eq!(field, "bar"),
            _ => panic!("expected Access"),
        }
    }

    #[test]
    fn test_match_access() {
        let pat = parse_pattern("Access(?t, foo)").unwrap();
        let expr = ExprNode::Access {
            target: Box::new(ExprNode::Identifier("x".to_string())),
            field: "foo".to_string(),
        };
        assert!(match_pattern(&pat, &expr).is_some());
    }
}
