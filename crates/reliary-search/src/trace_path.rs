//! Arc 30 Phase 2 — `reliary_trace_path`.
//!
//! Grammar-free call graph traversal. Walks the co-occurrence graph to find
//! callers (inbound) and callees (outbound) for a given symbol up to a depth.
//!
//! Algorithm (cheap, deterministic):
//! - **Outbound (callees)**: phrases used in anchor's block whose `is_def=1`
//!   elsewhere are candidate callees. Group by file, score by phrase-co-occurrence count.
//! - **Inbound (callers)**: occurrences of anchor symbol NOT is_def in OTHER blocks
//!   are caller candidates. Group by file.
//! - **Multi-hop**: extend outward from found nodes (visited set dedupe, depth cap).

use rusqlite::Connection;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Serialize, Clone)]
pub struct TraceNode {
    pub symbol: String,
    pub file: String,
    pub line: usize,
    pub hop: usize,
    pub score: f32,
}

#[derive(Debug, Serialize, Clone)]
pub struct TraceResult {
    pub anchor: AnchorInfo,
    pub direction: String,
    pub depth_used: usize,
    pub callers: Vec<TraceNode>,
    pub callees: Vec<TraceNode>,
    /// Populated when the symbol couldn't be resolved; explains the gap.
    /// Empty string when the trace completed normally.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct AnchorInfo {
    pub symbol: String,
    pub file: String,
    pub line: usize,
}

pub fn trace_path(
    db: &Connection,
    anchor_symbol: &str,
    anchor_file: &str,
    anchor_line: i32,
    direction: &str,
    depth: usize,
    project_path: &str,
) -> rusqlite::Result<TraceResult> {
    let depth = depth.clamp(1, 4);

    // Resolve anchor file_id — prefer exact match, fall back to shortest suffix match.
    let anchor_fid: i32 = db.query_row(
        "SELECT id FROM file_map WHERE file_path = ?1",
        rusqlite::params![anchor_file],
        |r| r.get(0),
    ).unwrap_or_else(|_| {
        // Fallback: suffix match, preferring shorter paths.
        let escaped = anchor_file.replace('%', r"\%").replace('_', r"\_");
        let mut stmt = match db.prepare_cached(
            "SELECT id, file_path FROM file_map WHERE file_path LIKE ?1 ESCAPE '\\' ORDER BY LENGTH(file_path) ASC LIMIT 1"
        ) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        match stmt.query_row(rusqlite::params![format!("%{}", escaped)], |r| r.get(0)) {
            Ok(id) => id,
            Err(rusqlite::Error::QueryReturnedNoRows) => 0,
            Err(e) => { eprintln!("trace_path file_id: {}", e); 0 }
        }
    });
    let stem = crate::porter_stem(anchor_symbol);
    let mut phrase_id: i32 = match db.query_row(
        "SELECT id FROM phrases WHERE phrase = ?1",
        [&stem],
        |r| r.get(0),
    ) {
        Ok(id) => id,
        Err(rusqlite::Error::QueryReturnedNoRows) => 0,
        Err(e) => { eprintln!("trace_path phrase_id: {}", e); 0 }
    };

    // W2: fallback when stem match fails — try unstemmed literal, then substring.
    if phrase_id == 0 {
        phrase_id = match db.query_row(
            "SELECT id FROM phrases WHERE phrase = ?1",
            [anchor_symbol.to_ascii_lowercase()],
            |r| r.get(0),
        ) {
            Ok(id) => id,
            Err(rusqlite::Error::QueryReturnedNoRows) => 0,
            Err(e) => { eprintln!("trace_path fallback phrase_id: {}", e); 0 }
        };
    }
    if phrase_id == 0 {
        let escaped = anchor_symbol.replace('%', r"\%").replace('_', r"\_");
        let mut stmt = match db.prepare_cached(
            "SELECT id FROM phrases WHERE phrase LIKE ?1 ESCAPE '\\' LIMIT 1"
        ) {
            Ok(s) => s,
            Err(_) => return Ok(TraceResult::empty_error(anchor_symbol, anchor_file, anchor_line, direction, depth, "phrase lookup failed")),
        };
        if let Ok(r) = stmt.query_row(
            rusqlite::params![format!("%{}%", escaped)],
            |r| r.get::<_, i32>(0),
        ) {
            phrase_id = r;
        }
    }

    // Helper: resolve file_id → relative path. V54: LAZY — query each file_id
    // on first use instead of loading ALL of file_map up front.
    let prefix = format!("{}/", project_path.trim_end_matches('/'));
    let mut fid_to_path: HashMap<i32, String> = HashMap::new();
    let mut path_stmt = db.prepare_cached("SELECT file_path FROM file_map WHERE id = ?1")?;
    let mut resolve_path = |fid: i32, map: &mut HashMap<i32, String>| -> Option<String> {
        if let Some(p) = map.get(&fid) {
            return Some(p.clone());
        }
        let raw: Option<String> = path_stmt
            .query_row(rusqlite::params![fid], |r| r.get(0))
            .ok();
        let rel = raw.map(|p| match p.strip_prefix(&prefix) {
            Some(s) => s.to_string(),
            None => p,
        });
        if let Some(rel) = &rel {
            map.insert(fid, rel.clone());
        }
        rel
    };

    let mut callers: Vec<TraceNode> = Vec::new();
    let mut callees: Vec<TraceNode> = Vec::new();

    if anchor_fid == 0 {
        return Ok(TraceResult::empty_error(
            anchor_symbol, anchor_file, anchor_line, direction, depth,
            &format!("file '{}' not found in index", anchor_file),
        ));
    }
    if phrase_id == 0 {
        return Ok(TraceResult::empty_error(
            anchor_symbol, anchor_file, anchor_line, direction, depth,
            &format!("symbol '{}' not indexed at {}:{} (structural classifier may have missed it)",
                anchor_symbol, anchor_file, anchor_line),
        ));
    }
    {
        // Callers (inbound): occurrences of anchor phrase in OTHER files where NOT is_def.
        // Group by file, take min line as representative.
        if direction == "inbound" || direction == "both" {
            let mut stmt = db.prepare_cached(
                "SELECT o.file_id, MIN(o.line), COUNT(*)
                 FROM occurrence o
                 WHERE o.phrase_id = ?1 AND o.is_def = 0 AND o.file_id != ?2
                 GROUP BY o.file_id
                 ORDER BY 3 DESC
                 LIMIT 50"
            )?;
            let rows = stmt.query_map(
                rusqlite::params![phrase_id, anchor_fid],
                |r| Ok((r.get::<_, i32>(0)?, r.get::<_, i32>(1)?, r.get::<_, i64>(2)?))
            )?;
            for row in rows {
                let (fid, line, count) = row?;
                if let Some(path) = resolve_path(fid, &mut fid_to_path) {
                    callers.push(TraceNode {
                        symbol: anchor_symbol.to_string(),
                        file: path.clone(),
                        line: (line + 1) as usize, // 1-based for display
                        hop: 1,
                        score: count as f32,
                    });
                }
            }
        }

        // Callees (outbound): phrases co-occurring in anchor file's blocks where the
        // phrase is is_def=1 ELSEWHERE. For each such candidate phrase, return the
        // definition site.
        if direction == "outbound" || direction == "both" {
            // Find blocks in anchor file (use anchor_line to find closest block).
            let block_id: i32 = db.query_row(
                "SELECT block_id FROM occurrence
                 WHERE file_id = ?1 AND phrase_id = ?2 AND ABS(line - ?3) < 500
                 ORDER BY ABS(line - ?3) LIMIT 1",
                rusqlite::params![anchor_fid, phrase_id, anchor_line],
                |r| r.get(0),
            ).unwrap_or(0);

            if block_id > 0 {
                // Distinct phrase_ids in anchor's block.
                let mut stmt = db.prepare_cached(
                    "SELECT DISTINCT phrase_id FROM occurrence WHERE block_id = ?1 AND phrase_id != ?2"
                )?;
                let rows = stmt.query_map(
                    rusqlite::params![block_id, phrase_id],
                    |r| r.get::<_, i32>(0)
                )?;
                let mut other_phrases: Vec<i32> = Vec::new();
                for r in rows {
                    if let Ok(p) = r { other_phrases.push(p); }
                    if other_phrases.len() >= 200 { break; }
                }

                if !other_phrases.is_empty() {
                    // Arc 31 Phase F: single batched IN-query instead of N round-trips.
                    // Find is_def=1 occurrences elsewhere for ALL anchor-block phrases.
                    let placeholders = other_phrases.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                    let sql = format!(
                        "SELECT o.file_id, MIN(o.line), p.phrase, COUNT(*)
                         FROM occurrence o JOIN phrases p ON o.phrase_id = p.id
                         WHERE o.phrase_id IN ({}) AND o.is_def = 1 AND o.file_id != ?
                         GROUP BY o.file_id, p.phrase
                         ORDER BY 4 DESC
                         LIMIT 50",
                        placeholders
                    );
                    let mut params: Vec<Box<dyn rusqlite::ToSql>> = other_phrases
                        .iter()
                        .map(|p| Box::new(*p) as Box<dyn rusqlite::ToSql>)
                        .collect();
                    params.push(Box::new(anchor_fid));
                    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
                    let mut stmt2 = db.prepare_cached(&sql)?;
                    let rows = stmt2.query_map(param_refs.as_slice(), |r| {
                        Ok((
                            r.get::<_, i32>(0)?,
                            r.get::<_, i32>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)?,
                        ))
                    })?;
                    for row in rows {
                        let (fid, line, sym, count) = row?;
                        if let Some(path) = resolve_path(fid, &mut fid_to_path) {
                            callees.push(TraceNode {
                                symbol: sym,
                                file: path.clone(),
                                line: (line + 1) as usize,
                                hop: 1,
                                score: count as f32,
                            });
                        }
                    }
                }
            }
        }
    }


    // Multi-hop extension (cheap, no recursive descent — just add 1 more layer).
    if depth >= 2 && direction != "inbound" {
        // For top-3 callees, find their own callees (hop=2).
        let top_callees: Vec<_> = callees.iter().take(3).cloned().collect();
        let mut extra_callees: Vec<TraceNode> = Vec::new();
        for cal in &top_callees {
            // Find any phrase_id for cal.symbol.
            let cal_pid: i32 = db.query_row(
                "SELECT id FROM phrases WHERE phrase = ?1",
                [&cal.symbol],
                |r| r.get(0),
            ).unwrap_or(0);
            if cal_pid == 0 { continue; }
            // Get is_def=1 elsewhere.
            let mut stmt = db.prepare_cached(
                "SELECT o.file_id, MIN(o.line), COUNT(*)
                 FROM occurrence o
                 WHERE o.phrase_id = ?1 AND o.is_def = 1
                 GROUP BY o.file_id
                 ORDER BY 3 DESC
                 LIMIT 5"
            )?;
            let rows = stmt.query_map([cal_pid], |r| {
                Ok((r.get::<_, i32>(0)?, r.get::<_, i32>(1)?, r.get::<_, i64>(2)?))
            })?;
            for row in rows {
                let (fid, line, count) = row?;
                if let Some(path) = resolve_path(fid, &mut fid_to_path) {
                    extra_callees.push(TraceNode {
                        symbol: cal.symbol.clone(),
                        file: path.clone(),
                        line: (line + 1) as usize,
                        hop: 2,
                        score: count as f32 * 0.5, // decay
                    });
                }
            }
        }
        // Dedupe by (symbol, file).
        let mut seen: HashSet<(String, String)> = callees.iter().map(|n| (n.symbol.clone(), n.file.clone())).collect();
        for n in extra_callees {
            let k = (n.symbol.clone(), n.file.clone());
            if seen.insert(k) {
                callees.push(n);
            }
        }
    }

    // Dedupe callers by (symbol, file).
    let mut seen: HashSet<(String, String)> = HashSet::new();
    callers.retain(|n| seen.insert((n.symbol.clone(), n.file.clone())));

    Ok(TraceResult {
        anchor: AnchorInfo {
            symbol: anchor_symbol.to_string(),
            file: anchor_file.to_string(),
            line: anchor_line as usize,
        },
        direction: direction.to_string(),
        depth_used: depth,
        callers,
        callees,
        error: None,
    })
}

impl TraceResult {
    /// Build an empty result with an error message — used when the symbol
    /// can't be resolved (not in index, or stem/literal/substring all miss).
    pub fn empty_error(
        anchor_symbol: &str, anchor_file: &str, anchor_line: i32,
        direction: &str, depth: usize, msg: &str,
    ) -> Self {
        TraceResult {
            anchor: AnchorInfo {
                symbol: anchor_symbol.to_string(),
                file: anchor_file.to_string(),
                line: anchor_line as usize,
            },
            direction: direction.to_string(),
            depth_used: depth,
            callers: vec![],
            callees: vec![],
            error: Some(msg.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_node_serialize() {
        let n = TraceNode {
            symbol: "spawn".into(),
            file: "src/runtime.rs".into(),
            line: 42,
            hop: 1,
            score: 5.0,
        };
        let s = serde_json::to_string(&n).unwrap();
        assert!(s.contains("spawn"));
        assert!(s.contains("src/runtime.rs"));
    }
}