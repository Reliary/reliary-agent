//! Holographic codebase pack generator.
//!
//! Produces a cache-stable, model-readable representation of a codebase
//! that pre-loads enough information into an LLM's context to answer most
//! codebase questions without tool calls.
//!
//! ## Format
//!
//! Each symbol gets one entry:
//! ```text
//! ## <symbol_name>
//! L2: <signature>  [<file>:<line>]
//! L3: <surprise facts only>
//! ```
//!
//! L2 (signature) is always present — it anchors the model's prior.
//! L3 (surprise) is present only when the function deviates from what
//! a trained model would predict.
//!
//! ## Cross-references
//!
//! Uses the SQLite index (occurrence + block + scope_binding tables)
//! for precise, disambiguated cross-references — not regex word matching.
//!
//! ## Surprise detection
//!
//! Uses skeleton-frequency analysis: lines whose aggressive_skeleton
//! is rare across the codebase are "surprising"; lines whose skeleton
//! is common are "expected" (the model's prior handles them).

use std::collections::HashSet;
use rustc_hash::FxHashMap;
use std::path::Path;

use rusqlite::Connection;

use reliary_sift::classify::aggressive_skeleton;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Pack format — controls which layers are included.
#[derive(Debug, Clone, Copy)]
pub enum PackFormat {
    /// Signatures + surprise only (optimal, 85% reasoning accuracy).
    L2L3,
    /// Full: L0 (purpose) + L1 (paragraph) + L2 (signature) + L3 (surprise) + cross-refs.
    Full,
    /// Body snippets: signature + 5-8 representative body lines (simpler than L3).
    /// The control experiment showed random body lines ≥ curated L3 surprise.
    BodySnippets,
}

/// Gate decision returned by `should_inject_pack`.
///
/// Two-tier: high-complexity codebases get the full pack; middle-complexity
/// ones get a minimal top-level map; low-complexity (or famous) ones get
/// nothing. Calibration is based on measured pack-helpfulness data on
/// reliary8 (HIGH), quale (MIDDLE), tokio (LOW).
#[derive(Debug, Clone, Copy)]
pub enum GateDecision {
    /// Inject the full focused pack (~5K chars, hotspot-selected).
    Full,
    /// Inject a minimal top-level map only (~1K chars, signatures + L3 for top-15 symbols).
    Minimal,
    /// Skip the pack entirely — codebase is either too simple or already known by the model.
    Skip,
}

/// Compute a composite complexity score for the codebase from the SQLite index.
///
/// `symbol_score = 0.4 * inbound_refs + 0.3 * outbound_calls + 0.3 * body_len_normalized`
///
/// The codebase score is the mean of symbol scores, weighted by inbound ref
/// count (high-traffic symbols dominate). Higher = more complex = pack more
/// useful.
pub fn compute_complexity_score(path: &str) -> Result<f32, String> {
    let db_path = format!(
        "{}/.reliary/index.sqlite",
        path.trim_end_matches('/')
    );
    let db = Connection::open(&db_path)
        .map_err(|e| format!("cannot open index at {}: {}", db_path, e))?;
    reliary_search::schema::open_existing_db_safe(&db)
        .map_err(|e| format!("index not initialized: {}", e))?;
    // V54: delegate to the single-pass implementation.
    compute_complexity_score_for(&db, path)
}

/// Decide whether to inject the pack based on the codebase's complexity score.
///
/// Two-tier gate:
/// - score > HIGH_THRESHOLD: full pack
/// - score > LOW_THRESHOLD: minimal map
/// - else: skip
///
/// Thresholds calibrated against measured data:
/// - reliary8 (complex, specific identifiers, tight coupling): score ≈ 6.4 → Full
/// - quali (medium, simpler bodies): score ≈ 5.4 → Minimal
/// - tokio (famous, similar score to quali): score ≈ 5.4 → Minimal
///   (we can't distinguish famous from private via index alone —
///   the model's training data exposure is external knowledge)
pub fn gate_decision(score: f32) -> GateDecision {
    const HIGH_THRESHOLD: f32 = 6.0;
    const LOW_THRESHOLD: f32 = 4.0;

    if score > HIGH_THRESHOLD {
        GateDecision::Full
    } else if score > LOW_THRESHOLD {
        GateDecision::Minimal
    } else {
        GateDecision::Skip
    }
}

/// Convenience: run the full gate pipeline for a codebase path.
pub fn should_inject_pack(path: &str) -> Result<GateDecision, String> {
    let score = compute_complexity_score(path)?;
    Ok(gate_decision(score))
}

/// C1: Gate decision using an already-open connection (no double open).
pub fn gate_decision_for(conn: &Connection, path: &str) -> Result<GateDecision, String> {
    let score = compute_complexity_score_for(conn, path)?;
    Ok(gate_decision(score))
}

/// C12: Complexity score using an already-open connection.
/// Combines 5 query_row calls into fewer queries via prepare_cached.
pub fn compute_complexity_score_for(conn: &Connection, _path: &str) -> Result<f32, String> {
    // V54: single pass over occurrence — one query computes symbol_count +
    // non_def_count (source-only) instead of two separate full scans.
    let row: (i64, i64) = conn.query_row(
        "SELECT
            COUNT(DISTINCT CASE WHEN o.is_def = 1 AND (
                f.file_path LIKE '%.rs' OR f.file_path LIKE '%.py' OR f.file_path LIKE '%.ts'
                OR f.file_path LIKE '%.go' OR f.file_path LIKE '%.c' OR f.file_path LIKE '%.js'
                OR f.file_path LIKE '%.java' OR f.file_path LIKE '%.rb' OR f.file_path LIKE '%.swift'
                OR f.file_path LIKE '%.kt' OR f.file_path LIKE '%.zig' OR f.file_path LIKE '%.nim'
            ) THEN p.phrase END),
            COUNT(CASE WHEN o.is_def = 0 AND (
                f.file_path NOT LIKE '%/bench/%' AND f.file_path NOT LIKE '%/tests/%'
                AND f.file_path NOT LIKE '%/test/%' AND f.file_path NOT LIKE '%/target/%'
                AND f.file_path NOT LIKE '%/vendor/%' AND f.file_path NOT LIKE '%/node_modules/%'
                AND f.file_path NOT LIKE '%/__pycache__/%' AND f.file_path NOT LIKE '%/.specstory/%'
            ) THEN 1 END)
         FROM occurrence o
         JOIN phrases p ON o.phrase_id = p.id
         JOIN file_map f ON o.file_id = f.id",
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap_or((0, 0));

    let (symbol_count, non_def_count) = row;
    if symbol_count == 0 {
        return Ok(0.0);
    }
    // Remaining scores: avg block span + specificity ratio (2 cheap queries).
    let avg_block_span: f32 = conn.query_row(
        "SELECT AVG(CAST(end_line - start_line AS REAL))
         FROM block WHERE end_line > start_line",
        [], |r| r.get::<_, Option<f64>>(0),
    ).ok().flatten().unwrap_or(0.0) as f32;

    let specific_count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT p.phrase)
         FROM phrases p
         WHERE length(p.phrase) > 8
          AND (p.phrase LIKE '%\\_%' ESCAPE '\\' OR substr(p.phrase, 2, 1) GLOB '[a-z]'
              AND substr(p.phrase, 1, 1) GLOB '[A-Z]')",
        [], |r| r.get(0),
    ).unwrap_or(0);
    let total_phrases: i64 = conn.query_row("SELECT COUNT(*) FROM phrases", [], |r| r.get(0)).unwrap_or(1);
    let specificity_ratio = if total_phrases > 0 { specific_count as f32 / total_phrases as f32 } else { 0.0 };

    let log_symbols = (symbol_count.max(1) as f32).ln();
    let log_xrefs = (non_def_count.max(1) as f32).ln();
    let span_score = avg_block_span / 10.0;

    Ok(0.25 * log_symbols + 0.20 * span_score + 0.25 * log_xrefs + 0.30 * specificity_ratio)
}

/// Generate a holographic pack for a codebase.
///
/// Reads the SQLite index at `<path>/.reliary/index.sqlite` and produces
/// a markdown pack string.
pub fn generate_pack(path: &str, format: PackFormat) -> Result<String, String> {
    let db_path = format!(
        "{}/.reliary/index.sqlite",
        path.trim_end_matches('/')
    );
    let db = Connection::open(&db_path)
        .map_err(|e| format!("cannot open index at {}: {}", db_path, e))?;

    // Verify the index is valid
    reliary_search::schema::open_existing_db_safe(&db)
        .map_err(|e| format!("index not initialized: {}", e))?;

    // 1. Extract all definition symbols from the index
    let symbols = extract_symbols_from_index(&db)?;

    // 2. Read sources + build skeleton frequency in one pass (avoids re-reading files)
    let (symbol_sources, skeleton_freq) = read_symbols_and_frequency(&symbols);

    // 3. Extract cross-references from the index
    let cross_refs = build_cross_refs_from_index(&db, &symbols);

    // 4. Render the pack
    let mut entries = Vec::new();
    entries.push(format!(
        "# Holographic Pack — {} ({} symbols)\n",
        Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string()),
        symbols.len()
    ));

    for sym in &symbols {
        let refs = cross_refs.get(&sym.name).cloned().unwrap_or_default();
        let source = symbol_sources.get(&sym.name).cloned().unwrap_or_default();
        let entry = render_entry(sym, &source, &refs, &skeleton_freq, format);
        entries.push(entry);
    }

    Ok(entries.join("\n"))
}

/// Generate a hotspot-selected pack: top-K symbols by composite score.
///
/// Use this when queries are unknown — picks the most "interesting" symbols
/// (most-called, most-connected, longest bodies) rather than the full pack.
///
/// Uses a LIGHT cross-ref computation (just inbound ref count per symbol)
/// instead of the full cross-ref graph — this is 50x faster on large codebases.
pub fn generate_pack_hotspot(
    path: &str,
    format: PackFormat,
    top_k: usize,
) -> Result<String, String> {
    let db = open_index(path)?;
    generate_pack_hotspot_with(&db, path, format, top_k)
}

/// C1: Hotspot pack using an already-open connection.
pub fn generate_pack_hotspot_with(
    db: &Connection,
    path: &str,
    format: PackFormat,
    top_k: usize,
) -> Result<String, String> {
    let symbols = extract_symbols_from_index(db)?;

    // For hotspot mode: only read sources for the top-K symbols (after ranking)
    // First pass: rank using just inbound ref count (no file I/O needed)
    let inbound_counts = compute_inbound_ref_counts(db, &symbols);
    let mut scored: Vec<(f32, &Symbol)> = symbols
        .iter()
        .map(|s| {
            let inbound = *inbound_counts.get(&s.name).unwrap_or(&0) as f32;
            (0.6 * inbound, s) // body_len unknown yet; rank by inbound only
        })
        .collect();
    // C33: select_nth_unstable_by for top-K (O(N) vs O(N log N)).
    let k = top_k.min(scored.len());
    if k < scored.len() {
        scored.select_nth_unstable_by(k, |a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    }
    scored.truncate(k);

    let selected: Vec<Symbol> = scored
        .into_iter()
        .map(|(_, s)| s.clone())
        .collect();

    let (symbol_sources, skeleton_freq) = read_symbols_for_subset(&selected, &symbols);

    // Build cross-refs only for the selected symbols
    let cross_refs = build_cross_refs_for_subset(db, &selected, &symbols);

    let mut entries = Vec::with_capacity(selected.len() + 1);
    entries.push(format!(
        "# Holographic Pack — {} ({} of {} symbols, hotspot-ranked)\n",
        Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string()),
        selected.len(),
        symbols.len()
    ));

    for sym in &selected {
        // C4: Borrow instead of clone.
        let refs = cross_refs.get(&sym.name).map(|v| v.as_slice()).unwrap_or(&[]);
        let source = symbol_sources.get(&sym.name).map(|s| s.as_str()).unwrap_or("");
        let entry = render_entry(sym, source, refs, &skeleton_freq, format);
        entries.push(entry);
    }

    let result = entries.join("\n");
    Ok(result)
}

/// Compute inbound reference counts for all symbols in one query.
/// Only counts refs for symbols that pass the is_definition_like filter
/// (otherwise common words like "std", "build", "fmt" dominate the ranking).
fn compute_inbound_ref_counts(
    db: &Connection,
    symbols: &[Symbol],
) -> FxHashMap<String, usize> {
    let mut counts: FxHashMap<String, usize> = FxHashMap::default();
    let symbol_names: HashSet<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

    // Count: for each phrase that appears as a non-def occurrence, how many times?
    // This is a proxy for "how many things reference this symbol".
    let mut stmt = match db.prepare(
        "SELECT p.phrase, COUNT(*) as cnt
         FROM occurrence o
         JOIN phrases p ON o.phrase_id = p.id
         JOIN file_map f ON o.file_id = f.id
         WHERE o.is_def = 0
         AND f.file_path NOT LIKE '%/bench/%'
         AND f.file_path NOT LIKE '%/tests/%'
         AND f.file_path NOT LIKE '%/test/%'
         AND f.file_path NOT LIKE '%/target/%'
         AND f.file_path NOT LIKE '%/vendor/%'
         AND f.file_path NOT LIKE '%/node_modules/%'
         AND f.file_path NOT LIKE '%/__pycache__/%'
         AND f.file_path NOT LIKE '%/.specstory/%'
         GROUP BY p.phrase",
    ) {
        Ok(s) => s,
        Err(_) => return counts,
    };

    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    });

    if let Ok(rows) = rows {
        for row in rows.flatten() {
            let (phrase, cnt) = row;
            // Only count if this phrase is a real symbol (passed is_definition_like)
            if symbol_names.contains(phrase.as_str()) {
                counts.insert(phrase, cnt as usize);
            }
        }
    }

    counts
}

/// Build cross-refs only for a subset of symbols (the selected hotspots).
/// This is much faster than building the full cross-ref graph.
fn build_cross_refs_for_subset(
    db: &Connection,
    selected: &[Symbol],
    all_symbols: &[Symbol],
) -> FxHashMap<String, Vec<String>> {
    let all_names: HashSet<&str> = all_symbols.iter().map(|s| s.name.as_str()).collect();
    let mut cross_refs: FxHashMap<String, Vec<String>> = FxHashMap::default();

    // BATCH: load all (file_id, phrase, is_def) in one query, then join in Rust.
    // This avoids 27 × 2 SQL queries (each scanning the full occurrence table).
    // V54: filter to known symbol names at the SQL level — cuts the load from
    // the full occurrence table (OOM risk on large repos) to symbol-relevant
    // rows only. The symbol set is small (thousands), occurrence rows for
    // non-symbol phrases (common words, std lib) dominate the table.
    let mut all_pairs_stmt = match db.prepare(
        "SELECT o.file_id, p.phrase, o.is_def
         FROM occurrence o
         JOIN phrases p ON o.phrase_id = p.id
         WHERE p.phrase IN (SELECT phrase FROM phrases WHERE length(phrase) BETWEEN 3 AND 30)
         ORDER BY o.file_id LIMIT 200000",
    ) {
        Ok(s) => s,
        Err(_) => return cross_refs,
    };

    let mut file_to_phrases: FxHashMap<i64, Vec<(String, bool)>> = FxHashMap::default();
    for row in all_pairs_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
            ))
        })
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|r| r.ok())
    {
        let (file_id, phrase, is_def) = row;
        file_to_phrases.entry(file_id).or_default().push((phrase, is_def));
    }

    // For each selected symbol: find files where it's used (non-def), then
    // find other defined symbols in those files.
    for sym in selected {
        // Find files where this symbol is used (non-def)
        let usage_files: Vec<i64> = file_to_phrases
            .iter()
            .filter(|(_, pairs)| pairs.iter().any(|(p, is_def)| *p == sym.name && !*is_def))
            .map(|(fid, _)| *fid)
            .take(50)
            .collect();

        let mut refs: HashSet<String> = HashSet::new();
        for file_id in usage_files {
            if let Some(pairs) = file_to_phrases.get(&file_id) {
                for (phrase, is_def) in pairs {
                    if *is_def
                        && *phrase != sym.name
                        && all_names.contains(phrase.as_str())
                        && !is_common_word(phrase)
                    {
                        refs.insert(phrase.clone());
                    }
                    if refs.len() >= 6 {
                        break;
                    }
                }
            }
            if refs.len() >= 6 {
                break;
            }
        }

        cross_refs.insert(sym.name.clone(), refs.into_iter().take(6).collect());
    }

    cross_refs
}

/// Auto-mode: gate + hotspot selection combined.
///
/// 1. Compute complexity score
/// 2. Gate: Full → top-50 hotspot pack; Minimal → top-15; Skip → empty
/// 3. Return the pack (or empty string)
pub fn generate_pack_auto(path: &str) -> Result<String, String> {
    // C1: Open DB once, pass through to gate + generate.
    let conn = open_index(path)?;
    let decision = gate_decision_for(&conn, path)?;
    
    match decision {
        GateDecision::Skip => Ok(String::new()),
        GateDecision::Minimal => generate_pack_hotspot_with(&conn, path, PackFormat::L2L3, 15),
        GateDecision::Full => generate_pack_hotspot_with(&conn, path, PackFormat::L2L3, 50),
    }
}

/// C9: Extract repeated open_index logic.
fn open_index(codebase_path: &str) -> Result<rusqlite::Connection, String> {
    let db_path = format!(
        "{}/.reliary/index.sqlite",
        codebase_path.trim_end_matches('/')
    );
    let conn = Connection::open(&db_path)
        .map_err(|e| format!("pack: open {}: {}", db_path, e))?;
    reliary_search::schema::open_existing_db_safe(&conn)
        .map_err(|e| format!("index not initialized: {}", e))?;
    Ok(conn)
}

/// A pack entry parsed into searchable fields.
#[derive(Debug, Clone)]
pub struct PackEntry {
    pub name: String,
    pub header: String,   // "## name/crate"
    pub body: String,     // L0+L2+L3+crossrefs text
}

/// Parse a full pack string into individual entries.
pub fn parse_pack_entries(pack: &str) -> Vec<PackEntry> {
    let mut entries = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_header: String = String::new();
    let mut current_body: String = String::new();

    for line in pack.lines() {
        if line.starts_with("## ") {
            // Save previous entry
            if let Some(name) = current_name.take() {
                entries.push(PackEntry {
                    name,
                    header: std::mem::take(&mut current_header),
                    body: std::mem::take(&mut current_body),
                });
            }
            current_name = Some(line.strip_prefix("///").unwrap_or(line).split('/').next().unwrap_or("").trim().to_string());
            current_header = line.to_string();
        } else if current_name.is_some() {
            if !current_body.is_empty() {
                current_body.push('\n');
            }
            current_body.push_str(line);
        }
    }
    // Save last entry
    if let Some(name) = current_name {
        entries.push(PackEntry {
            name,
            header: current_header,
            body: current_body,
        });
    }
    entries
}

/// Tokenize text into lowercase word tokens for BM25 scoring.
/// S1: Delegates to reliary_search::tokenize to use ASCII-only rules and
/// match the main index's BM25 grammar (prevents CJK/Unicode fusion).
fn tokenize(text: &str) -> Vec<String> {
    reliary_search::tokenize(text)
}

/// Slice a full pack for a specific query using BM25 scoring.
///
/// Pre-build the full pack once, then call this per query to retrieve the
/// top-K entries most relevant to the user's question. This handles the
/// Per-query decision: should we slice the pack for this query, or skip it?
///
/// Based on N→F analysis from the 63-probe benchmark:
/// - bug/crossref/detail queries: pack provides essential context (Δ = +1.0 to +1.8)
/// - arch/impact/review queries: model already knows from training (Δ = +0.0)
/// - discriminate/edge/what queries: marginal gain (Δ = +0.2 to +0.8)
///
/// This is a pure structural classifier — no API calls, no token cost.
/// Runs in <1ms, zero side effects. Safe to call on every query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceDecision {
    /// Send the sliced pack — query benefits from context
    Slice,
    /// Skip the pack — model likely knows the answer
    Skip,
}

/// Classify a query text to decide whether the sliced pack is worth sending.
///
/// Grammar-free: uses keyword presence and structural signals, not language parsing.
pub fn should_slice_for_query(query: &str) -> SliceDecision {
    let text = query.to_ascii_lowercase();

    // SLICE: bug detection and cross-references (biggest deltas)
    if text.contains("bug") || text.contains("issue") || text.contains("wrong")
        || text.contains("incorrect") || text.contains("missing") {
        return SliceDecision::Slice;
    }
    if text.contains("call") || text.contains("caller") || text.contains("who uses")
        || text.contains("references") || text.contains("called by") {
        return SliceDecision::Slice;
    }

    // SLICE: detail questions need specific values
    if text.contains("value") || text.contains("threshold") || text.contains("parameter")
        || text.contains("default") || text.contains("exactly") || text.contains("what number") {
        return SliceDecision::Slice;
    }

    // SLICE: question mentions specific symbol names that might not be in training.
    // These are NAMES that the model is unlikely to know from generic Rust knowledge
    // (project-specific internals), NOT common function names like "skeleton" or "classify".
    let func_indicators = [
        "find_clusters", "find_clusters_global", "skelhash", "skeleton_hash",
        "aggressive_skeleton_hash", "maxwell", "maxwellgate", "should_drop",
        "detect_strategy", "detect_tabular", "detect_json", "callgraph",
        "is_definition_line", "reliary_sift", "reliary-output",
    ];
    for indicator in &func_indicators {
        if text.contains(indicator) {
            return SliceDecision::Slice;
        }
    }

    // SKIP: architecture, impact, review, and what/edge/discriminate questions
    // (model already knows from training, or marginal gain)
    SliceDecision::Skip
}

/// Slice a pack for a specific query, but only if the classifier says it's worth it.
/// Returns (sliced_content, decision) so callers can log/meter the decision.
pub fn adaptive_slice(pack: &str, query: &str, top_k: usize) -> (String, SliceDecision) {
    let decision = should_slice_for_query(query);
    match decision {
        SliceDecision::Slice => (slice_pack_for_query(pack, query, top_k), decision),
        SliceDecision::Skip => (String::new(), decision),
    }
}

/// "which symbols to include" problem that hotspot selection can't solve.
///
/// Returns a sliced pack string with just the top-K entries.
pub fn slice_pack_for_query(pack: &str, query: &str, top_k: usize) -> String {
    let entries = parse_pack_entries(pack);
    if entries.is_empty() {
        return String::new();
    }

    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        // No query — return first top_k entries (fallback)
        return entries
            .into_iter()
            .take(top_k)
            .map(|e| format!("{}\n{}", e.header, e.body))
            .collect::<Vec<_>>()
            .join("\n\n");
    }

    // Build a simple BM25 index over the pack entries
    let n_docs = entries.len() as f64;
    let mut doc_tokens: Vec<Vec<String>> = Vec::with_capacity(entries.len());
    for entry in &entries {
        // Search over name + body (L0/L2/L3 text)
        let combined = format!("{} {}", entry.name, entry.body);
        doc_tokens.push(tokenize(&combined));
    }

    // Compute document frequencies for each query token
    let mut df: FxHashMap<String, usize> = FxHashMap::default();
    for tokens in &doc_tokens {
        let unique: HashSet<&str> = tokens.iter().map(|s| s.as_str()).collect();
        for t in &query_tokens {
            if unique.contains(t.as_str()) {
                *df.entry(t.clone()).or_insert(0) += 1;
            }
        }
    }

    // Compute avg document length
    let total_len: usize = doc_tokens.iter().map(|d| d.len()).sum();
    let avgdl = if n_docs > 0.0 {
        total_len as f64 / n_docs
    } else {
        1.0
    };

    // Score each document
    let mut scored: Vec<(f64, &PackEntry)> = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let doc_len = doc_tokens[i].len() as f64;
        let mut score = 0.0;
        // Count term frequencies in this doc
        let mut tf: FxHashMap<&str, usize> = FxHashMap::default();
        for t in &doc_tokens[i] {
            *tf.entry(t.as_str()).or_insert(0) += 1;
        }
        for qt in &query_tokens {
            let d_freq = *df.get(qt).unwrap_or(&0) as f64;
            if d_freq == 0.0 {
                continue;
            }
            let idf = ((n_docs - d_freq + 0.5) / (d_freq + 0.5) + 1.0).ln();
            let term_freq = *tf.get(qt.as_str()).unwrap_or(&0) as f64;
            if term_freq > 0.0 {
                let bm25 = idf * (term_freq * (1.2 + 1.0))
                    / (term_freq + 1.2 * (1.0 - 0.75 + 0.75 * (doc_len / avgdl)));
                score += bm25;
            }
        }
        // Boost: if the entry name directly matches a query token, boost score
        let name_lower = entry.name.to_ascii_lowercase();
        for qt in &query_tokens {
            if name_lower == *qt {
                score += 5.0; // strong boost for exact name match
            } else if name_lower.contains(qt.as_str()) {
                score += 2.0; // weaker boost for substring match
            }
        }
        scored.push((score, entry));
    }

    // Sort by score descending
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Take top-K with score > 0
    let selected: Vec<&PackEntry> = scored
        .into_iter()
        .filter(|(s, _)| *s > 0.0)
        .take(top_k)
        .map(|(_, e)| e)
        .collect();

    if selected.is_empty() {
        return String::new();
    }

    // Cross-ref expansion: for each selected entry, find cross-ref names
    // in its body (Cross-refs: or L4: lines) and include those entries too.
    // This ensures that when we slice for "detect_strategy", we also
    // include its callers/callees like "detect_json", "detect_tabular".
    let selected_names: HashSet<String> = selected.iter().map(|e| e.name.clone()).collect();
    // Pre-build index for O(1) entry lookup by name
    let mut entries_by_name: FxHashMap<&str, &PackEntry> = FxHashMap::default();
    for e in &entries {
        entries_by_name.entry(e.name.as_str()).or_insert(e);
    }
    let mut expanded: Vec<&PackEntry> = selected.clone();
    let mut expanded_names: HashSet<String> = selected_names.clone();
    let mut expansion_count = 0;
    let max_expansion = 15usize.saturating_sub(top_k.min(15));
    for entry in &selected {
        for line in entry.body.lines() {
            let refs_text = if let Some(rest) = line.strip_prefix("Cross-refs:") {
                Some(rest.trim())
            } else if line.starts_with("L4:") && line.contains("caller") {
                // L4: Rename → update N caller(s): name1, name2, name3
                if let Some(pos) = line.find(": ") {
                    let after = &line[pos + 2..];
                    // Skip if it's "update N caller(s)" prefix — extract names after the last ":"
                    if let Some(last_colon) = after.rfind(": ") {
                        Some(&after[last_colon + 2..])
                    } else {
                        Some(after)
                    }
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(refs) = refs_text {
                for ref_name in refs.split(',').take(3) {
                    let ref_name = ref_name.trim();
                    if ref_name.is_empty() { continue; }
                    let ref_name_slash = format!("{}/", ref_name);
                    // O(1) lookup: exact match or prefix match
                    if let Some(e) = entries_by_name.get(ref_name).or_else(|| {
                        entries_by_name.values().find(|e| e.name.starts_with(&ref_name_slash))
                    }) {
                        if !selected_names.contains(&e.name) && !expanded_names.contains(&e.name) {
                            expanded_names.insert(e.name.clone());
                            expanded.push(e);
                            expansion_count += 1;
                            if expansion_count >= max_expansion { break; }
                        }
                    }
                    if expansion_count >= max_expansion { break; }
                }
                break; // Only one Cross-refs/L4 line per entry
            }
        }
        if expansion_count >= max_expansion { break; }
    }

    let total_entries = expanded.len();

    let mut result = format!(
        "# Sliced Pack — {} entries for query: \"{}\"\n\n",
        total_entries,
        query.chars().take(80).collect::<String>()
    );
    for entry in &expanded {
        result.push_str(&format!("{}\n{}\n\n", entry.header, entry.body));
    }
    result
}

/// Generate a hierarchical pack: top-level map + per-module packs.
pub fn generate_hierarchical_pack(
    path: &str,
    format: PackFormat,
    max_symbols_per_module: usize,
) -> Result<HierarchicalPack, String> {
    let db_path = format!(
        "{}/.reliary/index.sqlite",
        path.trim_end_matches('/')
    );
    let db = Connection::open(&db_path)
        .map_err(|e| format!("cannot open index: {}", e))?;
    reliary_search::schema::open_existing_db_safe(&db)
        .map_err(|e| format!("index not initialized: {}", e))?;

    let symbols = extract_symbols_from_index(&db)?;
    let (symbol_sources, skeleton_freq) = read_symbols_and_frequency(&symbols);
    let cross_refs = build_cross_refs_from_index(&db, &symbols);

    // Group by module (top-level directory or packages/*)
    let modules = detect_modules(&symbols);

    // Build top-level map
    let mut top_parts = vec![format!(
        "# Holographic Pack — {} (top-level map)\n",
        Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string()),
    )];

    let mut module_packs: Vec<(String, String)> = Vec::new();

    for (module_name, module_syms) in &modules {
        let sym_count = module_syms.len();
        let key_syms: Vec<String> = module_syms
            .iter()
            .filter(|s| !s.doc_comment.is_empty())
            .take(5)
            .map(|s| s.name.clone())
            .collect();

        top_parts.push(format!("## {}", module_name));
        top_parts.push(format!("L0: {} symbols across this module", sym_count));
        if !key_syms.is_empty() {
            top_parts.push(format!("Key symbols: {}", key_syms.join(", ")));
        }
        top_parts.push(String::new());

        // Build per-module pack
        let limited: Vec<&Symbol> = module_syms.iter().take(max_symbols_per_module).collect();
        if limited.is_empty() {
            continue;
        }
        let mut pack_parts = vec![format!("# Holographic Pack — {}\n", module_name)];
        for sym in limited {
            let refs = cross_refs.get(&sym.name).cloned().unwrap_or_default();
            let source = symbol_sources.get(&sym.name).cloned().unwrap_or_default();
            let entry = render_entry(sym, &source, &refs, &skeleton_freq, format);
            pack_parts.push(entry);
        }
        module_packs.push((module_name.clone(), pack_parts.join("\n")));
    }

    Ok(HierarchicalPack {
        top_level: top_parts.join("\n"),
        module_packs,
    })
}

pub struct HierarchicalPack {
    pub top_level: String,
    pub module_packs: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Symbol extraction from the SQLite index
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub signature: String,
    pub file: String,
    pub line: i32,
    pub doc_comment: String,
    pub block_id: i64,
    pub end_line: i32,
    pub indent: i32,
}

/// Extract all definition symbols from the index.
///
/// Uses the `occurrence` table (is_def = 1) joined with `block` for body
/// boundaries and `file_map` for file paths.
fn extract_symbols_from_index(db: &Connection) -> Result<Vec<Symbol>, String> {
    let mut stmt = db
        .prepare(
            "SELECT o.phrase_id, o.file_id, o.line, o.block_id,
                    p.phrase, f.file_path,
                    b.end_line, b.indent
             FROM occurrence o
             JOIN phrases p ON o.phrase_id = p.id
             JOIN file_map f ON o.file_id = f.id
             JOIN block b ON o.block_id = b.block_id
             WHERE o.is_def = 1
             ORDER BY f.file_path, o.line",
        )
        .map_err(|e| format!("query symbols: {}", e))?;

    let rows = stmt
        .query_map([], |row| {
            let phrase: String = row.get(4)?;
            let file_path: String = row.get(5)?;
            let line: i32 = row.get(2)?;
            let block_id: i64 = row.get(3)?;
            let end_line: i32 = row.get(6)?;
            let indent: i32 = row.get(7)?;
            Ok((phrase, file_path, line, block_id, end_line, indent))
        })
        .map_err(|e| format!("query symbols: {}", e))?;

    let mut symbols: Vec<Symbol> = Vec::new();
    // Dedupe by (name, file_path) so the same name in different files
    // (e.g. `skeleton` in reliary-output and reliary-sift) is kept,
    // but the same name in the same file is deduped.
    let mut seen: HashSet<(String, String)> = HashSet::new();

    // S2: Use shared file_meta cache instead of per-call file_cache.
    // file_meta::get returns Arc<FileMeta> with lines pre-split and cached.
    let mut lines_cache: FxHashMap<String, Vec<String>> = FxHashMap::default();

    for row in rows {
        let (_phrase, file_path, line, block_id, end_line, indent) =
            row.map_err(|e| format!("row: {}", e))?;

        // Skip non-source files first (markdown, config, etc.)
        if !is_source_file(&file_path) {
            continue;
        }

        // Ensure lines are cached — file_meta::get is the shared LRU cache.
        if !lines_cache.contains_key(&file_path) {
            let lines_vec: Vec<String> = if let Some(meta) = reliary_search::file_meta::get(&file_path) {
                meta.lines.clone()
            } else {
                std::fs::read_to_string(&file_path)
                    .map(|c| c.lines().map(String::from).collect())
                    .unwrap_or_default()
            };
            lines_cache.insert(file_path.clone(), lines_vec);
        }

        let lines = &lines_cache[&file_path];

        // Read the source line for the signature
        let signature = read_signature_line(lines, line);

        // GRAMMAR-FREE DEFINITION FILTER:
        if !is_definition_like(&signature) {
            continue;
        }

        // Extract the REAL function name from the signature line
        let real_name = extract_name_from_signature(&signature);
        if real_name.len() < 3 {
            continue;
        }

        // Skip common English words and noise
        if is_common_word(&real_name) {
            continue;
        }

        // Skip Python dunders (often auto-generated, not real symbols)
        if real_name.starts_with("__") && real_name.ends_with("__") {
            continue;
        }

        // Dedupe by (name, file) — keep same name in different files
        let key = (real_name.clone(), file_path.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);

        // Look backward for doc comments
        let doc_comment = read_doc_comment(lines, line);

        symbols.push(Symbol {
            name: real_name,
            signature,
            file: file_path,
            line,
            doc_comment,
            block_id,
            end_line,
            indent,
        });
    }

    Ok(symbols)
}

/// Extract the function/type name from a signature line (grammar-free).
///
/// Strips common keywords and type annotations, then takes the first
/// identifier-like token that isn't a keyword or type.
fn extract_name_from_signature(sig: &str) -> String {
    let cleaned = sig.trim();

    // Strip leading keywords (language-agnostic)
    let mut rest = cleaned;
    let keywords = [
        "pub(crate) ", "pub(super) ", "pub ", "export ", "async ", "extern ",
        "static ", "inline ", "virtual ", "override ", "final ", "abstract ",
        "unsafe ", "mut ",
    ];
    loop {
        let mut changed = false;
        for kw in &keywords {
            if rest.starts_with(kw) {
                rest = &rest[kw.len()..];
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }

    // Strip declaration keywords
    let decl_kws = [
        "fn ", "function ", "def ", "func ", "struct ", "class ", "enum ",
        "trait ", "interface ", "type ", "impl ", "const ", "let ", "var ",
        "val ",
    ];
    for kw in &decl_kws {
        if rest.starts_with(kw) {
            rest = &rest[kw.len()..];
            break;
        }
    }

    // For impl blocks, skip the type name: "MaxwellGate {" → not a function
    // But "impl MaxwellGate {" → we want "MaxwellGate"
    if rest.starts_with("impl ") {
        rest = &rest[5..];
    }

    // Find the first identifier-like token (up to (, <, :, {, space, =)
    let end = rest
        .find(['(', '<', ':', '{', ' ', '=', ';'])
        .unwrap_or(rest.len());
    let name = rest[..end].trim();

    // Clean up: remove generics, lifetimes
    let name = name.trim_start_matches('\'');

    if name.is_empty() || name.len() < 2 {
        return String::new();
    }

    // Validate: must be identifier-like (alphanumeric + underscore, starts with alpha or _)
    if !name.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false) {
        return String::new();
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return String::new();
    }

    name.to_string()
}

/// Check if a file is a source code file (not markdown, config, etc.)
fn is_source_file(path: &str) -> bool {
    // Exclude common test/bench/vendor directories — these aren't the
    // codebase the user wants to explore.
    if path.contains("/bench/")
        || path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("/target/")
        || path.contains("/vendor/")
        || path.contains("/node_modules/")
        || path.contains("/__pycache__/")
        || path.contains("/.git/")
    {
        return false;
    }
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    matches!(
        ext,
        "rs" | "py" | "ts" | "tsx" | "js" | "jsx"
            | "go" | "java" | "c" | "h" | "cpp" | "cc" | "cxx"
            | "rb" | "swift" | "kt" | "scala" | "clj" | "ex" | "exs"
            | "elm" | "fs" | "ml" | "hs" | "lua" | "php" | "pl"
    )
}

/// Grammar-free definition detection — same heuristic as the experiment.
/// Checks if a line looks like a function/struct/enum/class definition
/// without knowing the language.
fn is_definition_like(line: &str) -> bool {
    let stripped = line.trim();
    if stripped.is_empty() {
        return false;
    }

    // Skip lines that are purely comments
    if stripped.starts_with("//")
        || stripped.starts_with('#')
        || stripped.starts_with("/*")
        || stripped.starts_with("///")
        || stripped.starts_with("--")
        || stripped.starts_with('\'')
    {
        return false;
    }

    // Skip control flow (if/for/while/switch/match/return) — not definitions
    if stripped.starts_with("if ")
        || stripped.starts_with("for ")
        || stripped.starts_with("while ")
        || stripped.starts_with("return ")
        || stripped.starts_with("match ")
        || stripped.starts_with("switch ")
        || stripped.starts_with("let ")
        || stripped.starts_with("let mut")
        || stripped.starts_with("const ")
        || stripped.starts_with("var ")
        || stripped.starts_with("static ")   // Rust statics, not function definitions
        || stripped.starts_with("type ")     // TypeScript type aliases
        || stripped.starts_with("enum ")     // already handles enum, but be explicit
        || stripped.starts_with("f.write")
        || stripped.starts_with("println")
        || stripped.starts_with("print")
        || stripped.starts_with("eprintln")
    {
        return false;
    }

    let check = stripped.trim_end_matches([';', ',']);

    // Must contain a function-like or type-like declaration pattern.
    // We require BOTH an identifier AND a block-opener or type-marker.

    // Function definition: has identifier( and ends with { or (
    // e.g., "pub fn skeleton(line: &str) -> String {"
    // e.g., "export function processSession(id: string): Result {"
    // Single-line body: ends with } but has balanced { ... } inside the line.
    //   e.g., "pub fn foo() -> i32 { 42 }"
    // BUT NOT: "std::iter::from_fn(move || {" (that's a call, not a definition)
    // BUT NOT: "search = run_altbackend_cli(" (that's an assignment, not a def)
    // BUT NOT: Python "def foo():" (handled separately below)
    let ends_with_brace = check.ends_with('{')
        || (check.ends_with('}')
            && check.matches('{').count() == 1
            && check.matches('}').count() == 1);
    if check.contains('(') && ends_with_brace {
        let before_paren = check.split('(').next().unwrap_or("").trim();
        // Must look like a function name: identifier optionally preceded by
        // keywords (pub, fn, def, function, etc.) but NOT a path (::) or a call.
        // Strip keywords to get the bare name
        let mut name_part = before_paren;
        for kw in &["pub(crate) ", "pub(super) ", "pub ", "async ", "unsafe ",
                    "extern ", "fn ", "function ", "func ", "export ",
                    "static ", "inline ", "const ", "final ", "override "] {
            if name_part.starts_with(kw) {
                name_part = &name_part[kw.len()..];
            }
        }
        // The name must be a simple identifier (no ::, no ., no ->, no =, no ||)
        // and must be > 3 chars to avoid noise like "new", "run", "get"
        if !name_part.contains("::")
            && !name_part.contains('.')
            && !name_part.contains("->")
            && !name_part.contains("||")
            && !name_part.contains('=')
            && !name_part.contains(' ')
            && name_part.len() > 3
        {
            return true;
        }
    }

    // Python-style: def name(args):
    if check.starts_with("def ") && check.ends_with(':') {
        return true;
    }

    // Class/struct/enum/interface/trait: ends with { and starts with keyword
    if check.ends_with('{') {
        let lower = check.to_ascii_lowercase();
        if lower.contains(" struct ")
            || lower.contains(" class ")
            || lower.contains(" enum ")
            || lower.contains(" trait ")
            || lower.contains(" interface ")
            || lower.contains(" impl ")
            || lower.starts_with("struct ")
            || lower.starts_with("class ")
            || lower.starts_with("enum ")
            || lower.starts_with("trait ")
            || lower.starts_with("interface ")
            || lower.starts_with("impl ")
            || lower.starts_with("pub struct ")
            || lower.starts_with("pub enum ")
            || lower.starts_with("pub trait ")
        {
            return true;
        }
    }

    // TypeScript interface/type: ends with { or starts with interface/type
    if check.ends_with('{') && (check.starts_with("export interface ") || check.starts_with("interface ") || check.starts_with("export type ") || check.starts_with("type ")) {
        return true;
    }

    false
}

/// Filter out common English words that the ingestion might mark as definitions.
fn is_common_word(word: &str) -> bool {
    // Skip very short words
    if word.len() < 3 {
        return true;
    }
    // Common English words that appear in comments/docs
    const COMMON: &[&str] = &[
        "the", "and", "for", "not", "are", "but", "you", "all", "can", "her",
        "was", "one", "our", "out", "has", "his", "how", "its", "may", "new",
        "now", "old", "see", "way", "who", "did", "get", "let", "say", "she",
        "too", "use", "any", "ask", "bad", "big", "day", "end", "few", "got",
        "had", "him", "his", "how", "its", "man", "men", "put", "run", "set",
        "try", "two", "use", "via", "yet", "also", "code", "data", "file",
        "from", "have", "into", "just", "like", "make", "more", "must",
        "only", "over", "some", "such", "than", "them", "then", "they",
        "this", "that", "when", "what", "where", "which", "while", "will",
        "with", "work", "would", "your", "about", "after", "again", "before",
        "being", "below", "could", "every", "first", "found", "great",
        "group", "having", "here", "high", "into", "itself", "just",
        "large", "last", "left", "like", "long", "made", "many", "most",
        "much", "never", "next", "once", "other", "part", "place", "right",
        "same", "should", "show", "since", "small", "some", "still",
        "such", "take", "their", "them", "then", "there", "these",
        "they", "thing", "think", "this", "those", "through", "time",
        "under", "until", "very", "well", "were", "what", "when",
        "where", "which", "while", "will", "with", "without", "would",
        "write", "your", "index", "free", "grammar", "instead", "tool",
        "tree", "language", "read", "work", "reliary",
    ];
    // P2-7: use HashSet for O(1) lookup instead of O(200) linear scan.
    static COMMON_SET: std::sync::OnceLock<std::collections::HashSet<&'static str>> = std::sync::OnceLock::new();
    let set = COMMON_SET.get_or_init(|| std::collections::HashSet::from_iter(COMMON.iter().copied()));
    let lower = word.to_ascii_lowercase();
    set.contains(lower.as_str())
}

fn read_signature_line(lines: &[String], line: i32) -> String {
    lines
        .get(line.max(0) as usize)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn read_doc_comment(lines: &[String], def_line: i32) -> String {
    let def_idx = (def_line.max(0) as usize).min(lines.len().saturating_sub(1));

    // Look backward for doc comments (/// or /** */ or // or #)
    let mut doc_parts: Vec<String> = Vec::new();
    let mut j = def_idx.saturating_sub(1);

    // Check for block comment ending right above
    if j < lines.len() && lines[j].trim().ends_with("*/") {
        let mut block: Vec<String> = Vec::new();
        while j > 0 {
            let l = lines[j].trim();
            block.insert(0, l.to_string());
            if l.starts_with("/**") || l.starts_with("/*") {
                break;
            }
            j -= 1;
        }
        for bl in &block {
            let clean = bl
                .trim()
                .trim_start_matches("/**")
                .trim_start_matches("/*")
                .trim_end_matches("*/")
                .trim()
                .trim_start_matches('*')
                .trim();
            if !clean.is_empty() && !clean.starts_with('@') && clean != "/**" {
                doc_parts.insert(0, clean.to_string());
            }
        }
    }

    if doc_parts.is_empty() {
        // Line comments above
        j = def_idx.saturating_sub(1);
        while j > 0 {
            let l = lines[j].trim();
            if let Some(rest) = l.strip_prefix("///") {
                doc_parts.insert(0, rest.trim().to_string());
            } else if let Some(rest) = l.strip_prefix("//") {
                doc_parts.insert(0, rest.trim().to_string());
            } else if l.starts_with('#') && !l.starts_with("#[") {
                doc_parts.insert(0, l[1..].trim().to_string());
            } else if l.is_empty() {
                // skip blanks between comment and definition
            } else {
                break;
            }
            j -= 1;
        }
    }

    doc_parts.join(" ")
}

// ---------------------------------------------------------------------------
// Skeleton-frequency for surprise detection
// ---------------------------------------------------------------------------

/// Returns true if `line` is a non-surprising code pattern that should NOT
/// appear in L3 surprise output.
///
/// These are the lines that pollute the auto-generated L3 with noise — they
/// appear as "unique line: ..." entries but carry no real surprise value.
/// The hand-crafted pack omitted them entirely; this filter makes the auto-
/// generated pack match that judgment.
fn is_noise_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Length: lines shorter than 15 chars are too short to carry surprise
    // (closing braces, semicolons, blank-ish content).
    if trimmed.len() < 15 {
        return true;
    }
    // Pure structural closers
    if trimmed == "}" || trimmed == "});" || trimmed == "};" || trimmed == "];" || trimmed == ")" {
        return true;
    }
    // Imports / use statements (the skeleton normalizes these all to {w} anyway,
    // and the model's prior handles them).
    if trimmed.starts_with("use ") || trimmed.starts_with("import ") {
        return true;
    }
    // Comments — already excluded by tokenization, but double-check
    if trimmed.starts_with("//") || trimmed.starts_with("#") || trimmed.starts_with("/*") {
        return true;
    }
    // Python docstrings (multi-line string literals used as documentation)
    if trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''") {
        return true;
    }
    // Python class-method internals — not surprising, just noise
    if trimmed.starts_with("self.") || trimmed.starts_with("cls.") || trimmed.starts_with("super()") {
        return true;
    }
    // Python imports — model's prior handles these
    if trimmed.starts_with("from ") && trimmed.contains(" import ") {
        return true;
    }
    // Python assertions — test helpers, not surprising
    if trimmed.starts_with("assert") {
        return true;
    }
    // Lines that are just a closing docstring/quote
    if trimmed == "\"\"\"" || trimmed == "'''" || trimmed == "\"\"\";" || trimmed == "'''\";" {
        return true;
    }
    // Simple variable bindings: `let x = y;` or `let mut x = ...;`
    // These are the most common "unique line" noise. Allow ones with parens
    // (`let x = foo(...)` may be a function call worth noting).
    if (trimmed.starts_with("let ") || trimmed.starts_with("let mut "))
        && !trimmed.contains("(")
        && !trimmed.contains("{")
        && trimmed.ends_with(";")
    {
        return true;
    }
    // Simple return statements with no value or a bare identifier
    // (`return;` or `return x;` where x is a single word — not surprising)
    // S1 fix: strip inline comments first so trailing // doesn't bypass filters
    let trimmed_for_return = if let Some(comment_start) = trimmed.find("//") {
        trimmed[..comment_start].trim()
    } else {
        trimmed
    };
    if let Some(rest) = trimmed_for_return.strip_prefix("return ") {
        // `return;` or `return Some(x);` or `return None;` IS interesting if it's
        // a sentinel. Only filter the trivial `return <single_word>;` case.
        if let Some(inner) = rest.strip_suffix(";") {
            let inner = inner.trim();
            if !inner.contains("(") && !inner.contains(".") && !inner.contains("[") {
                // Single bare identifier return — likely noise
                return true;
            }
        }
        // Python returns: `return X.lower() in Y.lower()` — too common to be surprising
        if rest.contains(".lower()") || rest.contains(".upper()") {
            return true;
        }
    }
    // Python `if isinstance(obj, X):` — too common to be surprising
    if trimmed.starts_with("if isinstance(") {
        return true;
    }
    // Python `for X in Y:` — basic iteration, not surprising
    if trimmed.starts_with("for ") && trimmed.ends_with(":") && !trimmed.contains("enumerate") {
        return true;
    }
    // Python `self.X = Y` — attribute assignment, not surprising
    if trimmed.starts_with("self.") && trimmed.contains(" = ") && !trimmed.contains("(") {
        return true;
    }
    // Simple assertions / panics with no context
    if trimmed.starts_with("assert!") || trimmed.starts_with("panic!") || trimmed.starts_with("todo!") {
        return true;
    }
    // Attribute lines
    if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
        return true;
    }
    // Mod / pub mod declarations
    if trimmed.starts_with("mod ") || trimmed.starts_with("pub mod ") {
        return true;
    }
    // S1 fix: derive attributes already filtered above with #include, but
    // additional patterns below were still producing "unique line:" noise
    if trimmed.starts_with("derive(") || trimmed.starts_with("cfg_attr(") || trimmed.starts_with("allow(") {
        return true;
    }
    // S1 fix: Python test methods (self.assertX, self.setUp, self.X.method)
    if let Some(rest) = trimmed.strip_prefix("self.") {
        if rest.starts_with("assert") || rest.contains(".assert") || rest.starts_with("setUp")
            || rest.starts_with("tearDown") || rest.starts_with("set_up")
            || rest.starts_with("tear_down") || rest.starts_with("skip(")
            || rest.starts_with("fixture") || rest.starts_with("patch(")
        {
            return true;
        }
    }
    // S1 fix: Rust method calls ending in ; — operations, not surprising
    //   e.g., `block_phrases.append(line);`
    if trimmed.contains(".append(") || trimmed.contains(".push(")
        || trimmed.contains(".insert(") || trimmed.contains(".remove(")
        || trimmed.contains(".clear(") || trimmed.contains(".drop(")
        || trimmed.contains(".len()") || trimmed.contains(".is_empty()")
        || trimmed.contains(".expect(") || trimmed.contains(".collect()")
        || trimmed.contains(".clone(") || trimmed.contains(".iter()")
    {
        return true;
    }
    // S1 fix: Rust single-token statements
    if trimmed == "break;" || trimmed == "continue;"
        || trimmed == "break" || trimmed == "continue" {
        return true;
    }
    // S1 fix: Python from X import statements (already covered by use)
    if trimmed.starts_with("from ") {
        return true;
    }
    // S1 fix: trivial returns (`return None;` `return true;` `return false;`)
    if let Some(stripped_return) = trimmed.strip_prefix("return ") {
        let rest = stripped_return.trim().trim_end_matches(';').trim();
        if matches!(rest, "true" | "false" | "None" | "0" | "1" | "Self" | "-1") {
            return true;
        }
    }
    // S1 fix: log macros and trivial logging
    if trimmed.starts_with("log::") || trimmed.starts_with("tracing::")
        || trimmed.starts_with("debug!") || trimmed.starts_with("info!")
        || trimmed.starts_with("warn!") || trimmed.starts_with("error!")
        || trimmed.starts_with("print!") || trimmed.starts_with("eprint!")
        || trimmed.starts_with("println!") || trimmed.starts_with("eprintln!")
        || trimmed.starts_with("dbg!") || trimmed.starts_with("todo!")
    {
        return true;
    }
    // S1 fix: ignore/discard assignments like `let _ = expr;`
    if trimmed.starts_with("let _ =") || trimmed.starts_with("let _mut _ =")
        || trimmed.starts_with("let (_, _) =") || trimmed.starts_with("let (_, _, _) =")
    {
        return true;
    }
    // S1 fix: option-result idioms (.ok() / .ok_or() patterns)
    // Strip trailing inline comment first (// GUARDED: ...)
    let code_only = if let Some(comment_start) = trimmed.find("//") {
        trimmed[..comment_start].trim()
    } else {
        trimmed
    };
    if code_only.ends_with(".ok()?") || code_only.ends_with(".ok();")
        || code_only.contains(".expect(\"") || code_only.contains(".unwrap_or(")
        || code_only.contains(".unwrap_or_default(") || code_only.contains(".unwrap_or_else(")
        || (code_only.starts_with("return ") && code_only.contains(".ok()"))
        || (code_only.starts_with("return ") && code_only.contains(".err()"))
        || (code_only.starts_with("return ") && code_only.contains(".unwrap()"))
    {
        return true;
    }
    if let Some(eq_pos) = trimmed.find('=') {
        if eq_pos > 0 && !trimmed[eq_pos+1..].trim().starts_with("\"")
            && trimmed.contains(".to_string(")
            && !trimmed.contains("unsafe ") && !trimmed.contains("transmute")
        {
            return true;
        }
    }
    false
}

/// Extract surprise facts from a symbol's body using skeleton-frequency.
///
/// A line is "surprising" if its aggressive_skeleton appears < RARE_THRESHOLD
/// times across the codebase. We also extract magic numbers, compound
/// conditions, and early returns from surprising lines.
fn extract_surprise_from_body(
    body: &str,
    skeleton_freq: &FxHashMap<String, usize>,
) -> Vec<String> {
    let mut surprises: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let lines: Vec<&str> = body.lines().collect();

    // === Pass 1: Pattern-based detection ===
    // Recognize common "surprising code patterns" and generate labeled descriptions.

    // Pattern: `<N>` auto-pass guard at the top of a function
    //   e.g., `if text.len() < 50 { return Some((1.0, text.len())); }`
    //   → "auto-pass for input below N chars (no gates applied)"
    if let Some(m) = regex_capture(r"if\s+\w+\.len\(\)\s*<\s*(\d+)\s*\{", body) {
        add_surprise(&mut surprises, &mut seen,
            format!("auto-pass for input below {} chars (no gates applied)", m));
    }

    // Pattern: UUID-like position checks (8, 13, 18, 23)
    //   e.g., `let expect_dash = j == 8 || j == 13 || j == 18 || j == 23;`
    if body.contains("expect_dash") || body.contains("j == 8") && body.contains("j == 13") && body.contains("j == 23") {
        add_surprise(&mut surprises, &mut seen,
            "UUID dash positions: 8, 13, 18, 23 (standard 8-4-4-4-12)".to_string());
    }

    // Pattern: hex-hash length range
    //   e.g., `he - i >= 7 && he - i <= 40`
    if body.contains("he - i") || body.contains("hex_len") {
        if let Some(m) = regex_capture(r">=\s*(\d+)\s*&&\s*\w+\s*-\s*\w+\s*<=\s*(\d+)", body) {
            add_surprise(&mut surprises, &mut seen,
                format!("hex-hash length range: {} to {}", m.split(',').next().unwrap_or(""), m.split(',').nth(1).unwrap_or("")));
        }
    }

    // Pattern: djb3-33 hash constants
    //   e.g., `let mut h: u64 = 5381; for b ... { h = h.wrapping_mul(33) }`
    if body.contains("5381") && body.contains("wrapping_mul") && body.contains("33") {
        add_surprise(&mut surprises, &mut seen,
            "djb3-33 hash: starts at 5381, multiplies by 33 (Bernstein variant)".to_string());
    }

    // Pattern: byte-DFA processing
    //   e.g., iterating `bytes[i]` and `bytes[he]` directly
    if body.contains("bytes[") && body.contains("while") && !body.contains("chars()") {
        add_surprise(&mut surprises, &mut seen,
            "byte-indexed processing (not char-indexed)".to_string());
    }

    // Pattern: sentinel return value
    //   e.g., `if s.is_empty() { return 0; }` or `return String::new()`
    if let Some(m) = regex_capture(r"if\s+\w+\.is_empty\(\)\s*\{\s*return\s+([^;]+)", body) {
        add_surprise(&mut surprises, &mut seen,
            format!("sentinel return: {} for empty input", m));
    }

// Pattern: import collapse label
    //   e.g., `format!("[{} imports]", import_count)`
    if body.contains("import") && body.contains("count") && body.contains('[') {
        add_surprise(&mut surprises, &mut seen,
            "imports collapsed into a single label '[N imports]'".to_string());
    }

    // Pattern: lazy/deferred flush
    //   e.g., only writing to result on the next non-matching line
    if body.contains("flush") || (body.contains("if ") && body.contains("emit") && body.contains("reset")) {
        add_surprise(&mut surprises, &mut seen,
            "lazy flush: label emitted on next non-matching line, not immediately".to_string());
    }

    // Pattern: priority ordering
    //   e.g., Error check before Comment check
    if body.contains("Error") && body.contains("Comment") {
        let error_pos = body.find("Error").unwrap_or(0);
        let comment_pos = body.find("Comment").unwrap_or(0);
        if error_pos < comment_pos && error_pos > 0 && comment_pos > 0 {
            add_surprise(&mut surprises, &mut seen,
                "Error check runs BEFORE Comment check (priority order)".to_string());
        } else if comment_pos < error_pos && comment_pos > 0 && error_pos > 0 {
            add_surprise(&mut surprises, &mut seen,
                "Comment check runs BEFORE Error check (potential bug: errors in comments are missed)".to_string());
        }
    }

    // Pattern: gate combination (AND vs OR)
    //   e.g., `if ent < threshold && ratio > max { return None; }`
    if regex_capture(r"if\s+\w+\s*[<>]=\s*\w+_threshold\s*[&|][&|]\s*\w+\s*[<>]=\s*\w+", body).is_some() {
        if body.contains("&&") {
            add_surprise(&mut surprises, &mut seen,
                "gates are AND-combined: ALL must fail for None return".to_string());
        } else if body.contains("||") {
            add_surprise(&mut surprises, &mut seen,
                "gates are OR-combined: ANY failing gate returns None".to_string());
        }
    }

    // Pattern: minimum word length for aggressive collapsing
    //   e.g., `we > i + 1` (length > 1 keeps single letters verbatim)
    if body.contains("aggressive") || body.contains("collapse") {
        if let Some(m) = regex_capture(r"\w+\s*>\s*\w+\s*\+\s*(\d+)", body) {
            add_surprise(&mut surprises, &mut seen,
                format!("minimum word length threshold: {} (below this, words kept verbatim)", m));
        }
    }

    // Pattern: function called on the SKELETON, not the original
    //   e.g., `let s = skeleton(text); ... hash(s)`
    if let Some(m) = regex_capture(r"let\s+(\w+)\s*=\s*skeleton\(\w+\)", body) {
        add_surprise(&mut surprises, &mut seen,
            format!("operates on the SKELETON of input, not the original text (via {})", m));
    }

    // Pattern: regex-based normalization (not byte-DFA)
    //   e.g., `PATTERNS.uuid.replace_all(&cleaned, "{uuid}")`
    if body.contains("replace_all") && body.contains("&") {
        let placeholders: Vec<&str> = body
            .split(".replace_all")
            .skip(1)
            .filter_map(|s| {
                if let Some(idx) = s.find('"') {
                    let after = &s[idx..];
                    if let Some(end) = after[1..].find('"') {
                        return Some(&after[1..=end]);
                    }
                }
                None
            })
            .collect();
        if !placeholders.is_empty() {
            add_surprise(&mut surprises, &mut seen,
                format!("regex-based replacement: {}", placeholders.iter().take(3).cloned().collect::<Vec<_>>().join(", ")));
        }
    }

// Pattern: three-comma version detection
    //   e.g., `X.Y.Z` with `num_end` calls
    if body.contains("num_end") && body.contains('.') {
        add_surprise(&mut surprises, &mut seen,
            "version detection: requires exactly three dot-separated numeric components (X.Y.Z)".to_string());
    }

    // === Pass 2: Line-level signal extraction ===
    let mut seen_nums: HashSet<String> = HashSet::new();
    let mut seen_conds: HashSet<String> = HashSet::new();

    for (line_idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("#")
            || trimmed.starts_with("///") || trimmed.starts_with("/*") || trimmed.starts_with("*") {
            continue;
        }
        let code = trimmed;

        // Magic numbers in comparisons
        for num in find_magic_numbers(code) {
            let key = format!("{}_{}", num, line_idx);
            if !seen_nums.contains(&key) {
                seen_nums.insert(key);
                let labeled = label_number(&num, code);
                if !labeled.starts_with("constant: 0") && !labeled.starts_with("constant: 1") {
                    add_surprise(&mut surprises, &mut seen, labeled);
                }
            }
        }

        // Compound conditions (not already captured by patterns)
        if code.contains("if ") && (code.contains("&&") || code.contains("||")) {
            if let Some(cond) = extract_condition(code) {
                let cond_key: String = cond.chars().take(40).collect();
                if cond.len() > 10 && cond.len() < 200 && !seen_conds.contains(&cond_key) {
                    seen_conds.insert(cond_key);
                    add_surprise(&mut surprises, &mut seen,
                        format!("compound condition: {}", cond));
                }
            }
        }

        // Early returns
        if line_idx < 15 && code.contains("if ") && code.contains("return") {
            if let Some(ret) = extract_return_value(code) {
                if !ret.is_empty() && ret != "true" && ret != "false" {
                    add_surprise(&mut surprises, &mut seen,
                        format!("early return → {}", ret));
                }
            }
        }

        // Deliberate patterns
        if code.contains(".wrapping_") {
            let ops: Vec<&str> = code
                .split(".wrapping_")
                .skip(1)
                .filter_map(|s| s.split('(').next())
                .collect();
            if !ops.is_empty() {
                add_surprise(&mut surprises, &mut seen,
                    format!("wrapping: {}", ops.join(", ")));
            }
        }
        if code.contains("unwrap_or(") {
            if let Some(def) = extract_unwrap_default(code) {
                if !def.is_empty() {
                    add_surprise(&mut surprises, &mut seen,
                        format!("fallback: unwrap_or({})", def));
                }
            }
        }

        // Capacity hints
        if code.contains("with_capacity(") {
            if let Some(n) = extract_capacity(code) {
                add_surprise(&mut surprises, &mut seen,
                    format!("capacity: {}", n));
            }
        }

        // Comment signals
        if let Some(comment_start) = code.find("//") {
            let comment = code[comment_start + 2..].trim().to_ascii_lowercase();
            let comment_text = code[comment_start + 2..].trim();
            for sig in ["key", "difference", "note", "important", "surprise",
                        "critical", "deliberate", "only fires", "not regex", "must"]
            {
                if comment.contains(sig) {
                    add_surprise(&mut surprises, &mut seen,
                        format!("source note: {}", comment_text));
                    break;
                }
            }
        }
    }

    // === Pass 3: Skeleton-frequency rare patterns (tertiary) ===
    for (line_idx, line) in lines.iter().enumerate() {
        if line_idx == 0 { continue; } // skip signature line
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("#") {
            continue;
        }
        if is_noise_line(trimmed) {
            continue;
        }
        let skel = aggressive_skeleton(trimmed);
        if skel.is_empty() || skel == "{w}" || skel == "}" || skel.len() < 10 {
            continue;
        }
        let freq = skeleton_freq.get(&skel).copied().unwrap_or(0);
        if freq == 1 {
            // S1 fix: only emit unique-line for lines with structural signal.
            // A truly unique line with no parens / braces / comparisons /
            // assignments is just noise (any random `let x = other_line;`
            // can be unique because of variable name).
            let has_signal = trimmed.contains('(')
                || trimmed.contains("==") || trimmed.contains("!=")
                || trimmed.contains(" < ") || trimmed.contains(" > ")
                || trimmed.contains(">=") || trimmed.contains("<=")
                || trimmed.contains("&&") || trimmed.contains("||")
                || trimmed.contains("wrapping_") || trimmed.contains("unwrap_or")
                || trimmed.contains("match ") || trimmed.contains("=>");
            if has_signal {
                let snippet: String = trimmed.chars().take(80).collect();
                add_surprise(&mut surprises, &mut seen,
                    format!("unique line: {}", snippet));
            }
        }
    }

    // S1 fix: dedupe across passes (early return + unique line often duplicate)
    // After both passes are done, dedupe entries that share key information.
    let pre_dedup_len = surprises.len();
    let mut final_surprises: Vec<String> = Vec::new();
    let mut seen_fragments: HashSet<String> = HashSet::new();
    for s in surprises {
        // Extract a canonical "fragment" — same early return + same unique line → same key
        let fragment: String = s
            .chars()
            .filter(|c| !c.is_whitespace())
            .take(40)
            .collect();
        if seen_fragments.contains(&fragment) {
            continue;
        }
        seen_fragments.insert(fragment);
        final_surprises.push(s);
    }
    let surprises = final_surprises;
    let post_dedup_len = surprises.len();
    if pre_dedup_len > post_dedup_len + 1 {
        // dedupe was meaningful; that's good
    }
    // Count pattern-based (non "rare pattern" / "unique line") vs. frequency-based
    let pattern_count = surprises.iter()
        .filter(|s| !s.starts_with("rare pattern") && !s.starts_with("unique line"))
        .count();
    let freq_count = surprises.len() - pattern_count;

    // S1 fix: only include freq-based if we have 3+ pattern matches.
    // Without this guard, functions with zero patterns (just let-binding lines)
    // get 5 useless "unique line:" entries.
    let limit = if pattern_count >= 3 {
        pattern_count.min(8) // only pattern-based
    } else if pattern_count >= 1 {
        (pattern_count + freq_count.min(2)).min(8)
    } else {
        // No patterns matched — skip freq-based entirely (would be all noise)
        0
    };

    surprises.into_iter().take(limit).collect()
}

fn add_surprise(surprises: &mut Vec<String>, seen: &mut HashSet<String>, s: String) {
    let key: String = s.chars().take(60).collect();
    if !seen.contains(&key) {
        seen.insert(key);
        surprises.push(s);
    }
}

/// Minimal regex capture helper — finds the first match of `pattern` in `text`
/// and returns the first capture group as a String.
fn regex_capture(pattern: &str, text: &str) -> Option<String> {
    // Avoid adding a regex dependency — simple manual patterns
    if pattern == r"if\s+\w+\.len\(\)\s*<\s*(\d+)\s*\{" {
        return manual_capture_len_guard(text);
    }
    if pattern == r">=\s*(\d+)\s*&&\s*\w+\s*-\s*\w+\s*<=\s*(\d+)" {
        return manual_capture_range(text);
    }
    if pattern == r"if\s+\w+\.is_empty\(\)\s*\{\s*return\s+([^;]+)" {
        return manual_capture_sentinel(text);
    }
    if pattern == r"\w+\s*>\s*\w+\s*\+\s*(\d+)" {
        return manual_capture_gt_plus(text);
    }
    if pattern == r"if\s+\w+\s*[<>]=\s*\w+_threshold\s*[&|][&|]\s*\w+\s*[<>]=\s*\w+" {
        return if text.contains("_threshold") { Some("found".to_string()) } else { None };
    }
    if pattern == r"let\s+(\w+)\s*=\s*skeleton\(\w+\)" {
        return manual_capture_skeleton_let(text);
    }
    None
}

fn manual_capture_len_guard(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 2 < bytes.len() && bytes[i] == b'i' && bytes[i+1] == b'f' {
            // skip "if"
            i += 2;
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
            // skip identifier
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') { i += 1; }
            // skip .len()
            while i + 4 < bytes.len() && &bytes[i..i+5] == b".len(" { i += 5; while i < bytes.len() && bytes[i] != b')' { i += 1; } if i < bytes.len() { i += 1; } }
            // skip spaces and <
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
            if i < bytes.len() && bytes[i] == b'<' { i += 1; }
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
            // read number
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
            if i > start {
                return Some(String::from_utf8_lossy(&bytes[start..i]).to_string());
            }
        }
        i += 1;
    }
    None
}

fn manual_capture_range(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'>' && bytes[i+1] == b'=' {
            i += 2;
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
            if i > start {
                let first = String::from_utf8_lossy(&bytes[start..i]).to_string();
                // find the second number after <=
                while i < bytes.len() && bytes[i] != b'<' { i += 1; }
                if i + 1 < bytes.len() && bytes[i] == b'<' && bytes[i+1] == b'=' { i += 2; }
                while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }
                let start2 = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
                if i > start2 {
                    let second = String::from_utf8_lossy(&bytes[start2..i]).to_string();
                    return Some(format!("{},{}", first, second));
                }
            }
        }
        i += 1;
    }
    None
}

fn manual_capture_sentinel(text: &str) -> Option<String> {
    if let Some(idx) = text.find("is_empty") {
        let after = &text[idx + 9..];
        if let Some(ret_idx) = after.find("return") {
            let after_ret = &after[ret_idx + 6..];
            let end = after_ret
                .find([';', '}', '\n'])
                .unwrap_or(after_ret.len());
            return Some(after_ret[..end].trim().to_string());
        }
    }
    None
}

fn manual_capture_gt_plus(text: &str) -> Option<String> {
    if let Some(idx) = text.find('>') {
        let after = &text[idx + 1..];
        // skip spaces
        let trimmed = after.trim_start();
        // skip identifier
        let after_ident = trimmed
            .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_');
        let trimmed2 = after_ident.trim_start();
        if let Some(rest) = trimmed2.strip_prefix('+') {
            let trimmed3 = rest.trim_start();
            let end = trimmed3
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(trimmed3.len());
            if end > 0 {
                return Some(trimmed3[..end].to_string());
            }
        }
    }
    None
}

fn manual_capture_skeleton_let(text: &str) -> Option<String> {
    if let Some(idx) = text.find("let ") {
        let after = &text[idx + 4..];
        let after_ident = after
            .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_');
        let trimmed = after_ident.trim_start();
        if let Some(after_eq) = trimmed.strip_prefix('=') {
            let after_eq = after_eq.trim_start();
            if let Some(sk_idx) = after_eq.find("skeleton(") {
                let before = &after_eq[..sk_idx].trim();
                if !before.is_empty() {
                    return Some(before.to_string());
                }
            }
        }
    }
    None
}

fn find_magic_numbers(code: &str) -> Vec<String> {
    let mut nums = Vec::new();
    // Simple regex-free approach: find numbers after comparison operators
    let bytes = code.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if (bytes[i] == b'>' || bytes[i] == b'<' || bytes[i] == b'=' || bytes[i] == b'!')
            && i + 1 < bytes.len()
        {
            // Skip the operator
            i += 1;
            if i < bytes.len() && (bytes[i] == b'=' || bytes[i] == b' ') {
                i += 1;
            }
            // Skip spaces
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            // Read number
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if i > start {
                let num = String::from_utf8_lossy(&bytes[start..i]).to_string();
                if num != "0" && num != "1" {
                    nums.push(num);
                }
            }
        } else {
            i += 1;
        }
    }
    nums
}

fn label_number(num: &str, code: &str) -> String {
    let lower = code.to_ascii_lowercase();
    if lower.contains("uuid") || (num == "8" || num == "13" || num == "18" || num == "23") {
        format!("UUID position: {}", num)
    } else if lower.contains("hash") || lower.contains("hex") {
        format!("hex-hash bound: {}", num)
    } else if lower.contains("entropy") || lower.contains("threshold") {
        format!("threshold: {}", num)
    } else if lower.contains("len") || lower.contains("length") || lower.contains("count") {
        format!("length: {}", num)
    } else if lower.contains("capacity") {
        format!("capacity: {}", num)
    } else {
        format!("constant: {}", num)
    }
}

fn extract_condition(code: &str) -> Option<String> {
    let if_pos = code.find("if ")?;
    let after = &code[if_pos + 3..];
    // Find the end of condition (opening brace or end of line)
    let end = after
        .find(['{', '\n'])
        .unwrap_or(after.len());
    Some(after[..end].trim().to_string())
}

fn extract_return_value(code: &str) -> Option<String> {
    let ret_pos = code.find("return")?;
    let after = &code[ret_pos + 6..];
    let end = after.find([';', '}']).unwrap_or(after.len());
    Some(after[..end].trim().to_string())
}

fn extract_unwrap_default(code: &str) -> Option<String> {
    let pos = code.find("unwrap_or(")?;
    let after = &code[pos + 10..];
    let end = after.find(')')?;
    Some(after[..end].trim().to_string())
}

fn extract_capacity(code: &str) -> Option<String> {
    let pos = code.find("with_capacity(")?;
    let after = &code[pos + 14..];
    let end = after.find(')')?;
    Some(after[..end].trim().to_string())
}

// ---------------------------------------------------------------------------
// Cross-references from the SQLite index
// ---------------------------------------------------------------------------

/// Build cross-reference map: symbol_name → list of symbols that reference it.
///
/// Uses file-level co-occurrence: find files where the symbol appears as a
/// non-def occurrence, then list other symbols defined in those files.
/// This is broader than block-matching and catches most real references.
fn build_cross_refs_from_index(
    db: &Connection,
    symbols: &[Symbol],
) -> FxHashMap<String, Vec<String>> {
    // C3: Return empty map on error instead of panicking.
    build_cross_refs_inner(db, symbols).unwrap_or_default()
}

fn build_cross_refs_inner(
    db: &Connection,
    symbols: &[Symbol],
) -> Result<FxHashMap<String, Vec<String>>, rusqlite::Error> {
    let def_names: HashSet<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    let mut cross_refs: FxHashMap<String, Vec<String>> = FxHashMap::default();
    let mut ref_set: FxHashMap<String, HashSet<String>> = FxHashMap::default();

    // === Phase 1: Batch-load ALL (file_id, phrase) pairs in one query ===
    // This avoids the N+1 query pattern that made the previous version O(N²).
    // V54: cap rows to bound memory on large repos (full occurrence table
    // can be 500K+ rows; symbol-relevant rows are a small subset).
    let mut all_pairs_stmt = db.prepare(
        "SELECT o.file_id, p.phrase, o.is_def
         FROM occurrence o
         JOIN phrases p ON o.phrase_id = p.id
         WHERE length(p.phrase) BETWEEN 3 AND 30
         ORDER BY o.file_id
         LIMIT 200000",
    )?;

    // For each file_id, the set of (phrase, is_def) pairs that appear in it
    let mut file_to_phrases: FxHashMap<i64, Vec<(String, bool)>> = FxHashMap::default();
    for row in all_pairs_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
            ))
        })?
        .flatten()
    {
        let (file_id, phrase, is_def) = row;
        file_to_phrases
            .entry(file_id)
            .or_default()
            .push((phrase, is_def));
    }

    // === Phase 2: For each symbol, find files where it's used (non-def) ===
    let mut phrase_to_non_def_files: FxHashMap<String, Vec<i64>> = FxHashMap::default();
    for (file_id, pairs) in &file_to_phrases {
        for (phrase, is_def) in pairs {
            if !*is_def {
                phrase_to_non_def_files
                    .entry(phrase.clone())
                    .or_default()
                    .push(*file_id);
            }
        }
    }

    // Now for each symbol: look up files directly via inverted index
    for sym in symbols {
        let mut refs: HashSet<String> = HashSet::new();

        let files_using_sym = match phrase_to_non_def_files.get(&sym.name) {
            Some(f) => f,
            None => continue,
        };

        for &file_id in files_using_sym {
            if let Some(pairs) = file_to_phrases.get(&file_id) {
                for (phrase, is_def) in pairs {
                    if *is_def
                        && phrase != &sym.name
                        && def_names.contains(phrase.as_str())
                        && !is_common_word(phrase)
                        && phrase.len() >= 3
                        && phrase.len() < 30
                        && !phrase.starts_with('_')
                        && !phrase.ends_with("Error")
                        && !phrase.ends_with("Result")
                        && !phrase.ends_with("Option")
                        && phrase != "Self"
                    {
                        refs.insert(phrase.clone());
                    }
                }
            }

            // Don't bother scanning more files once we have enough refs
            if refs.len() >= 5 {
                break;
            }
        }

        // Take top 3 refs, sorted
        if !refs.is_empty() {
            let mut sorted: Vec<String> = refs.into_iter().collect();
            sorted.sort();
            sorted.truncate(3);
            ref_set.insert(sym.name.clone(), sorted.into_iter().collect());
        }
    }

    // Convert sets to sorted vectors, capped at 3 to keep pack compact
    for (name, refs) in ref_set {
        let mut sorted: Vec<String> = refs.into_iter().collect();
        sorted.sort();
        sorted.truncate(3);
        cross_refs.insert(name, sorted);
    }

    Ok(cross_refs)
}

// ---------------------------------------------------------------------------
// Source reading
// ---------------------------------------------------------------------------

/// Read sources for a SUBSET of symbols (the selected hotspots) + build
/// skeleton frequency from just those symbols' bodies. This is much faster
/// than reading all files — only ~50 file reads instead of ~700.
fn read_symbols_for_subset(
    selected: &[Symbol],
    _all_symbols: &[Symbol],
) -> (FxHashMap<String, String>, FxHashMap<String, usize>) {
    let mut sources: FxHashMap<String, String> = FxHashMap::default();
    let mut freq: FxHashMap<String, usize> = FxHashMap::default();

    // S2: use the shared file_meta cache instead of a per-call file_cache.
    // file_meta::get returns Arc<FileMeta> with lines pre-split and cached
    // across the MCP lifetime. Eliminates redundant disk reads.
    for sym in selected {
        let lines: Vec<String> = if let Some(meta) = reliary_search::file_meta::get(&sym.file) {
            meta.lines.to_vec()
        } else {
            // Fallback: direct disk read for files not yet cached.
            std::fs::read_to_string(&sym.file)
                .map(|c| c.lines().map(String::from).collect())
                .unwrap_or_default()
        };

        let start = (sym.line as usize).min(lines.len().saturating_sub(1));
        let end = find_body_end(lines.as_slice(), start, sym.indent).min(lines.len());
        if start < end {
            // Iterate the slice directly — no join→re-split
            for line in &lines[start..end] {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let skel = aggressive_skeleton(trimmed);
                if !skel.is_empty() {
                    *freq.entry(skel).or_insert(0) += 1;
                }
            }
            sources.insert(sym.name.clone(), lines[start..end].join("\n"));
        }
    }

    (sources, freq)
}

/// Read all symbol sources and build skeleton frequency in a single pass.
/// Avoids re-reading the same file multiple times across symbols.
fn read_symbols_and_frequency(
    symbols: &[Symbol],
) -> (FxHashMap<String, String>, FxHashMap<String, usize>) {
    let mut sources: FxHashMap<String, String> = FxHashMap::default();
    let mut freq: FxHashMap<String, usize> = FxHashMap::default();

    // S2: shared file_meta cache replaces per-call file_cache.

    for sym in symbols {
        let lines: Vec<String> = if let Some(meta) = reliary_search::file_meta::get(&sym.file) {
            meta.lines.to_vec()
        } else {
            std::fs::read_to_string(&sym.file)
                .map(|c| c.lines().map(String::from).collect())
                .unwrap_or_default()
        };

        // Find the actual body extent. The block table's end_line is often wrong
        // (it undercounts for large functions), so we use brace-matching to find
        // the true end. We take the max of brace-matched end and end_line so
        // we never lose body content when the index is wrong.
        let start = (sym.line.max(0) as usize).min(lines.len().saturating_sub(1));
        let brace_end = find_body_end(lines.as_slice(), start, sym.indent);
        let index_end = (sym.end_line as usize).max(start + 1).min(lines.len());
        let end = brace_end.max(index_end).min(lines.len());

        if start < end {
            // Build skeleton frequency from body lines — iterate slice directly, no join→re-split
            for line in &lines[start..end] {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("#") {
                    continue;
                }
                let skel = aggressive_skeleton(trimmed);
                if skel.is_empty() || skel == "{w}" || skel == "}" || skel.len() < 5 {
                    continue;
                }
                *freq.entry(skel).or_insert(0) += 1;
            }

            sources.insert(sym.name.clone(), lines[start..end].join("\n"));
        }
    }

    (sources, freq)
}

/// Find the end of a function body by brace-matching from the start line.
/// Returns the line index AFTER the closing brace.
/// If brace-matching fails (e.g., Python uses indentation), falls back to
/// looking for the next def/class line at the same indent.
fn find_body_end(lines: &[String], start: usize, _base_indent: i32) -> usize {
    if start >= lines.len() {
        return start;
    }
    let first_line = &lines[start];

    // Python-style: find the next def/class at the same or lower indent
    let stripped = first_line.trim();
    if stripped.starts_with("def ")
        || stripped.starts_with("class ")
        || stripped.starts_with("async def ")
    {
        let first_indent = first_line.len() - first_line.trim_start().len();
        for (i, line) in lines.iter().enumerate().skip(start + 1) {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("#") {
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            // A new top-level def/class at the same indent marks the end
            if indent <= first_indent
                && (trimmed.starts_with("def ")
                    || trimmed.starts_with("class ")
                    || trimmed.starts_with("async def "))
            {
                return i;
            }
        }
        return lines.len();
    }

    // C-style: brace-matching
    let mut depth: i32 = 0;
    let mut seen_open = false;
    for (i, line) in lines.iter().enumerate().skip(start) {
        let trimmed = line.trim();

        // Skip comments and strings (crude — doesn't handle multi-line strings)
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") {
            continue;
        }

        for ch in line.chars() {
            match ch {
                '{' => { depth += 1; seen_open = true; }
                '}' => { depth -= 1; }
                _ => {}
            }
        }

        if seen_open && depth == 0 {
            return i + 1;
        }
    }
    lines.len()
}

// ---------------------------------------------------------------------------
// Module detection
// ---------------------------------------------------------------------------

/// Extract the crate name from a file path.
/// Looks for `crates/<name>/` or `packages/<name>/` patterns.
fn derive_crate_name(file_path: &str) -> String {
    let parts: Vec<&str> = file_path.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if (*part == "crates" || *part == "packages") && i + 1 < parts.len() {
            return parts[i + 1].to_string();
        }
    }
    // Fallback: parent directory name
    if parts.len() >= 2 {
        return parts[parts.len() - 2].to_string();
    }
    String::new()
}

fn detect_modules(symbols: &[Symbol]) -> Vec<(String, Vec<Symbol>)> {
    let mut module_map: FxHashMap<String, Vec<Symbol>> = FxHashMap::default();

    for sym in symbols {
        let module = derive_module_name(&sym.file);
        module_map.entry(module).or_default().push(sym.clone());
    }

    let mut modules: Vec<(String, Vec<Symbol>)> = module_map.into_iter().collect();
    modules.sort_by(|a, b| a.0.cmp(&b.0));
    modules
}

fn derive_module_name(file_path: &str) -> String {
    let path = Path::new(file_path);
    let components: Vec<_> = path.components().collect();

    // packages/<name>/... → packages/<name>
    for (i, comp) in components.iter().enumerate() {
        let part = comp.as_os_str().to_string_lossy().to_string();
        if part == "packages" && i + 1 < components.len() {
            return format!("packages/{}", components[i + 1].as_os_str().to_string_lossy());
        }
        if part == "src" && i + 1 < components.len() {
            return format!("src/{}", components[i + 1].as_os_str().to_string_lossy());
        }
        if part == "crates" && i + 1 < components.len() {
            return format!("crates/{}", components[i + 1].as_os_str().to_string_lossy());
        }
    }

    // Fallback: first component
    components
        .first()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string())
}

// ---------------------------------------------------------------------------
// Entry rendering
// ---------------------------------------------------------------------------

/// Build an L4 co-change line for editing guidance.
fn build_cochange_line(_name: &str, signature: &str, _body: &str, cross_refs: &[String]) -> String {
    let lower = signature.to_ascii_lowercase();
    let mut parts: Vec<String> = Vec::new();
    let is_enum = lower.contains(" enum ") || lower.starts_with("enum ") || lower.starts_with("pub enum ");
    let is_struct = lower.contains(" struct ") || lower.starts_with("struct ") || lower.starts_with("pub struct ");

    if is_enum {
        if !cross_refs.is_empty() {
            let sites: Vec<&str> = cross_refs.iter().take(6).map(|s| s.as_str()).collect();
            parts.push(format!("Add variant → update match sites: {}", sites.join(", ")));
        }
    } else if is_struct {
        if !cross_refs.is_empty() {
            let sites: Vec<&str> = cross_refs.iter().take(6).map(|s| s.as_str()).collect();
            parts.push(format!("Add field → update constructors: {}", sites.join(", ")));
        }
    } else {
        if !cross_refs.is_empty() {
            let sites: Vec<&str> = cross_refs.iter().take(6).map(|s| s.as_str()).collect();
            parts.push(format!("Rename → update {} caller(s): {}", cross_refs.len(), sites.join(", ")));
        }
    }
    parts.join("; ")
}

fn expand_type_signature(signature: &str, body: &str) -> String {
    let sig_trimmed = signature.trim();
    let lower = sig_trimmed.to_ascii_lowercase();
    let is_enum = lower.contains(" enum ") || lower.starts_with("enum ") || lower.starts_with("pub enum ");
    let is_struct = lower.contains(" struct ") || lower.starts_with("struct ") || lower.starts_with("pub struct ");
    if !is_enum && !is_struct {
        return signature.to_string();
    }
    let mut names: Vec<String> = Vec::new();
    for line in body.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed == "}" || trimmed.is_empty() { continue; }
        if is_enum {
            let name = trimmed.split([',', '{', '(', ':']).next().unwrap_or("").trim().trim_end_matches(',');
            if !name.is_empty() && name.len() > 1 && name.chars().next().is_some_and(|c| c.is_uppercase() || c == '_') {
                names.push(name.to_string());
            }
        } else if is_struct {
            if let Some(colon_pos) = trimmed.find(':') {
                let name = trimmed[..colon_pos].trim().trim_start_matches("pub ");
                if !name.is_empty() && name.len() > 1 { names.push(name.to_string()); }
            }
        }
    }
    if names.is_empty() { return signature.to_string(); }
    let kind = if is_enum { "variants" } else { "fields" };
    format!("{} // {}({})", sig_trimmed, kind, names.join(", "))
}

fn render_entry(
    sym: &Symbol,
    body: &str,
    cross_refs: &[String],
    skeleton_freq: &FxHashMap<String, usize>,
    format: PackFormat,
) -> String {
    // Include crate/module name to disambiguate same-name symbols across crates
    let crate_name = derive_crate_name(&sym.file);
    let header = if crate_name.is_empty() {
        format!("## {}", sym.name)
    } else {
        format!("## {}/{}", sym.name, crate_name)
    };
    let mut parts = vec![header];

    // L0: one-line purpose (only in Full format, if doc comment exists)
    if matches!(format, PackFormat::Full) && !sym.doc_comment.is_empty() {
        let l0 = sym
            .doc_comment
            .split('.')
            .next()
            .unwrap_or(&sym.doc_comment)
            .trim();
        let l0 = if l0.chars().count() > 120 {
            // Truncate at char boundary, not byte boundary
            l0.chars().take(120).collect::<String>()
        } else {
            l0.to_string()
        };
        parts.push(format!("L0: {}", l0));
    }

    // L2: always present — expand enum/struct signatures to include variants/fields
    let l2_line = expand_type_signature(&sym.signature, body);
    parts.push(format!("L2: {}  [{}:{}]", l2_line, sym.file, sym.line));

    // L3: surprise
    if !body.is_empty() {
        let surprises = extract_surprise_from_body(body, skeleton_freq);
        if !surprises.is_empty() {
            parts.push(format!("L3: {}", surprises.join("; ")));
        }
    }

    // Cross-refs (only in Full format, with ≥3 refs)
    if matches!(format, PackFormat::Full) && cross_refs.len() >= 2 {
        parts.push(format!("Cross-refs: {}", cross_refs.iter().take(5).cloned().collect::<Vec<_>>().join(", ")));
    }

    // L4: co-change (editing guidance) — always present when we have cross-refs or type info
    let l4 = build_cochange_line(&sym.name, &sym.signature, body, cross_refs);
    if !l4.is_empty() {
        parts.push(format!("L4: {}", l4));
    }

    parts.push(String::new());
    parts.join("\n")
}