/// Phase 5: Diffusion on Code Space.
/// The LLM seeds an initial candidate. The harness evolves it deterministically.

use std::process::Command;

#[derive(Clone)]
struct Candidate {
    file: String,
    old: String,
    new: String,
    test_pass: bool,
    output_len: usize,
}

/// Run a single candidate through the heal pipeline
fn test_candidate(c: &Candidate, workdir: &str) -> Candidate {
    let mut result = c.clone();
    match super::heal::heal_fix(&c.file, &c.old, &c.new, workdir) {
        Ok(msg) => {
            result.test_pass = msg.starts_with("OK");
            result.output_len = msg.len();
        }
        Err(_) => {
            result.test_pass = false;
            result.output_len = 0;
        }
    }
    result
}

/// Generate mutation variants of a candidate
fn mutate(c: &Candidate) -> Vec<Candidate> {
    let mut variants = Vec::new();

    // Mutation 1: whitespace normalization (add/remove spaces around operators)
    let ws_new = c.new
        .replace(" == ", "==")
        .replace(" != ", "!=")
        .replace(" <= ", "<=")
        .replace(" >= ", ">=");
    if ws_new != c.new {
        variants.push(Candidate {
            new: ws_new,
            ..c.clone()
        });
    }

    // Mutation 2: add explicit comparison to boolean (for threshold guards)
    if c.new.contains("&&") || c.new.contains("||") {
        let explicit_new = format!("({})", c.new);
        variants.push(Candidate {
            new: explicit_new,
            ..c.clone()
        });
    }

    // Mutation 3: negate condition
    if c.new.starts_with("if ") {
        let body = c.new.trim_start_matches("if ").trim();
        if body.starts_with('!') {
            variants.push(Candidate {
                new: format!("if {}", &body[1..]),
                ..c.clone()
            });
        } else {
            variants.push(Candidate {
                new: format!("if !({})", body),
                ..c.clone()
            });
        }
    }

    // Mutation 4: variable naming conventions (snake_case ↔ camelCase — placeholder)
    if c.new.contains('_') {
        let camel = c.new.chars().enumerate().map(|(i, ch)| {
            if ch == '_' { ' ' } else if i > 0 && c.new.as_bytes()[i-1] == b'_' { ch.to_ascii_uppercase() } else { ch }
        }).collect::<String>().replace(' ', "");
        if camel != c.new {
            variants.push(Candidate {
                new: camel,
                ..c.clone()
            });
        }
    }

    variants
}

/// Run a full diffusion: seed, mutate, test, select, repeat.
pub fn diffuse(file: &str, old: &str, new: &str, workdir: &str) -> String {
    let seed = Candidate {
        file: file.to_string(),
        old: old.to_string(),
        new: new.to_string(),
        test_pass: false,
        output_len: 0,
    };

    // Test the seed
    let seed_result = test_candidate(&seed, workdir);
    if seed_result.test_pass {
        return format!("OK: seed candidate passed tests (no diffusion needed)");
    }

    // Generate mutations
    let mut population = mutate(&seed_result);
    if population.is_empty() {
        return format!("FAIL: seed failed, no mutations possible");
    }

    // Test all mutations
    let results: Vec<Candidate> = population.into_iter()
        .map(|c| test_candidate(&c, workdir))
        .collect();

    let passers: Vec<&Candidate> = results.iter().filter(|c| c.test_pass).collect();

    if passers.is_empty() {
        return format!("FAIL: {} mutations tested, none passed", results.len());
    }

    // Select the shortest passing candidate
    let winner = passers.iter().min_by_key(|c| c.output_len).unwrap();
    // Apply the winning candidate
    if let Ok(content) = std::fs::read_to_string(&winner.file) {
        let (modified, _) = reliary_fix::apply_fixes(&content, &[(winner.old.clone(), winner.new.clone())]);
        let _ = std::fs::write(&winner.file, &modified);
    }
    format!("OK: diffusion — {} variants, {} passed, winner applied", results.len(), passers.len())
}
