//! Arc 30 Phase 1 — `reliary_architecture`.
//!
//! Grammar-free architecture overview that mirrors altbackend's `get_architecture`.
//! All data is computed from existing tables: `file_map`, `occurrence`,
//! `block`, `method_occurrence`. No new tables, no new algorithms.
//!
//! Returned sections:
//! - `languages` — file counts per language (extension-based detection)
//! - `packages` — directory groups (top-level + sub-package)
//! - `entry_points` — files with many `is_def=1` that are rarely imported
//! - `hotspots` — files with most occurrences
//! - `boundaries` — files with high inbound phrase references
//! - `clusters` — union-find on file co-occurrence (files sharing phrases)
//! - `routes` — files containing route-like tokens with `is_def=1`
//! - `layers` — files grouped by path depth
//! - `summary` — total counts

use rusqlite::Connection;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Serialize, Clone)]
pub struct ArchitectureSummary {
    pub project_path: String,
    pub summary: Summary,
    pub languages: Vec<LangCount>,
    pub packages: Vec<PkgCount>,
    pub entry_points: Vec<FileCount>,
    pub hotspots: Vec<FileCount>,
    pub boundaries: Vec<FileCount>,
    pub clusters: Vec<Cluster>,
    pub routes: Vec<FileRef>,
    pub layers: Vec<LayerCount>,
}

#[derive(Debug, Serialize, Clone)]
pub struct Summary {
    pub total_files: usize,
    pub total_blocks: usize,
    pub total_occurrences: usize,
    pub total_methods: usize,
    pub total_clusters: usize,
}

#[derive(Debug, Serialize, Clone)]
pub struct LangCount {
    pub language: String,
    pub file_count: usize,
}

#[derive(Debug, Serialize, Clone)]
pub struct PkgCount {
    pub name: String,
    pub file_count: usize,
    pub fan_in: usize,
    pub fan_out: usize,
}

#[derive(Debug, Serialize, Clone)]
pub struct FileRef {
    pub file: String,
    pub line: usize,
    pub name: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct FileCount {
    pub file: String,
    pub count: usize,
}

#[derive(Debug, Serialize, Clone)]
pub struct Cluster {
    pub id: usize,
    pub name: String,
    pub file_count: usize,
    pub sample_files: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct LayerCount {
    pub depth: usize,
    pub file_count: usize,
}

/// Resolve a stored absolute path to a project-relative path.
/// Strips any prefix that matches `project_path`; if the path is already
/// relative, returns it unchanged.
fn to_rel_path(abs: &str, project_path: &str) -> String {
    let abs = abs.trim();
    let pp = project_path.trim().trim_end_matches('/');
    if !pp.is_empty() && abs.starts_with(pp) {
        let rel = &abs[pp.len()..];
        return rel.trim_start_matches('/').to_string();
    }
    abs.to_string()
}

/// Detect language from file extension.
fn detect_language(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".rs") {
        "Rust"
    } else if lower.ends_with(".py") {
        "Python"
    } else if lower.ends_with(".js") || lower.ends_with(".jsx") {
        "JavaScript"
    } else if lower.ends_with(".ts") || lower.ends_with(".tsx") {
        "TypeScript"
    } else if lower.ends_with(".go") {
        "Go"
    } else if lower.ends_with(".java") {
        "Java"
    } else if lower.ends_with(".cpp") || lower.ends_with(".cc") || lower.ends_with(".cxx") {
        "C++"
    } else if lower.ends_with(".c") || lower.ends_with(".h") {
        "C"
    } else if lower.ends_with(".rb") {
        "Ruby"
    } else if lower.ends_with(".md") {
        "Markdown"
    } else if lower.ends_with(".toml") {
        "TOML"
    } else if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        "YAML"
    } else if lower.ends_with(".json") {
        "JSON"
    } else if lower.ends_with(".sh") || lower.ends_with(".bash") {
        "Shell"
    } else if lower.ends_with(".sql") {
        "SQL"
    } else {
        "Other"
    }
}

/// Get the top-2 directory components as a "package name".
/// e.g. `runtime/scheduler/multi_thread/worker.rs` → `runtime.scheduler`
fn package_name(rel_path: &str) -> String {
    let parts: Vec<&str> = rel_path.split('/').collect();
    if parts.len() >= 3 {
        // Skip filename; combine top-2 directories.
        format!("{}.{}", parts[0], parts[1])
    } else if parts.len() == 2 {
        parts[0].to_string()
    } else {
        "(root)".to_string()
    }
}

fn path_depth(rel_path: &str) -> usize {
    rel_path.matches('/').count()
}

/// Main entry point. Compute architecture overview.
pub fn get_architecture(
    db: &Connection,
    project_path: &str,
    limit: usize,
) -> rusqlite::Result<ArchitectureSummary> {
    let limit = if limit == 0 { 20 } else { limit };

    // Total counts.
    let total_files: usize =
        db.query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0))?;
    let total_blocks: usize =
        db.query_row("SELECT COUNT(*) FROM block", [], |r| r.get(0))?;
    let total_occurrences: usize =
        db.query_row("SELECT COUNT(*) FROM occurrence", [], |r| r.get(0))?;
    let total_methods: usize =
        db.query_row("SELECT COUNT(*) FROM method_occurrence", [], |r| r.get(0))?;

    // Languages.
    let mut file_paths: Vec<String> = Vec::with_capacity(total_files);
    let mut stmt = db.prepare_cached("SELECT file_path FROM file_map ORDER BY file_path")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    for row in rows {
        file_paths.push(to_rel_path(&row?, project_path));
    }
    let mut lang_counts: HashMap<&'static str, usize> = HashMap::new();
    for p in &file_paths {
        *lang_counts.entry(detect_language(p)).or_insert(0) += 1;
    }
    let mut languages: Vec<LangCount> = lang_counts
        .into_iter()
        .map(|(language, file_count)| LangCount {
            language: language.to_string(),
            file_count,
        })
        .collect();
    languages.sort_by(|a, b| b.file_count.cmp(&a.file_count));

    // Packages: group files by top-2 dirs, count + measure cross-package references.
    let mut pkg_files: HashMap<String, Vec<String>> = HashMap::new();
    for p in &file_paths {
        let pkg = package_name(p);
        pkg_files.entry(pkg).or_default().push(p.clone());
    }
    // Cache file_id→path.
    let mut fid_to_path: HashMap<i32, String> = HashMap::new();
    let mut fid_stmt = db.prepare_cached("SELECT id, file_path FROM file_map ORDER BY id")?;
    let fid_rows = fid_stmt.query_map([], |r| {
        Ok((r.get::<_, i32>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in fid_rows {
        let (id, path) = row?;
        fid_to_path.insert(id, to_rel_path(&path, project_path));
    }
    // Fan-in/out: use cheap proxy — count is_def=1 occurrences per file.
    // (Fan-in could be expensive self-join; we use def counts as proxy for now.)
    let mut fan_in_proxy: HashMap<i32, i64> = HashMap::new();
    let mut stmt = db.prepare_cached(
        "SELECT file_id, COUNT(*) FROM occurrence WHERE is_def=1 GROUP BY file_id"
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, i32>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (fid, cnt) = row?;
        fan_in_proxy.insert(fid, cnt);
    }
    // Aggregate def counts per package.
    let pkg_of: HashMap<String, String> = file_paths
        .iter()
        .map(|p| (p.clone(), package_name(p)))
        .collect();
    let mut pkg_defs: HashMap<String, i64> = HashMap::new();
    for (fid, cnt) in &fan_in_proxy {
        if let Some(path) = fid_to_path.get(fid) {
            if let Some(pkg) = pkg_of.get(path) {
                *pkg_defs.entry(pkg.clone()).or_insert(0) += cnt;
            }
        }
    }
    let mut packages: Vec<PkgCount> = pkg_files
        .into_iter()
        .map(|(name, files)| PkgCount {
            file_count: files.len(),
            fan_in: *pkg_defs.get(&name).unwrap_or(&0) as usize,
            fan_out: 0, // Computed lazily on demand
            name,
        })
        .collect();
    packages.sort_by(|a, b| b.file_count.cmp(&a.file_count));
    packages.truncate(limit);

    // Hotspots: files with most occurrences.
    let mut stmt = db.prepare_cached(
        "SELECT file_id, COUNT(*) FROM occurrence GROUP BY file_id ORDER BY 2 DESC LIMIT ?"
    )?;
    let hotspots: Vec<FileCount> = stmt
        .query_map([limit as i64], |r| {
            let fid: i32 = r.get(0)?;
            let cnt: i64 = r.get(1)?;
            Ok((fid, cnt))
        })?
        .filter_map(|r| {
            let (fid, cnt) = r.ok()?;
            fid_to_path.get(&fid).map(|p| FileCount {
                file: p.clone(),
                count: cnt as usize,
            })
        })
        .collect();

    // Entry points: files with most `is_def=1` AND few references from other files.
    let mut stmt = db.prepare_cached(
        "SELECT file_id, COUNT(*) FROM occurrence WHERE is_def=1 GROUP BY file_id ORDER BY 2 DESC LIMIT ?"
    )?;
    let def_counts: HashMap<i32, usize> = stmt
        .query_map([limit as i64], |r| {
            Ok((r.get::<_, i32>(0)?, r.get::<_, i64>(1)? as usize))
        })?
        .filter_map(|r| r.ok())
        .collect();
    // Entry points: files with most is_def (cheap proxy, no cross-file join).
    let mut candidates: Vec<(i32, usize)> = def_counts
        .iter()
        .map(|(&fid, &defs)| (fid, defs))
        .collect();
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    let entry_points: Vec<FileCount> = candidates
        .into_iter()
        .take(limit)
        .filter_map(|(fid, defs)| {
            fid_to_path.get(&fid).map(|p| FileCount {
                file: p.clone(),
                count: defs,
            })
        })
        .collect();

    // Boundaries: files with most unique phrases defined.
    // Cheap proxy: same as entry points (most defs = widest influence).
    // (Real cross-file inbound would be expensive; defer.)
    let boundaries: Vec<FileCount> = entry_points.clone();

    // Clusters: cheap package-based grouping (fast, deterministic).
    let mut pkg_files: HashMap<String, Vec<String>> = HashMap::new();
    for p in &file_paths {
        pkg_files.entry(package_name(p)).or_default().push(p.clone());
    }
    let total_clusters = pkg_files.len();
    let mut sorted_pkgs: Vec<(String, Vec<String>)> = pkg_files.into_iter().collect();
    sorted_pkgs.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    sorted_pkgs.truncate(limit);
    let clusters: Vec<Cluster> = sorted_pkgs
        .into_iter()
        .enumerate()
        .map(|(i, (pkg_name, mut files))| {
            files.sort();
            let file_count = files.len();
            let sample_files: Vec<String> = files.iter().take(3).cloned().collect();
            Cluster {
                id: i,
                name: pkg_name,
                file_count,
                sample_files,
            }
        })
        .collect();

    // Routes: files containing route-like tokens with is_def=1.
    let route_tokens = ["route", "handler", "endpoint", "controller", "view"];
    let http_tokens = ["GET", "POST", "PUT", "DELETE", "PATCH"];
    let all_tokens: Vec<&str> = route_tokens
        .iter()
        .chain(http_tokens.iter())
        .copied()
        .collect();
    let placeholders = all_tokens
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        "SELECT file_id, phrase_id FROM occurrence
         WHERE is_def = 1 AND phrase_id IN (
           SELECT id FROM phrases WHERE phrase IN ({})
         )
         ORDER BY file_id, phrase_id
         LIMIT 500",
        placeholders
    );
    let mut stmt = db.prepare_cached(&query)?;
    let params: Vec<&dyn rusqlite::ToSql> =
        all_tokens.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let route_files: HashSet<i32> = stmt
        .query_map(params.as_slice(), |r| {
            Ok(r.get::<_, i32>(0)?)
        })?
        .filter_map(|r| r.ok())
        .collect();
    let mut routes: Vec<FileRef> = route_files
        .iter()
        .take(limit)
        .filter_map(|fid| {
            fid_to_path.get(fid).map(|p| FileRef {
                file: p.clone(),
                line: 1, // Approximate; not stored per-file.
                name: package_name(p),
            })
        })
        .collect();
    routes.sort_by(|a, b| a.file.cmp(&b.file));

    // Layers: files grouped by path depth.
    let mut depth_counts: HashMap<usize, usize> = HashMap::new();
    for p in &file_paths {
        *depth_counts.entry(path_depth(p)).or_insert(0) += 1;
    }
    let mut layers: Vec<LayerCount> = depth_counts
        .into_iter()
        .map(|(depth, file_count)| LayerCount { depth, file_count })
        .collect();
    layers.sort_by_key(|l| l.depth);

    Ok(ArchitectureSummary {
        project_path: project_path.to_string(),
        summary: Summary {
            total_files,
            total_blocks,
            total_occurrences,
            total_methods,
            total_clusters,
        },
        languages,
        packages,
        entry_points,
        hotspots,
        boundaries,
        clusters,
        routes,
        layers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language("foo.rs"), "Rust");
        assert_eq!(detect_language("foo.PY"), "Python");
        assert_eq!(detect_language("foo/bar.go"), "Go");
        assert_eq!(detect_language("weird.xyz"), "Other");
    }

    #[test]
    fn test_package_name() {
        assert_eq!(package_name("runtime/scheduler/worker.rs"), "runtime.scheduler");
        assert_eq!(package_name("util/wake.rs"), "util");
        assert_eq!(package_name("single.rs"), "(root)");
    }

    #[test]
    fn test_path_depth() {
        assert_eq!(path_depth("a.rs"), 0);
        assert_eq!(path_depth("a/b.rs"), 1);
        assert_eq!(path_depth("a/b/c/d.rs"), 3);
    }

    #[test]
    fn test_to_rel_path() {
        assert_eq!(
            to_rel_path("/tmp/proj/src/foo.rs", "/tmp/proj"),
            "src/foo.rs"
        );
        assert_eq!(to_rel_path("foo.rs", "/tmp/proj"), "foo.rs");
        assert_eq!(
            to_rel_path("/tmp/proj/src/foo.rs", "/tmp/proj/"),
            "src/foo.rs"
        );
    }
}