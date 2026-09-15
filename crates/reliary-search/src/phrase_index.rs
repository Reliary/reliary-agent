//! V56: in-memory phrase index — moves hot phrase lookups out of SQLite.
//!
//! The phrases table is ~14K rows (<1MB) — small enough to load into memory
//! once and search with SWAR/memchr (microseconds) instead of `LIKE '%x%'`
//! full-table scans (milliseconds). Used by:
//!   - search_fts5 term matching (was per-term LIKE)
//!   - closest_symbols recovery (was 5× LIKE scans)
//!   - late_interaction_rerank phrase scan (V55)
//!
//! SQLite stays the source of truth for writes; this is a read accelerator.
//! Staleness guard: if the index file's mtime is newer than the load time,
//! fall back to SQL (correctness over speed).

use rusqlite::Connection;

pub struct PhraseIndex {
    /// All phrases, loaded in id order. `id` is the position + 1... but ids
    /// may have gaps (deleted phrases), so keep parallel id vec.
    pub phrases: Vec<String>,
    pub ids: Vec<i64>,
    /// Indices into `phrases` sorted lexicographically (for prefix binary search).
    sorted: Vec<u32>,
    /// Index file mtime at load time (staleness guard).
    pub loaded_mtime: Option<std::time::SystemTime>,
}

impl PhraseIndex {
    pub fn load(db: &Connection, db_path: &str) -> Option<PhraseIndex> {
        let mut phrases = Vec::new();
        let mut ids = Vec::new();
        let mut stmt = db.prepare("SELECT id, phrase FROM phrases ORDER BY id").ok()?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).ok()?;
        for row in rows.flatten() {
            ids.push(row.0);
            phrases.push(row.1);
        }
        if phrases.is_empty() {
            return None;
        }
        let mut sorted: Vec<u32> = (0..phrases.len() as u32).collect();
        sorted.sort_by(|&a, &b| phrases[a as usize].cmp(&phrases[b as usize]));
        let loaded_mtime = std::fs::metadata(db_path).ok().and_then(|m| m.modified().ok());
        Some(PhraseIndex { phrases, ids, sorted, loaded_mtime })
    }

    /// Binary search for the first phrase >= prefix.
    fn lower_bound(&self, prefix: &str) -> usize {
        let mut lo = 0usize;
        let mut hi = self.sorted.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.phrases[self.sorted[mid] as usize].as_str() < prefix {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// All phrases starting with `prefix`, shortest-first (matches the SQL
    /// `ORDER BY LENGTH(phrase) ASC` semantics so suggestions are identical).
    pub fn find_prefix(&self, prefix: &str) -> Vec<&str> {
        let start = self.lower_bound(prefix);
        let mut out: Vec<&str> = Vec::new();
        for &idx in &self.sorted[start..] {
            let p = &self.phrases[idx as usize];
            if p.starts_with(prefix) {
                out.push(p.as_str());
            } else {
                break;
            }
        }
        out.sort_by_key(|p| p.len());
        out
    }

    /// All phrases containing `substr` (linear scan with a cheap length gate
    /// first — still far faster than SQLite LIKE because it's in-memory and
    /// we skip short phrases without scanning bytes), shortest-first.
    pub fn find_substring(&self, substr: &str) -> Vec<&str> {
        let mut out = Vec::new();
        if substr.is_empty() {
            return out;
        }
        let needle = substr.as_bytes();
        for p in &self.phrases {
            if p.len() >= needle.len() && p.contains(substr) {
                out.push(p.as_str());
            }
        }
        out.sort_by_key(|p| p.len());
        out
    }

    /// V57: closest phrases by edit distance (typo recovery). Scans all
    /// phrases with a length-gate first (|len_a - len_b| <= 3), computes
    /// Levenshtein, returns the `limit` closest, nearest-first.
    pub fn closest(&self, name: &str, limit: usize) -> Vec<(&str, usize)> {
        let mut best: Vec<(&str, usize)> = Vec::new();
        for p in &self.phrases {
            let lp = p.len();
            let ln = name.len();
            if lp.abs_diff(ln) > 3 {
                continue;
            }
            let d = crate::search::edit_distance(p, name);
            if d <= 3 {
                best.push((p.as_str(), d));
            }
        }
        best.sort_by_key(|&(_, d)| d);
        best.truncate(limit);
        best
    }

    /// Best fuzzy candidates for a misspelled/misspelled term: prefix matches
    /// first, then substring, then edit-distance <= 2 over those candidates.
    pub fn find_closest(&self, term: &str, limit: usize) -> Vec<(String, u8)> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<(String, u8)> = Vec::new();
        for p in self.find_prefix(term) {
            let s = p.to_string();
            if seen.insert(s.clone()) {
                out.push((s, 1));
            }
        }
        if out.len() >= limit {
            return out;
        }
        for p in self.find_substring(term) {
            let s = p.to_string();
            if seen.insert(s.clone()) {
                out.push((s, 2));
            }
        }
        if out.len() >= limit {
            return out;
        }
        // Edit-distance pass over the whole list (bounded by phrase count,
        // each distance check is O(len) — fine for 14K short phrases).
        for p in &self.phrases {
            if out.len() >= limit {
                break;
            }
            if p.len().abs_diff(term.len()) > 2 {
                continue;
            }
            if seen.contains(p) {
                continue;
            }
            if crate::search::edit_distance(p, term) <= 2 {
                seen.insert(p.clone());
                out.push((p.clone(), 3));
            }
        }
        out
    }
}

impl PhraseIndex {
    /// V58 P6a: candidate phrases for late-interaction rerank — substring
    /// matches capped at `limit`, as owned Strings (SQL IN-list friendly).
    pub fn substring_candidates(&self, term: &str, limit: usize) -> Vec<String> {
        self.find_substring(term).into_iter().take(limit).map(|s| s.to_string()).collect()
    }
}

#[allow(dead_code)]
fn global_available_impl(db: &Connection, db_path: &str) -> bool {
    get_phrase_index(db, db_path).is_some()
}

static CURRENT_DB_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn current_db_path() -> Option<String> {
    CURRENT_DB_PATH.get().cloned()
}

fn set_current_db_path(p: &str) {
    let _ = CURRENT_DB_PATH.set(p.to_string());
}

// Thread-local in-memory phrase index for the MCP server (CWD project).
thread_local! {
    static PHRASE_INDEX: std::cell::RefCell<Option<std::rc::Rc<PhraseIndex>>> = const { std::cell::RefCell::new(None) };
}

/// Get the in-memory phrase index, loading it on first use from the given
/// DB connection. Returns None if not loadable. Reloads when the index file's
/// mtime changes (staleness guard).
pub fn get_phrase_index(db: &Connection, db_path: &str) -> Option<std::rc::Rc<PhraseIndex>> {
    PHRASE_INDEX.with(|c| {
        let mut opt = c.borrow_mut();
        if let Some(idx) = opt.as_ref() {
            let fresh = std::fs::metadata(db_path).ok().and_then(|m| m.modified().ok());
            if idx.loaded_mtime == fresh {
                return Some(std::rc::Rc::clone(idx));
            }
        }
        set_current_db_path(db_path);
        if let Some(idx) = PhraseIndex::load(db, db_path) {
            let rc = std::rc::Rc::new(idx);
            *opt = Some(std::rc::Rc::clone(&rc));
            return Some(rc);
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        crate::schema::create_new_db(&db).unwrap();
        db.execute_batch(
            "INSERT INTO phrases (id, phrase) VALUES
               (1, 'consume'), (2, 'consumer'), (3, 'block_on'),
               (4, 'block_on_inner'), (5, 'spawn'), (6, 'spawning');",
        ).unwrap();
        db
    }

    #[test]
    fn prefix_search_finds_matches() {
        let db = test_db();
        let idx = PhraseIndex::load(&db, ":memory:").unwrap();
        let prefix = idx.find_prefix("block");
        assert!(prefix.contains(&"block_on"));
        assert!(prefix.contains(&"block_on_inner"));
        assert_eq!(prefix.len(), 2);
    }

    #[test]
    fn substring_search_finds_containing() {
        let db = test_db();
        let idx = PhraseIndex::load(&db, ":memory:").unwrap();
        let subs = idx.find_substring("consume");
        assert!(subs.contains(&"consume"));
        assert!(subs.contains(&"consumer"));
        assert_eq!(subs.len(), 2);
    }

    #[test]
    fn closest_handles_typo() {
        let db = test_db();
        let idx = PhraseIndex::load(&db, ":memory:").unwrap();
        let close = idx.find_closest("spwan", 5);
        assert!(close.iter().any(|(p, _)| p == "spawn"),
            "typo 'spwan' should suggest 'spawn', got {:?}", close);
    }
}
