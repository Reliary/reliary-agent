//! V70 P6: `reliary bench` — deterministic benchmark generation + scoring.
//!
//! `bench gen`     — derive comprehension questions + verifiable GT facts
//!                   from any index (no LLM, no Python).
//! `bench verify`  — score a results JSONL against GT deterministically:
//!                   precision (claims that verify), recall (GT facts found),
//!                   F1. Reuses the `verify` claim extractor.
//!
//! Output of both is deterministic: seeded shuffle, ordered SQL, sorted GT.

use crate::verify;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// Deterministic 64-bit LCG + Fisher-Yates shuffle (no rand dependency).
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407))
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        if v.is_empty() {
            return;
        }
        for i in (1..v.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
}

/// One GT fact.
#[derive(Debug, Clone)]
struct GtFact {
    sym: String,
    file: String,
    line: i32,
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

const STOP: &[&str] = &[
    "self", "some", "none", "true", "false", "path", "file", "line", "name",
    "value", "string", "result", "data", "test", "new", "default", "into",
    "from", "with", "index", "error", "parse", "format", "print",
];

fn source_files(db: &Connection, rust_only: bool, skip: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut stmt = match db.prepare_cached(
        "SELECT DISTINCT file_path FROM file_map WHERE is_source = 1 ORDER BY file_path",
    ) {
        Ok(s) => s,
        Err(_) => return out,
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(_) => return out,
    };
    while let Ok(Some(r)) = rows.next() {
        let fp: String = r.get(0).unwrap_or_default();
        if rust_only && !fp.ends_with(".rs") {
            continue;
        }
        if skip.iter().any(|s| fp.contains(&format!("/{}/", s))) {
            continue;
        }
        out.push(fp);
    }
    out
}

/// Def sites (file, line0) for a phrase, restricted to the given files.
fn def_sites(db: &Connection, phrase: &str, files: &[String]) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    let stem = reliary_search::stem_identifier(phrase);
    let Ok(Some(pid)) = reliary_search::symbol::phrase_id_for(db, &stem) else {
        return out;
    };
    let Ok(mut stmt) = db.prepare_cached(
        "SELECT f.file_path, o.line FROM occurrence o
         JOIN file_map f ON f.id = o.file_id
         WHERE o.phrase_id = ?1 AND o.is_def = 1 AND o.tag IN (1, 2)
         ORDER BY f.file_path, o.line",
    ) else {
        return out;
    };
    let Ok(mut rows) = stmt.query(rusqlite::params![pid]) else {
        return out;
    };
    while let Ok(Some(r)) = rows.next() {
        let fp: String = r.get(0).unwrap_or_default();
        if files.contains(&fp) {
            out.push((fp, r.get::<_, i32>(1).unwrap_or(0)));
        }
    }
    out
}

fn nondef_count(db: &Connection, phrase: &str) -> usize {
    let stem = reliary_search::stem_identifier(phrase);
    let Ok(Some(pid)) = reliary_search::symbol::phrase_id_for(db, &stem) else {
        return 0;
    };
    db.query_row(
        "SELECT COUNT(*) FROM occurrence o JOIN file_map f ON f.id = o.file_id
         WHERE o.phrase_id = ?1 AND o.is_def = 0 AND f.is_source = 1",
        rusqlite::params![pid],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0) as usize
}

fn callers_of(db: &Connection, phrase: &str, limit: usize) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    let stem = reliary_search::stem_identifier(phrase);
    let Ok(Some(pid)) = reliary_search::symbol::phrase_id_for(db, &stem) else {
        return out;
    };
    let Ok(mut stmt) = db.prepare_cached(
        "SELECT f.file_path, o.line FROM occurrence o
         JOIN file_map f ON f.id = o.file_id
         WHERE o.phrase_id = ?1 AND o.is_def = 0 AND f.is_source = 1
         ORDER BY f.file_path, o.line LIMIT ?2",
    ) else {
        return out;
    };
    let Ok(mut rows) = stmt.query(rusqlite::params![pid, limit as i64]) else {
        return out;
    };
    while let Ok(Some(r)) = rows.next() {
        out.push((r.get(0).unwrap_or_default(), r.get(1).unwrap_or(0)));
    }
    out
}

/// Grammar-free struct-field scan: find `struct <Name>` then grab
/// `name: Type,` lines until the closing brace (depth-aware, comment-skip).
fn struct_fields(lines: &[String], type_name: &str) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    // Phrases are stored lowercase; match the source declaration
    // case-insensitively (struct StructuralResult ~ type_name "structuralresult").
    let needle = format!("struct {}", type_name);
    let needle_lower = needle.to_lowercase();
    let mut in_struct = false;
    let mut depth: i32 = 0;
    for (i, raw) in lines.iter().enumerate() {
        let t = raw.trim();
        if !in_struct {
            if t.to_lowercase().contains(&needle_lower) && (t.contains("struct")) {
                // V74: a unit struct (`pub struct Foo;`) or tuple struct
                // (`struct Foo(u8);`) ends on its own line — never enter the
                // brace-scan state or following code is read as fields.
                if t.trim_end().ends_with(';') {
                    break;
                }
                if t.contains('(') && !t.contains('{') {
                    break;
                }
                in_struct = true;
                depth = t.matches('{').count() as i32 - t.matches('}').count() as i32;
                if depth <= 0 {
                    // Brace on the next line — wait for it.
                    depth = 1;
                }
            }
            continue;
        }
        depth += t.matches('{').count() as i32;
        depth -= t.matches('}').count() as i32;
        if depth <= 0 {
            break;
        }
        if t.starts_with("//") || t.starts_with("/*") || t.starts_with("*") {
            continue;
        }
        // field line: `pub name: Type,`
        let body = t.strip_prefix("pub ").unwrap_or(t);
        if let Some((name, _)) = body.split_once(':') {
            let name = name.trim();
            if is_ident(name) && !name.starts_with('\'') {
                out.push((name.to_string(), i as i32));
            }
        }
    }
    out
}

/// Generate questions + GT facts from an index. Deterministic per (repo, seed).
pub fn generate(index_path: &str, seed: u64) -> Value {
    let db = match Connection::open(index_path) {
        Ok(d) => d,
        Err(e) => return json!({"error": format!("open index: {}", e)}),
    };
    let repo = std::path::Path::new(index_path)
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());

    let skip = ["bench", "scripts", "fixtures", "configs", "tests"];
    let mut files = source_files(&db, true, &skip);
    if files.is_empty() {
        files = source_files(&db, false, &skip);
    }
    if files.is_empty() {
        return json!({"repo": repo, "questions": [], "n_files": 0});
    }

    // Symbol candidates: defs in these files.
    let mut funcs: Vec<(String, String, i32)> = Vec::new();
    {
        let Ok(mut stmt) = db.prepare_cached(
            "SELECT DISTINCT p.phrase, f.file_path, o.line FROM occurrence o
             JOIN phrases p ON p.id = o.phrase_id
             JOIN file_map f ON f.id = o.file_id
             WHERE o.is_def = 1 AND o.tag = 1 AND f.is_source = 1
             ORDER BY p.phrase, f.file_path, o.line",
        ) else {
            return json!({"repo": repo, "questions": [], "n_files": files.len()});
        };
        let Ok(mut rows) = stmt.query([]) else {
            return json!({"repo": repo, "questions": [], "n_files": files.len()});
        };
        while let Ok(Some(r)) = rows.next() {
            let ph: String = r.get(0).unwrap_or_default();
            let fp: String = r.get(1).unwrap_or_default();
            let ln: i32 = r.get(2).unwrap_or(0);
            if files.contains(&fp) && is_ident(&ph) && !STOP.contains(&ph.as_str()) {
                funcs.push((ph, fp, ln));
            }
        }
    }
    let mut rng = Lcg::new(
        repo.bytes().fold(seed, |a, b| a.wrapping_mul(31).wrapping_add(b as u64)) ^ seed,
    );
    rng.shuffle(&mut funcs);

    let mut questions: Vec<Value> = Vec::new();
    let mut used: BTreeSet<String> = BTreeSet::new();

    // q1: unambiguous function def, snake_case, 5+ chars, has usages.
    for (ph, fp, ln) in &funcs {
        if used.contains(&ph.to_lowercase()) || ph.len() < 5 || !ph.contains('_') {
            continue;
        }
        if def_sites(&db, ph, &files).len() != 1 || nondef_count(&db, ph) < 1 {
            continue;
        }
        used.insert(ph.to_lowercase());
        questions.push(json!({
            "query_id": "q1_def",
            "question": format!("Where is the function `{}` defined? Give the file path and line number.", ph),
            "gt": [{"sym": ph, "file": basename(fp), "line": ln + 1}],
        }));
        break;
    }

    // q2: callers (2-8).
    for (ph, _fp, _ln) in &funcs {
        if used.contains(&ph.to_lowercase()) || ph.len() < 4 {
            continue;
        }
        let callers = callers_of(&db, ph, 8);
        if (2..=8).contains(&callers.len()) {
            used.insert(ph.to_lowercase());
            let gt: Vec<Value> = callers.iter().map(|(f, l)| json!({"sym": ph, "file": basename(f), "line": l + 1})).collect();
            questions.push(json!({
                "query_id": "q2_callers",
                "question": format!("Which functions or modules call `{}`? List each caller file:line site.", ph),
                "gt": gt,
            }));
            break;
        }
    }

    // q3: methods on a type.
    {
        let mut types: Vec<(String, String, i32)> = Vec::new();
        let Ok(mut stmt) = db.prepare_cached(
            "SELECT DISTINCT p.phrase, f.file_path, o.line FROM occurrence o
             JOIN phrases p ON p.id = o.phrase_id
             JOIN file_map f ON f.id = o.file_id
             WHERE o.is_def = 1 AND o.tag = 2 AND f.is_source = 1
             ORDER BY p.phrase, f.file_path LIMIT 500",
        ) else {
            return json!({"repo": repo, "questions": questions, "n_files": files.len()});
        };
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                let ph: String = r.get(0).unwrap_or_default();
                let fp: String = r.get(1).unwrap_or_default();
                let ln: i32 = r.get(2).unwrap_or(0);
                if files.contains(&fp) && is_ident(&ph) && ph.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
                    types.push((ph, fp, ln));
                }
            }
        }
        rng.shuffle(&mut types);
        for (ph, fp, ln) in &types {
            if used.contains(&ph.to_lowercase()) {
                continue;
            }
            if let Ok(mr) = reliary_search::callgraph_v2::find_methods_on(&db, ph) {
                // V75: the question asks for PUBLIC methods, so the GT must
                // contain only public ones. Private helpers in the GT made a
                // correct answer look incomplete.
                let methods: Vec<(String, i32)> = mr
                    .methods
                    .iter()
                    .filter(|m| m.is_pub && !m.is_field)
                    .map(|m| (m.name.clone(), m.line))
                    .filter(|(n, _)| !STOP.contains(&n.to_lowercase().as_str()))
                    .take(8)
                    .collect();
                if methods.len() >= 3 {
                    used.insert(ph.to_lowercase());
                    // V75: `MethodOn.line` is 1-indexed (brace graph) — no +1.
                    let gt: Vec<Value> = methods.iter().map(|(m, l)| json!({"sym": m, "file": basename(fp), "line": l})).collect();
                    questions.push(json!({
                        "query_id": "q3_methods",
                        "question": format!("List the public methods defined on the type `{}` (its impl block is in {} around line {}).", ph, basename(fp), ln + 1),
                        "gt": gt,
                    }));
                    break;
                }
            }
        }
    }

    // q4: struct fields.
    {
        let mut types: Vec<(String, String, i32)> = Vec::new();
        let Ok(mut stmt) = db.prepare_cached(
            "SELECT DISTINCT p.phrase, f.file_path, o.line FROM occurrence o
             JOIN phrases p ON p.id = o.phrase_id
             JOIN file_map f ON f.id = o.file_id
             WHERE o.is_def = 1 AND o.tag = 2 AND f.is_source = 1
             ORDER BY p.phrase, f.file_path LIMIT 500",
        ) else {
            return json!({"repo": repo, "questions": questions, "n_files": files.len()});
        };
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                let ph: String = r.get(0).unwrap_or_default();
                let fp: String = r.get(1).unwrap_or_default();
                let ln: i32 = r.get(2).unwrap_or(0);
                if files.contains(&fp) && is_ident(&ph) && ph.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
                    types.push((ph, fp, ln));
                }
            }
        }
        rng.shuffle(&mut types);
        for (ph, fp, ln) in &types {
            if used.contains(&ph.to_lowercase()) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(fp) else { continue };
            let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
            let fields = struct_fields(&lines, ph);
            if fields.len() >= 2 {
                used.insert(ph.to_lowercase());
                let gt: Vec<Value> = fields.iter().map(|(m, l)| json!({"sym": m, "file": basename(fp), "line": l + 1})).collect();
                questions.push(json!({
                    "query_id": "q4_fields",
                    "question": format!("List the fields of the struct `{}` (defined in {} at line {}).", ph, basename(fp), ln + 1),
                    "gt": gt,
                }));
                break;
            }
        }
    }

    // q5: dead code in a module.
    {
        let mut by_mod: std::collections::BTreeMap<String, Vec<(String, String, i32)>> =
            std::collections::BTreeMap::new();
        for (ph, fp, ln) in &funcs {
            if used.contains(&ph.to_lowercase()) {
                continue;
            }
            if nondef_count(&db, ph) == 0 {
                let mod_dir = fp.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
                by_mod.entry(mod_dir).or_default().push((ph.clone(), fp.clone(), *ln));
            }
        }
        if let Some((mod_dir, mut syms)) = by_mod.into_iter().max_by_key(|(_, v)| v.len()) {
            syms.sort();
            let sample: Vec<_> = syms.into_iter().take(5).collect();
            if !sample.is_empty() {
                let gt: Vec<Value> = sample.iter().map(|(s, f, l)| json!({"sym": s, "file": basename(f), "line": l + 1})).collect();
                questions.push(json!({
                    "query_id": "q5_dead",
                    "question": format!("Find pub functions in `{}/` that are never called anywhere in the workspace. List name + file:line for any you find.", mod_dir),
                    "gt": gt,
                }));
            }
        }
    }

    // q6: which structs derive/implement Default.
    {
        let mut impls: Vec<(String, String, i32)> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for fp in files.iter().filter(|f| f.ends_with(".rs")).take(400) {
            let Ok(content) = std::fs::read_to_string(fp) else { continue };
            let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
            for (i, t) in lines.iter().enumerate() {
                let tl = t.trim();
                if (tl.starts_with("impl Default for ") || tl.starts_with("impl std::default::Default for "))
                    && tl.contains('{')
                {
                    let name = tl
                        .trim_start_matches("impl ")
                        .trim_start_matches("std::default::Default for ")
                        .trim_start_matches("Default for ")
                        .split(['<', ' ', '{'])
                        .next()
                        .unwrap_or("")
                        .trim();
                    if is_ident(name) && seen.insert(name.to_lowercase()) {
                        impls.push((name.to_string(), fp.clone(), i as i32));
                    }
                } else if tl.starts_with("#[derive(") && tl.contains("Default") {
                    // Find the struct name on the next non-attribute line.
                    for t2 in lines.iter().skip(i + 1).take(3) {
                        let t2t = t2.trim();
                        if let Some(rest) = t2t.strip_prefix("pub struct ").or_else(|| t2t.strip_prefix("struct ")) {
                            let name = rest.split(['<', ' ', '{']).next().unwrap_or("").trim();
                            if is_ident(name) && seen.insert(name.to_lowercase()) {
                                impls.push((name.to_string(), fp.clone(), i as i32));
                            }
                            break;
                        }
                        if !t2t.starts_with('#') && !t2t.starts_with("///") {
                            break;
                        }
                    }
                }
            }
        }
        if !impls.is_empty() {
            let gt: Vec<Value> = impls.iter().take(8).map(|(n, f, l)| json!({"sym": n, "file": basename(f), "line": l + 1})).collect();
            questions.push(json!({
                "query_id": "q6_default_impls",
                "question": "Which structs in this codebase implement or derive the Default trait? List their names with file:line.",
                "gt": gt,
            }));
        }
    }

    json!({"repo": repo, "seed": seed, "n_files": files.len(), "questions": questions})
}

/// Score a bench results file against a GT file. Returns per-query and
/// aggregate precision/recall/F1 (claim-weighted, ±1 line tolerance).
pub fn score_results(results_path: &str, gt_path: &str, tol: i32) -> Value {
    let db = match results_db_index(results_path) {
        Ok(d) => d,
        Err(e) => return json!({"error": e}),
    };
    let gt_json: Value = match std::fs::read_to_string(gt_path).ok().and_then(|s| serde_json::from_str(&s).ok()) {
        Some(v) => v,
        None => return json!({"error": format!("cannot read GT: {}", gt_path)}),
    };
    let mut gt_map: std::collections::HashMap<String, Vec<GtFact>> = std::collections::HashMap::new();
    if let Some(qs) = gt_json.get("questions").and_then(|v| v.as_array()) {
        for q in qs {
            let qid = q.get("query_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut facts = Vec::new();
            if let Some(gts) = q.get("gt").and_then(|v| v.as_array()) {
                for g in gts {
                    let sym = g.get("sym").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let file = basename(g.get("file").and_then(|v| v.as_str()).unwrap_or(""));
                    // V74: `generate` already stores 1-indexed lines in the GT
                    // (it converts the 0-indexed occurrence line with +1 at
                    // generation time). Adding +1 here double-counted, which
                    // tol=1 masked but --tol 0 exposed.
                    let line = g.get("line").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    // Python's GT prose ("sym at file:line") yields TWO facts:
                    // the symbol form and the file-only form. Both participate
                    // in recall — the file-only form is what matches answers
                    // phrased "X is called from: - file:line".
                    facts.push(GtFact { sym: sym.clone(), file: file.clone(), line });
                    facts.push(GtFact { sym: String::new(), file, line });
                }
            }
            gt_map.insert(qid, facts);
        }
    }

    let results_text = match std::fs::read_to_string(results_path) {
        Ok(t) => t,
        Err(e) => return json!({"error": format!("read results: {}", e)}),
    };

    let mut total_claims = 0usize;
    let mut total_matched_claims = 0usize;
    let mut total_gt = 0usize;
    let mut total_gt_found = 0usize;
    let mut per_query: Vec<Value> = Vec::new();
    let mut by_cond: std::collections::BTreeMap<String, (usize, usize, usize, usize)> =
        std::collections::BTreeMap::new();

    for line in results_text.lines() {
        let Ok(rec) = serde_json::from_str::<Value>(line) else { continue };
        let cond = rec.get("cond").and_then(|v| v.as_str()).unwrap_or("A").to_string();
        let Some(queries) = rec.get("queries").and_then(|v| v.as_array()) else { continue };
        for q in queries {
            let qid = q.get("query_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let answer = q.get("answer").and_then(|v| v.as_str()).unwrap_or("");
            let claims = verify::extract_claims(answer);
            let mut matched = 0usize;
            for c in &claims {
                let verdict = crate::verify::verify_claim(&db, c, tol);
                if format!("{:?}", verdict).starts_with("Verified") {
                    matched += 1;
                }
            }
            let gt_facts = gt_map.get(&qid).cloned().unwrap_or_default();
            let mut gt_found = 0usize;
            for g in &gt_facts {
                // Symbol facts need symbol+file+line; file-only facts need
                // file+line (±tol). Parity with the Python verifier.
                let matched = if g.sym.is_empty() {
                    claims.iter().any(|c| c.file == g.file && (c.line - g.line).abs() <= tol)
                } else {
                    claims.iter().any(|c| {
                        c.symbol.eq_ignore_ascii_case(&g.sym)
                            && c.file == g.file
                            && (c.line - g.line).abs() <= tol
                    })
                };
                if matched {
                    gt_found += 1;
                }
            }
            total_claims += claims.len();
            total_matched_claims += matched;
            total_gt += gt_facts.len();
            total_gt_found += gt_found;
            let e = by_cond.entry(cond.clone()).or_insert((0, 0, 0, 0));
            e.0 += claims.len();
            e.1 += matched;
            e.2 += gt_facts.len();
            e.3 += gt_found;
            per_query.push(json!({
                "cond": cond, "query_id": qid,
                "claims": claims.len(), "verified": matched,
                "gt": gt_facts.len(), "gt_found": gt_found,
            }));
        }
    }

    let mut aggregates: Vec<Value> = Vec::new();
    for (cond, (cl, m, g, gf)) in &by_cond {
        let p = if *cl > 0 { *m as f64 / *cl as f64 } else { 0.0 };
        let r = if *g > 0 { *gf as f64 / *g as f64 } else { 0.0 };
        let f1 = if p + r > 0.0 { 2.0 * p * r / (p + r) } else { 0.0 };
        aggregates.push(json!({
            "cond": cond,
            "claims": *cl, "verified": *m,
            "gt": *g, "gt_found": *gf,
            "precision": (p * 1000.0).round() / 1000.0,
            "recall": (r * 1000.0).round() / 1000.0,
            "f1": (f1 * 1000.0).round() / 1000.0,
        }));
    }

    let p = if total_claims > 0 { total_matched_claims as f64 / total_claims as f64 } else { 0.0 };
    let r = if total_gt > 0 { total_gt_found as f64 / total_gt as f64 } else { 0.0 };
    let f1 = if p + r > 0.0 { 2.0 * p * r / (p + r) } else { 0.0 };
    json!({
        "aggregates": aggregates,
        "overall": {
            "claims": total_claims, "verified": total_matched_claims,
            "gt": total_gt, "gt_found": total_gt_found,
            "precision": (p * 1000.0).round() / 1000.0,
            "recall": (r * 1000.0).round() / 1000.0,
            "f1": (f1 * 1000.0).round() / 1000.0,
        },
        "per_query": per_query,
    })
}

/// Open the index next to a results file: results in bench/results/ => repo
/// root is two levels up + /.reliary. Also honours RELIARY_BENCH_INDEX.
fn results_db_index(results_path: &str) -> Result<Connection, String> {
    let db_path = if let Ok(p) = std::env::var("RELIARY_BENCH_INDEX") {
        p
    } else {
        let p = std::path::Path::new(results_path);
        let root = p
            .parent()
            .and_then(|r| r.parent())
            .and_then(|r| r.parent())
            .map(|r| r.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        root.join(".reliary/index.sqlite").to_string_lossy().to_string()
    };
    if !std::path::Path::new(&db_path).exists() {
        return Err(format!(
            "no index at {} — set RELIARY_BENCH_INDEX=/path/to/.reliary/index.sqlite",
            db_path
        ));
    }
    Connection::open(&db_path).map_err(|e| format!("open index: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcg_shuffle_is_deterministic() {
        let mut a: Vec<i32> = (0..20).collect();
        let mut b: Vec<i32> = (0..20).collect();
        Lcg::new(42).shuffle(&mut a);
        Lcg::new(42).shuffle(&mut b);
        assert_eq!(a, b, "same seed must produce same order");
        let mut c: Vec<i32> = (0..20).collect();
        Lcg::new(43).shuffle(&mut c);
        assert_ne!(a, c, "different seeds should differ");
    }

    #[test]
    fn lcg_shuffle_preserves_elements() {
        let mut v: Vec<i32> = (0..50).collect();
        Lcg::new(7).shuffle(&mut v);
        let mut sorted = v.clone();
        sorted.sort();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn empty_shuffle_noop() {
        let mut v: Vec<i32> = Vec::new();
        Lcg::new(1).shuffle(&mut v);
        assert!(v.is_empty());
    }

    #[test]
    fn struct_fields_depth_aware() {
        let lines: Vec<String> = vec![
            "pub struct Foo {".into(),
            "    pub name: String,".into(),
            "    pub count: u32,".into(),
            "}".into(),
            "pub fn after() { let x = 1; }".into(),
        ];
        let fields = struct_fields(&lines, "foo");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].0, "name");
        assert_eq!(fields[1].0, "count");
        assert_eq!(fields[0].1, 1); // 0-indexed line
    }

    #[test]
    fn struct_fields_skips_comments() {
        let lines: Vec<String> = vec![
            "struct Bar {".into(),
            "    /// doc comment: not a field".into(),
            "    pub real: i64,".into(),
            "}".into(),
        ];
        let fields = struct_fields(&lines, "bar");
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].0, "real");
    }

    #[test]
    fn struct_fields_missing_type_empty() {
        let lines: Vec<String> = vec!["pub struct Other {".into(), "    pub x: u8,".into(), "}".into()];
        assert!(struct_fields(&lines, "notpresent").is_empty());
    }

    #[test]
    fn basename_handles_paths() {
        assert_eq!(basename("/a/b/c.rs"), "c.rs");
        assert_eq!(basename("c.rs"), "c.rs");
        assert_eq!(basename(""), "");
    }
}
