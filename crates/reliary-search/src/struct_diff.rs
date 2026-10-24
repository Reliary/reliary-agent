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
    index_revision_impl(dir, db_path, false)
}

/// Like [`index_revision`], but `file_map.file_path` is rewritten to be
/// root-relative.
///
/// The indexer stores absolute canonical paths (`ingest::index_directory`).
/// A cache that outlives the directory it was built from must not: the same
/// content at a different root would otherwise produce absolute paths, and
/// every definition would look relocated. Warrant keys its cache on content
/// and reuses entries across roots, so it needs this variant.
pub fn index_revision_relative(dir: &str, db_path: &str) -> Result<Connection, String> {
    index_revision_impl(dir, db_path, true)
}

/// [`index_revision_relative`] with repo-relative paths excluded from indexing.
pub fn index_revision_relative_excluding(
    dir: &str,
    db_path: &str,
    exclude_rel: &std::collections::BTreeSet<String>,
) -> Result<Connection, String> {
    index_revision_impl_excluding(dir, db_path, true, exclude_rel)
}

fn index_revision_impl(dir: &str, db_path: &str, relative: bool) -> Result<Connection, String> {
    index_revision_impl_excluding(dir, db_path, relative, &std::collections::BTreeSet::new())
}

fn index_revision_impl_excluding(
    dir: &str,
    db_path: &str,
    relative: bool,
    exclude_rel: &std::collections::BTreeSet<String>,
) -> Result<Connection, String> {
    let _ = std::fs::remove_file(db_path);
    let _ = std::fs::remove_file(format!("{}-wal", db_path));
    let _ = std::fs::remove_file(format!("{}-shm", db_path));
    let db = Connection::open(db_path).map_err(|e| format!("open {}: {}", db_path, e))?;
    crate::schema::create_new_db(&db).map_err(|e| format!("schema: {}", e))?;
    crate::ingest::index_directory_excluding(&db, dir, exclude_rel)?;
    // Fresh indexes only have phrase_occ after trust — symbols live in the
    // occurrence table, which build_all_occurrence populates. Without this the
    // diff reports zero symbols.
    let built = crate::lazy_occurrence::build_all_occurrence(&db)
        .map_err(|e| format!("occurrence build: {}", e))?;
    if built == 0 {
        eprintln!("[struct_diff] warning: 0 occurrence rows indexed for {}", dir);
    }
    if relative {
        relativize_index(&db, dir)?;
    }
    Ok(db)
}

/// Rewrite `file_map.file_path` as a path relative to `root`.
///
/// Only `file_map` changes: `occurrence`, `file_phrases` and `file_stats` are
/// keyed by `file_id`, so the whole index follows. Idempotent — a path that
/// is already relative has no `root` prefix and is left alone.
pub fn relativize_index(db: &Connection, root: &str) -> Result<usize, String> {
    let root = std::fs::canonicalize(root)
        .map_err(|e| format!("cannot canonicalize {}: {}", root, e))?;
    let rows: Vec<(i64, String)> = {
        let mut stmt = db
            .prepare("SELECT id, file_path FROM file_map ORDER BY id")
            .map_err(|e| format!("prepare file_map: {}", e))?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| format!("query file_map: {}", e))?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(|e| format!("read file_map row: {}", e))?);
        }
        out
    };
    let mut changed = 0usize;
    {
        let mut upd = db
            .prepare("UPDATE file_map SET file_path = ?1 WHERE id = ?2")
            .map_err(|e| format!("prepare update: {}", e))?;
        for (id, path) in rows {
            let rel = match std::path::Path::new(&path).strip_prefix(&root) {
                Ok(p) => p.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            if rel != path {
                upd.execute(rusqlite::params![rel, id])
                    .map_err(|e| format!("update file_map: {}", e))?;
                changed += 1;
            }
        }
    }
    Ok(changed)
}

/// (name, rel-path) -> (display basename, first line). The dedup key uses the
/// last two path segments so same-named files in different crates don't
/// collide; the display keeps the basename + real line of the first sighting.
fn def_rows(db: &Connection) -> BTreeMap<(String, String), (String, i32)> {
    let mut out: BTreeMap<(String, String), (String, i32)> = BTreeMap::new();
    let mut stmt = db
        .prepare_cached(
            // V80: ORDER BY so the line/display recorded for a (symbol, file)
            // key is deterministic. Without it `or_insert` kept whichever row
            // SQLite yielded first, so reported lines/order varied run to run.
            "SELECT p.phrase, f.file_path, o.line FROM occurrence o
             JOIN phrases p ON p.id = o.phrase_id
             JOIN file_map f ON f.id = o.file_id
             WHERE o.is_def = 1 AND o.tag IN (1, 2) AND f.is_source = 1
             ORDER BY p.phrase, f.file_path, o.line",
        )
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Ok(Some(r)) = rows.next() {
        let phrase: String = r.get(0).unwrap_or_default();
        let path: String = r.get(1).unwrap_or_default();
        let line: i32 = r.get(2).unwrap_or(0);
        // V80: dedup per (symbol, file). Two segments collided for
        // `crates/a/src/foo.rs` vs `crates/b/src/foo.rs` (both "src/foo.rs"),
        // silently dropping a symbol and producing false add/remove. Use the
        // LAST THREE segments — still independent of the checkout root, but no
        // collision for the common `crates/<x>/src/<file>` layout.
        let segs: Vec<&str> = path.split('/').filter(|x| !x.is_empty()).collect();
        let rel = if segs.len() >= 3 {
            format!("{}/{}/{}", segs[segs.len() - 3], segs[segs.len() - 2], segs[segs.len() - 1])
        } else {
            segs.join("/")
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
            // V80: ORDER BY before the `.take(500)` cap — otherwise a different
            // 500 names were chosen per run, changing edges_added/removed for
            // identical inputs (breaking byte-identical determinism).
            "SELECT DISTINCT p.phrase FROM occurrence o
             JOIN phrases p ON p.id = o.phrase_id
             WHERE o.is_def = 1 AND o.tag = 1
             ORDER BY p.phrase",
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

    // ── V85 A5 P0: path relativization (cache prerequisite) ──────────────

    fn make_tree(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").unwrap();
    }

    #[test]
    fn relativize_makes_paths_root_relative() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        make_tree(&a);
        let db_path = dir.path().join("a.sqlite");
        let db = index_revision_relative(a.to_str().unwrap(), db_path.to_str().unwrap()).unwrap();
        let paths: Vec<String> = db
            .prepare("SELECT file_path FROM file_map ORDER BY file_path")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(paths, vec!["src/lib.rs".to_string()]);
    }

    /// The bug the cache would expose: the same content at a different root
    /// must resolve to the same relative paths. Without `relativize_index`
    /// the absolute paths differ and every definition looks relocated.
    #[test]
    fn relativized_index_survives_a_root_move() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        make_tree(&a);
        let db_a = index_revision_relative(
            a.to_str().unwrap(),
            dir.path().join("a.sqlite").to_str().unwrap(),
        )
        .unwrap();

        let b = dir.path().join("b");
        std::fs::create_dir_all(&b).unwrap();
        for e in walkdir::WalkDir::new(&a).into_iter().filter_map(|e| e.ok()) {
            if e.file_type().is_file() {
                let rel = e.path().strip_prefix(&a).unwrap();
                let dst = b.join(rel);
                std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
                std::fs::copy(e.path(), &dst).unwrap();
            }
        }
        let db_b = index_revision_relative(
            b.to_str().unwrap(),
            dir.path().join("b.sqlite").to_str().unwrap(),
        )
        .unwrap();

        let ra = crate::warrant::def_rows(&db_a, &a).unwrap();
        let rb = crate::warrant::def_rows(&db_b, &b).unwrap();
        assert_eq!(ra, rb, "same content at a different root must agree");
    }

    #[test]
    fn non_relativized_index_would_not_survive_a_move() {
        // Negative control for the test above: plain `index_revision` keeps
        // absolute paths, so the relative view still works (def_rows strips
        // the root) but the stored paths differ. This pins WHY the relative
        // variant exists: a cached DB is read from a root it was not built at.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        make_tree(&a);
        let db = index_revision(
            a.to_str().unwrap(),
            dir.path().join("plain.sqlite").to_str().unwrap(),
        )
        .unwrap();
        let stored: String = db
            .query_row("SELECT file_path FROM file_map LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert!(
            stored.starts_with('/'),
            "index_revision stores absolute paths; a cache must not"
        );
    }
}
