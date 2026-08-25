//! FTS5 Document Frequency as pseudo-perplexity for compression weighting.
//!
//! Grammar-free analog of LLM Lingua (Jiang et al. 2023): uses small LM
//! perplexity to drop predictable tokens. We use document frequency instead —
//! pure index lookup, no ML.
//!
//! Math:
//!   pp(token) ∝ -log(1 / DF(token))     // lower DF = higher perplexity
//!   info(line) = mean(log(DF(token) for token in identifiers(line)))
//!
//! High-DF tokens appear in many files (boilerplate, library names) → compress
//! Low-DF tokens appear in few files (project-specific) → preserve

use rusqlite::Connection;
use std::collections::HashMap;

use crate::scan_identifiers;

/// Per-project FTS5 DF index for compression scoring.
/// Open the same SQLite database used by search/ingest.
pub struct FtWeight {
    db: Connection,
    /// Cached phrase_id → DF count to avoid repeated lookups for the same
    /// token within a single compress_weighted() call.
    df_cache: HashMap<String, usize>,
    /// Total number of files in the index, for normalizing DF.
    /// Reserved for future use (relative-DF normalization across projects).
    #[allow(dead_code)]
    total_files: usize,
}

impl FtWeight {
    /// Open the FTS5 index database. Returns None if the DB is empty/missing.
    pub fn open(path: &str) -> Option<Self> {
        let db = Connection::open(path).ok()?;
        let total_files: usize = db
            .query_row("SELECT COUNT(*) FROM file_map", [], |r| r.get(0))
            .unwrap_or(0) as usize;
        if total_files == 0 {
            return None;
        }
        Some(Self {
            db,
            df_cache: HashMap::new(),
            total_files,
        })
    }

    /// Look up document frequency (number of files containing this identifier).
    /// Grammar-free: pure integer count, no stemming or similarity.
    pub fn df(&mut self, token: &str) -> usize {
        if let Some(&cached) = self.df_cache.get(token) {
            return cached;
        }
        // FTS5 index uses Porter stemming — look up the stem of the token.
        let stem = crate::porter_stem(token);
        let df: usize = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM phrase_occ po
                 JOIN phrases p ON p.id = po.phrase_id
                 WHERE p.phrase = ?1",
                rusqlite::params![stem],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize;
        self.df_cache.insert(token.to_string(), df);
        df
    }

    /// Compute information score for a single line.
    /// Score = mean(log(DF + 1)) across identifiers in the line.
    /// Higher score = more "predictable" (boilerplate) → safe to compress.
    /// Lower score = more "surprising" (project-specific) → preserve.
    pub fn line_info_score(&mut self, line: &str) -> f64 {
        let idents = scan_identifiers(line);
        if idents.is_empty() {
            return 0.0;
        }
        // Filter to idents with length ≥ 3 (avoid noise like "to", "a", "x")
        let significant: Vec<&String> = idents.iter().filter(|s| s.len() >= 3).collect();
        if significant.is_empty() {
            return 0.0;
        }
        let mut total = 0.0;
        for tok in &significant {
            let df = self.df(tok);
            // log(DF + 1) avoids log(0); bounded above by log(total_files + 1)
            total += ((df + 1) as f64).ln();
        }
        total / significant.len() as f64
    }

    /// Decide if a line should be PRESERVED verbatim.
    /// Threshold: log(10) ≈ 2.3 (tokens appearing in ≥10 files are "predictable").
    /// Lowered to log(5) ≈ 1.6 for real-world projects where 10+ is too strict.
    pub fn should_preserve(&mut self, line: &str) -> bool {
        let score = self.line_info_score(line);
        // Below threshold = "surprising" (low DF) = preserve
        score < 1.6
    }

    /// Compress content using DF weighting.
    /// Returns None if >70% of lines are signal (don't compress high-signal content).
    /// Returns Some(compressed) otherwise, with preserved lines verbatim and
    /// non-preserved lines passed to `compress_fn` for further compression.
    pub fn compress_weighted<F>(&mut self, content: &str, mut compress_fn: F) -> Option<String>
    where
        F: FnMut(&str) -> String,
    {
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return None;
        }

        let preserved: Vec<bool> = lines
            .iter()
            .map(|l| {
                let trimmed = l.trim();
                if trimmed.is_empty() {
                    return true; // preserve blank lines as-is
                }
                self.should_preserve(l)
            })
            .collect();

        let preserve_count = preserved.iter().filter(|&&p| p).count();
        let preserve_ratio = preserve_count as f64 / lines.len() as f64;

        // If >70% of lines are signal → too much to compress, return None
        if preserve_ratio > 0.70 {
            return None;
        }

        // Split content into preservable and compressible chunks.
        // Preserve blank lines verbatim, but group consecutive compressible lines
        // so the compress function can apply skeleton grouping etc.
        let mut result = String::new();
        let mut compress_buffer = String::new();
        let mut last_was_preserved = false;

        for (i, line) in lines.iter().enumerate() {
            if preserved[i] {
                // Flush compress buffer if any
                if !compress_buffer.is_empty() {
                    let compressed = compress_fn(&compress_buffer);
                    if !compressed.is_empty() {
                        result.push_str(compressed.trim_end());
                        result.push('\n');
                    }
                    compress_buffer.clear();
                }
                result.push_str(line);
                result.push('\n');
                last_was_preserved = true;
            } else {
                if !compress_buffer.is_empty() {
                    compress_buffer.push('\n');
                }
                compress_buffer.push_str(line);
                last_was_preserved = false;
            }
        }

        // Flush remaining compress buffer
        if !compress_buffer.is_empty() {
            let compressed = compress_fn(&compress_buffer);
            if !compressed.is_empty() {
                if !last_was_preserved {
                    result.push('\n');
                }
                result.push_str(compressed.trim_end());
            }
        }

        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_info_score_empty_line() {
        let mut fw = FtWeight {
            db: Connection::open_in_memory().unwrap(),
            df_cache: HashMap::new(),
            total_files: 0,
        };
        assert_eq!(fw.line_info_score(""), 0.0);
        assert_eq!(fw.line_info_score("   "), 0.0);
    }

    #[test]
    fn test_info_score_short_idents() {
        let mut fw = FtWeight {
            db: Connection::open_in_memory().unwrap(),
            df_cache: HashMap::new(),
            total_files: 0,
        };
        // No idents ≥ 3 chars long → score 0
        assert_eq!(fw.line_info_score("a b c"), 0.0);
    }

    #[test]
    fn test_info_score_uses_cache() {
        let mut fw = FtWeight {
            db: Connection::open_in_memory().unwrap(),
            df_cache: HashMap::new(),
            total_files: 100,
        };
        // Empty DB → all lookups return 0 → score = ln(1) = 0
        fw.df_cache.insert("test".to_string(), 5);
        assert_eq!(fw.df("test"), 5);
        // Cached, doesn't hit DB
    }

    #[test]
    fn test_should_preserve_low_score() {
        // score < 1.6 → preserve (project-specific)
        let mut fw = FtWeight {
            db: Connection::open_in_memory().unwrap(),
            df_cache: HashMap::new(),
            total_files: 100,
        };
        // Empty DB → all tokens get DF=0 → score = ln(0+1) = 0
        // score 0 < 1.6 → preserve
        assert!(fw.should_preserve("src/foo.rs:42"));
    }
}
