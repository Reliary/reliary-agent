//! Universal AST node classifier (Chomsky-inspired).
//!
//! Predicts universal AST node types (DECLARATION/EXPRESSION/STATEMENT/PATTERN/TYPE)
//! from STRUCTURAL FEATURES only (no tokens, no grammar).
//!
//! Uses logistic regression weights trained ONE-TIME via bench/label_nodes.py
//! using tree-sitter as auto-labeler. Tree-sitter is NOT a runtime dependency.

use std::fs;

/// Universal AST node types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeLabel {
    Declaration = 0,
    Expression = 1,
    Statement = 2,
    Pattern = 3,
    Type = 4,
}

impl NodeLabel {
    pub fn from_index(i: usize) -> Self {
        match i {
            0 => Self::Declaration,
            1 => Self::Expression,
            2 => Self::Statement,
            3 => Self::Pattern,
            _ => Self::Type,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::Expression => "expression",
            Self::Statement => "statement",
            Self::Pattern => "pattern",
            Self::Type => "type",
        }
    }
}

/// Expression sub-types (only classify EXPRESSION nodes further).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprLabel {
    Call = 0,
    Binary = 1,
    Unary = 2,
    Access = 3,
    Literal = 4,
}

impl ExprLabel {
    pub fn from_index(i: usize) -> Self {
        match i {
            0 => Self::Call,
            1 => Self::Binary,
            2 => Self::Unary,
            3 => Self::Access,
            _ => Self::Literal,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Binary => "binary",
            Self::Unary => "unary",
            Self::Access => "access",
            Self::Literal => "literal",
        }
    }
}

/// Trained classifier weights.
#[derive(Debug, Clone)]
pub struct ClassifierWeights {
    pub weights_5: Vec<Vec<f32>>,
    pub bias_5: Vec<f32>,
    pub mean_5: Vec<f32>,
    pub std_5: Vec<f32>,
    pub weights_expr: Option<Vec<Vec<f32>>>,
    pub bias_expr: Option<Vec<f32>>,
    pub mean_expr: Option<Vec<f32>>,
    pub std_expr: Option<Vec<f32>>,
}

impl ClassifierWeights {
    /// Load from JSON file produced by bench/label_nodes.py.
    pub fn load(path: &str) -> std::io::Result<Self> {
        let content = fs::read_to_string(path)?;
        let v: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let parse_floats = |key: &str| -> Vec<f32> {
            v[key].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect()
        };
        let parse_floats_nested = |key: &str| -> Vec<Vec<f32>> {
            v[key].as_array().unwrap().iter()
                .map(|row| row.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect())
                .collect()
        };
        Ok(Self {
            weights_5: parse_floats_nested("weights_5"),
            bias_5: parse_floats("bias_5"),
            mean_5: parse_floats("mean_5"),
            std_5: parse_floats("std_5"),
            weights_expr: if v.get("weights_expr").is_some() {
                Some(parse_floats_nested("weights_expr"))
            } else { None },
            bias_expr: if v.get("bias_expr").is_some() {
                Some(parse_floats("bias_expr"))
            } else { None },
            mean_expr: if v.get("mean_expr").is_some() {
                Some(parse_floats("mean_expr"))
            } else { None },
            std_expr: if v.get("std_expr").is_some() {
                Some(parse_floats("std_expr"))
            } else { None },
        })
    }

    /// Classify from 12 structural features.
    pub fn predict_5(&self, features: &[f32; 12]) -> NodeLabel {
        let norm: Vec<f32> = (0..12).map(|i| (features[i] - self.mean_5[i]) / self.std_5[i]).collect();
        let mut scores = self.bias_5.clone();
        for (c, col) in self.weights_5.iter().enumerate() {
            for (f, &w) in col.iter().enumerate() {
                scores[c] += w * norm[f];
            }
        }
        // Argmax.
        let mut best = 0;
        let mut best_score = scores[0];
        for (i, &s) in scores.iter().enumerate().skip(1) {
            if s > best_score {
                best_score = s;
                best = i;
            }
        }
        NodeLabel::from_index(best)
    }

    /// Classify expression subtype (only if EXPRESSION).
    pub fn predict_expr(&self, features: &[f32; 12]) -> Option<ExprLabel> {
        if self.weights_expr.is_none() { return None; }
        let weights = self.weights_expr.as_ref().unwrap();
        let bias = self.bias_expr.as_ref().unwrap();
        let mean = self.mean_expr.as_ref().unwrap();
        let std = self.std_expr.as_ref().unwrap();
        let norm: Vec<f32> = (0..12).map(|i| (features[i] - mean[i]) / std[i]).collect();
        let mut scores = bias.clone();
        for (c, col) in weights.iter().enumerate() {
            for (f, &w) in col.iter().enumerate() {
                scores[c] += w * norm[f];
            }
        }
        let mut best = 0;
        let mut best_score = scores[0];
        for (i, &s) in scores.iter().enumerate().skip(1) {
            if s > best_score {
                best_score = s;
                best = i;
            }
        }
        Some(ExprLabel::from_index(best))
    }
}

/// Compute 12 structural features for a line.
/// ALL features are language-agnostic — no tokens used.
pub fn extract_features(line: &str, all_lines: &[&str], line_idx: usize) -> [f32; 12] {
    // 1-2. Depth from lines above.
    let mut brace_depth = 0i32;
    let mut paren_depth = 0i32;
    for above in all_lines.iter().take(line_idx) {
        for c in above.chars() {
            match c {
                '{' => brace_depth += 1,
                '}' => brace_depth -= 1,
                '(' => paren_depth += 1,
                ')' => paren_depth -= 1,
                _ => {}
            }
        }
    }
    let brace_depth = brace_depth.max(0).min(10) as f32;
    let paren_depth = paren_depth.max(0).min(10) as f32;

    // 3-5. Punctuation counts on line.
    let mut brace_count = 0i32;
    let mut paren_count = 0i32;
    let mut bracket_count = 0i32;
    // Track string/comment state.
    let mut in_string = false;
    let mut in_char = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_line_comment {
            if b == b'\n' { in_line_comment = false; }
            i += 1; continue;
        }
        if in_block_comment {
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block_comment = false; i += 2; continue;
            }
            i += 1; continue;
        }
        if in_string {
            if b == b'\\' && i + 1 < bytes.len() { i += 2; continue; }
            if b == b'"' { in_string = false; }
            i += 1; continue;
        }
        if in_char {
            if b == b'\\' && i + 1 < bytes.len() { i += 2; continue; }
            if b == b'\'' { in_char = false; }
            i += 1; continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_line_comment = true; i += 2; continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            in_block_comment = true; i += 2; continue;
        }
        if b == b'"' { in_string = true; i += 1; continue; }
        if b == b'\'' { in_char = true; i += 1; continue; }
        match b {
            b'{' | b'}' => brace_count += if b == b'{' { 1 } else { -1 },
            b'(' | b')' => paren_count += if b == b'(' { 1 } else { -1 },
            b'[' | b']' => bracket_count += if b == b'[' { 1 } else { -1 },
            _ => {}
        }
        i += 1;
    }
    let brace_count = brace_count as f32;
    let paren_count = paren_count as f32;
    let bracket_count = bracket_count as f32;

    // Strip strings and comments for content analysis.
    let mut clean = String::new();
    let mut in_s = false;
    let mut in_c = false;
    let mut in_lc = false;
    let mut in_bc = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_lc { if b == b'\n' { in_lc = false; } i += 1; continue; }
        if in_bc {
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' { in_bc = false; i += 2; continue; }
            i += 1; continue;
        }
        if in_s {
            if b == b'\\' && i + 1 < bytes.len() { i += 2; continue; }
            if b == b'"' { in_s = false; }
            i += 1; continue;
        }
        if in_c {
            if b == b'\\' && i + 1 < bytes.len() { i += 2; continue; }
            if b == b'\'' { in_c = false; }
            i += 1; continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' { in_lc = true; i += 2; continue; }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' { in_bc = true; i += 2; continue; }
        if b == b'"' { in_s = true; i += 1; continue; }
        if b == b'\'' { in_c = true; i += 1; continue; }
        clean.push(b as char);
        i += 1;
    }

    // 6. Operator count.
    let mut op_count = 0;
    let mut tokens = Vec::new();
    let mut word = String::new();
    for c in clean.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            if !word.is_empty() {
                tokens.push(word.clone());
                word.clear();
            }
            tokens.push(c.to_string());
        }
    }
    if !word.is_empty() { tokens.push(word); }
    for j in 0..tokens.len().saturating_sub(1) {
        let a = &tokens[j];
        let b = &tokens[j + 1];
        let a_is_word = a.chars().all(|c| c.is_alphanumeric() || c == '_');
        let b_is_word = b.chars().all(|c| c.is_alphanumeric() || c == '_');
        // Treat each char not in _alphanumeric individually.
        if !a_is_word && matches!(a.as_str(), "+"|"-"|"*"|"/"|"%"|"<"|">"|"="|"&"|"|"|"^"|"!") {
            // Skip — already counted.
        }
        if a_is_word && !b_is_word && !b.contains('(') && !b.contains('[') && !b.contains('{') && b != "." && b != "," && b != ";" && b != ":" {
            op_count += 1;
        }
    }
    let op_count = op_count as f32;

    // 7. Identifier count.
    let id_count = tokens.iter().filter(|t| t.chars().all(|c| c.is_alphanumeric() || c == '_') && t.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false)).count() as f32;

    // 8. Indent bucket.
    let indent = clean.chars().take_while(|c| *c == ' ' || *c == '\t').count();
    let indent_bucket = ((indent / 4).min(4)) as f32;

    // 9. Trailing delimiter code.
    let trimmed = clean.trim_end();
    let trailing = if trimmed.ends_with('{') { 1.0 }
        else if trimmed.ends_with('(') { 2.0 }
        else if trimmed.ends_with(':') { 3.0 }
        else if trimmed.ends_with(';') { 4.0 }
        else if trimmed.ends_with(',') { 5.0 }
        else if trimmed.ends_with('}') { 6.0 }
        else if trimmed.ends_with(')') { 7.0 }
        else if trimmed.ends_with('.') { 8.0 }
        else { 0.0 };

    // 10. Has top-level `=`.
    let stripped = trimmed.replace("==", "").replace("!=", "").replace("<=", "").replace(">=", "").replace("=>", "");
    let has_eq = if stripped.contains('=') { 1.0 } else { 0.0 };

    // 11. Comment start.
    let starts_comment = if line.trim_start().starts_with("//") || line.trim_start().starts_with('#') || line.trim_start().starts_with("/*") || line.trim_start().starts_with("* ") { 1.0 } else { 0.0 };

    // 12. First identifier capitalized.
    let first_id_match: Option<String> = tokens.iter().find(|t| t.chars().all(|c| c.is_alphanumeric() || c == '_') && t.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false)).cloned();
    let starts_cap = if let Some(id) = first_id_match {
        if id.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) { 1.0 } else { 0.0 }
    } else { 0.0 };

    [
        brace_depth, paren_depth, brace_count, paren_count, bracket_count,
        op_count, id_count, indent_bucket, trailing, has_eq,
        starts_comment, starts_cap,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_features_basic() {
        let lines = vec!["fn main() {", "    let x = 1;", "}"];
        let feats = extract_features("    let x = 1;", &lines.iter().map(|s| s.as_ref()).collect::<Vec<_>>(), 1);
        // Should have depth=1, indent_bucket=1, has_eq=1, etc.
        assert_eq!(feats[0], 1.0); // brace_depth
        assert_eq!(feats[7], 1.0); // indent_bucket
        assert_eq!(feats[9], 1.0); // has_eq
    }

    #[test]
    fn test_extract_features_call() {
        let lines = vec!["fn main() {", "    foo.bar(x);", "}"];
        let feats = extract_features("    foo.bar(x);", &lines.iter().map(|s| s.as_ref()).collect::<Vec<_>>(), 1);
        assert_eq!(feats[0], 1.0); // brace_depth
    }

    #[test]
    fn test_extract_features_comment() {
        let lines = vec!["fn main() {", "    // hello world", "}"];
        let feats = extract_features("    // hello world", &lines.iter().map(|s| s.as_ref()).collect::<Vec<_>>(), 1);
        assert_eq!(feats[10], 1.0); // starts_comment
    }
}
