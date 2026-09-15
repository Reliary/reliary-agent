//! V70 P4: `reliary diff <rev1> <rev2>` — structural diff.
//!
//! Indexes two revisions (git worktrees or plain directories) and reports
//! symbol-level changes: added/removed definitions, call-edge changes, and
//! new dead code. Deterministic: sorted output.

use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, Clone)]
pub struct StructDiff {
    pub symbols_added: Vec<(String, String, i32)>,   // (name, file, line)
    pub symbols_removed: Vec<(String, String, i32)>,
    pub edges_added: Vec<(String, String)>,          // (caller, callee)
    pub edges_removed: Vec<(String, String)>,
}

/// Build a fresh index for `dir` into `db_path` (removes any existing file).
pub fn index_revision(dir: &str, db_path: &str) -> Result<Connection, String> {
    let _ = std::fs::remove_file(db_path);
    let _ = std::fs::remove_file(format!("{}-wal", db_path));
    let _ = std::fs::remove_file(format!("{}-shm", db_path));
    let db = Connection::open(db_path).map_err(|e| format!("open {}: {}", db_path, e))?;
    crate::schema::create_new_db(&db).map_err(|e| format!("schema: {}", e))?;
    crate::ingest::index_directory(&db, dir)?;
    // Fresh indexes only have phrase_occ after trust — symbols live in the
    // occurrence table, which build_all_occurrence populates. Without this the
    // diff reports zero symbols.
    let built = crate::lazy_occurrence::build_all_occurrence(&db)
        .map_err(|e| format!("occurrence build: {}", e))?;
    if built == 0 {
        eprintln!("[struct_diff] warning: 0 occurrence rows indexed for {}", dir);
    }
    Ok(db)
}

/// (name, rel-path) -> (display basename, first line). The dedup key uses the
/// last two path segments so same-named files in different crates don't
/// collide; the display keeps the basename + real line of the first sighting.
fn def_rows(db: &Connection) -> BTreeMap<(String, String), (String, i32)> {
    let mut out: BTreeMap<(String, String), (String, i32)> = BTreeMap::new();
    let mut stmt = db
        .prepare_cached(
            "SELECT p.phrase, f.file_path, o.line FROM occurrence o
             JOIN phrases p ON p.id = o.phrase_id
             JOIN file_map f ON f.id = o.file_id
             WHERE o.is_def = 1 AND o.tag IN (1, 2) AND f.is_source = 1",
        )
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Ok(Some(r)) = rows.next() {
        let phrase: String = r.get(0).unwrap_or_default();
        let path: String = r.get(1).unwrap_or_default();
        let line: i32 = r.get(2).unwrap_or(0);
        // V74: dedup per (symbol, file) — keying on the line made pure line
        // moves show as add+remove, and basename-only keys collided across
        // crates. Use the last two path segments (enough to disambiguate
        // same-named files without depending on the checkout root).
        let segs: Vec<&str> = path.split('/').filter(|x| !x.is_empty()).collect();
        let rel = if segs.len() >= 2 {
            format!("{}/{}", segs[segs.len() - 2], segs[segs.len() - 1])
        } else {
            segs.last().copied().unwrap_or(&path).to_string()
        };
        let display = path.rsplit('/').next().unwrap_or(&path).to_string();
        out.entry((phrase, rel)).or_insert((display, line));
    }
    out
}

/// Call edges: (caller_def_name, callee_name) via callee extraction per def.
fn edge_rows(db: &Connection) -> BTreeSet<(String, String)> {
    let mut out: BTreeSet<(String, String)> = BTreeSet::new();
    // For each def, extract call patterns from its body via callgraph.
    let mut stmt = db
        .prepare_cached(
            "SELECT DISTINCT p.phrase FROM occurrence o
             JOIN phrases p ON p.id = o.phrase_id
             WHERE o.is_def = 1 AND o.tag = 1",
        )
        .unwrap();
    let names: Vec<String> = {
        let mut rows = stmt.query([]).unwrap();
        let mut v = Vec::new();
        while let Ok(Some(r)) = rows.next() {
            if let Ok(n) = r.get::<_, String>(0) {
                v.push(n);
            }
        }
        v
    };
    for name in names.iter().take(500) {
        if let Ok(cg) = crate::callgraph_v2::build_call_graph(db, name, ".", None, 1) {
            for c in &cg.callees {
                out.insert((name.clone(), c.name.clone()));
            }
        }
    }
    out
}

/// Compute the structural diff between two indexed revisions.
pub fn compute_diff(db_a: &Connection, db_b: &Connection) -> StructDiff {
    let defs_a = def_rows(db_a);
    let defs_b = def_rows(db_b);
    let mut diff = StructDiff::default();

    for (k, v) in defs_b.iter() {
        if !defs_a.contains_key(k) {
            diff.symbols_added.push((k.0.clone(), v.0.clone(), v.1));
        }
    }
    for (k, v) in defs_a.iter() {
        if !defs_b.contains_key(k) {
            diff.symbols_removed.push((k.0.clone(), v.0.clone(), v.1));
        }
    }

    let edges_a = edge_rows(db_a);
    let edges_b = edge_rows(db_b);
    for e in &edges_b {
        if !edges_a.contains(e) {
            diff.edges_added.push(e.clone());
        }
    }
    for e in &edges_a {
        if !edges_b.contains(e) {
            diff.edges_removed.push(e.clone());
        }
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_dbs_produce_empty_diff() {
        let a = Connection::open_in_memory().unwrap();
        let b = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&a).unwrap();
        crate::schema::create_new_db(&b).unwrap();
        let d = compute_diff(&a, &b);
        assert!(d.symbols_added.is_empty());
        assert!(d.symbols_removed.is_empty());
        assert!(d.edges_added.is_empty());
        assert!(d.edges_removed.is_empty());
    }

    #[test]
    fn added_def_reported() {
        let a = Connection::open_in_memory().unwrap();
        let b = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&a).unwrap();
        crate::schema::create_new_db(&b).unwrap();
        b.execute_batch(
            "INSERT INTO phrases (phrase) VALUES ('newfn');
             INSERT INTO file_map (file_path, is_source) VALUES ('/x/src/a.rs', 1);
             INSERT INTO occurrence (phrase_id, file_id, line, col, is_def, block_id, tag)
               VALUES (1, 1, 5, 0, 1, 0, 1);",
        )
        .unwrap();
        let d = compute_diff(&a, &b);
        assert_eq!(d.symbols_added.len(), 1);
        assert_eq!(d.symbols_added[0].0, "newfn");
        assert_eq!(d.symbols_added[0].1, "a.rs");
        assert_eq!(d.symbols_added[0].2, 5);
    }

    #[test]
    fn removed_def_reported() {
        let a = Connection::open_in_memory().unwrap();
        let b = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&a).unwrap();
        crate::schema::create_new_db(&b).unwrap();
        a.execute_batch(
            "INSERT INTO phrases (phrase) VALUES ('oldfn');
             INSERT INTO file_map (file_path, is_source) VALUES ('/x/src/b.rs', 1);
             INSERT INTO occurrence (phrase_id, file_id, line, col, is_def, block_id, tag)
               VALUES (1, 1, 9, 0, 1, 0, 1);",
        )
        .unwrap();
        let d = compute_diff(&a, &b);
        assert_eq!(d.symbols_removed.len(), 1);
        assert_eq!(d.symbols_removed[0].0, "oldfn");
    }
}
