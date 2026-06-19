//! Novel compression mechanisms — proxy-side, invisible to the LLM.
//!
//! 1. Cache-hit feedback loop: adapt compression aggressiveness from API cache metrics.
//! 2. Stream-aware prefetch: parse SSE chunks for file paths, pre-load reads.
//! 3. Maxwell's Demon: information-theoretic token erasure (cost = -log2(freq)).
//! 4. Asymmetric tool-call compression: preserve results, compress requests.
//! 5. Invariant hoisting: JSON arrays → header + delta rows.
//! 6. Dialogue State Ledger: extract [STATE] from old reasoning.

use rustc_hash::FxHashMap;
use serde_json::Value;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Instant;

// ── 1. Cache-Hit Feedback Loop ──

// Tracks cache hit ratio across turns for each auth key.
// When ratio drops below 30%, compression pauses for N turns to let the cache rebuild.
// When ratio rises above 80%, compression resumes.
struct CacheFeedback {
    /// Rolling window of (prompt_cache_hit_tokens, prompt_tokens) per turn
    hit_ratios: Vec<f64>,
    /// Number of consecutive turns we've hit the pause threshold
    pause_count: u32,
    /// When the current pause started (if paused)
    paused_since: Option<Instant>,
    /// Maximum pause duration (5 seconds — enough for cache rebuild)
    pause_duration: std::time::Duration,
}

impl CacheFeedback {
    fn new() -> Self {
        Self {
            hit_ratios: Vec::with_capacity(32),
            pause_count: 0,
            paused_since: None,
            pause_duration: std::time::Duration::from_secs(5),
        }
    }

    /// Feed cache metrics from upstream response. Returns `true` if compression should be paused.
    fn record_turn(&mut self, hit_tokens: u32, total_prompt_tokens: u32) -> bool {
        if total_prompt_tokens == 0 {
            return self.is_paused();
        }
        let ratio = hit_tokens as f64 / total_prompt_tokens as f64;
        self.hit_ratios.push(ratio);
        if self.hit_ratios.len() > 32 {
            self.hit_ratios.remove(0);
        }

        // Check if we should pause or resume
        let recent_avg = self.hit_ratios.iter().rev().take(5).sum::<f64>()
            / self.hit_ratios.len().min(5) as f64;

        if recent_avg < 0.30 {
            // Below 30% cache hit — compression is busting the cache. Pause.
            self.pause_count += 1;
            if self.paused_since.is_none() {
                self.paused_since = Some(Instant::now());
            }
            true
        } else if recent_avg > 0.80 && self.paused_since.is_some() {
            // Above 80% again — cache is stable, resume compression
            self.paused_since = None;
            self.pause_count = 0;
            false
        } else if self.paused_since.is_some() {
            // Still paused, check if pause duration expired
            if self.paused_since.unwrap().elapsed() > self.pause_duration {
                self.paused_since = None;
                self.pause_count = 0;
                false // Resume regardless after timeout
            } else {
                true // Stay paused
            }
        } else {
            false // Normal operation
        }
    }

    fn is_paused(&self) -> bool {
        self.paused_since.is_some()
    }
}

static CACHE_FEEDBACK: LazyLock<Mutex<FxHashMap<String, CacheFeedback>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));

/// Feed cache metrics from upstream response. Returns true if compression should pause.
pub fn feed_cache_metrics(auth_key: &str, hit_tokens: u32, total_prompt_tokens: u32) -> bool {
    let mut map = CACHE_FEEDBACK.lock().unwrap_or_else(|e| e.into_inner());
    let fb = map.entry(auth_key.to_string()).or_insert_with(CacheFeedback::new);
    fb.record_turn(hit_tokens, total_prompt_tokens)
}

// ── 4. Invariant Hoisting for JSON ──

// Parses SSE chunks in real-time for file path mentions.
// When a chunk contains a path like "src/main.rs", prefetch it into the read cache.
// Uses an LRU of recently-prefetched paths to avoid duplicate work.

use std::collections::VecDeque;

const PREFETCH_HISTORY: usize = 32;
const MAX_PREFETCH_FILES: usize = 3;

struct PrefetchTracker {
    recently_prefetched: VecDeque<String>,
    prefetch_count: usize,
}

impl PrefetchTracker {
    fn new() -> Self {
        Self { recently_prefetched: VecDeque::with_capacity(PREFETCH_HISTORY), prefetch_count: 0 }
    }

    fn try_prefetch(&mut self, chunk: &str, workdir: &str, _state: &crate::session_state::SessionState) {
        // Only check chunks that contain likely file path patterns
        if !chunk.contains('/') && !chunk.contains(".rs") && !chunk.contains(".py")
            && !chunk.contains(".ts") && !chunk.contains(".go") && !chunk.contains(".js")
        {
            return;
        }

        // Extract file paths from the chunk using simple heuristic:
        // Look for patterns like `src/foo.rs`, `./lib.rs`, `crates/bar/src/main.rs`
        let mut found: Vec<String> = Vec::new();
        for word in chunk.split_whitespace() {
            let cleaned = word.trim_matches(|c: char| c == '\'' || c == '"' || c == '`'
                || c == '(' || c == ')' || c == '[' || c == ']'
                || c == '{' || c == '}' || c == ',' || c == '.');
            if cleaned.contains('/') {
                // Check it looks like a source file path
                let has_source_ext = cleaned.ends_with(".rs") || cleaned.ends_with(".py")
                    || cleaned.ends_with(".ts") || cleaned.ends_with(".js")
                    || cleaned.ends_with(".go") || cleaned.ends_with(".java")
                    || cleaned.ends_with(".rs:") || cleaned.ends_with(".py:");
                if has_source_ext && !self.recently_prefetched.contains(&cleaned.to_string()) {
                    found.push(cleaned.to_string());
                }
            }
        }

        if found.is_empty() { return; }

        // Prefetch up to MAX_PREFETCH_FILES files
        let full_paths: Vec<String> = found.iter().take(MAX_PREFETCH_FILES)
            .map(|f| {
                if f.starts_with('/') { f.clone() }
                else if f.contains(":") { f.split(':').next().unwrap_or(f).to_string() }
                else { format!("{}/{}", workdir.trim_end_matches('/'), f) }
            })
            .collect();

        for pf in &full_paths {
            self.recently_prefetched.push_back(pf.clone());
            if self.recently_prefetched.len() > PREFETCH_HISTORY {
                self.recently_prefetched.pop_front();
            }
            self.prefetch_count += 1;
            std::fs::read_to_string(pf).ok(); // Warm the OS page cache
        }
    }
}

static PREFETCH_TRACKER: LazyLock<Mutex<PrefetchTracker>> =
    LazyLock::new(|| Mutex::new(PrefetchTracker::new()));

/// Call from the streaming loop with each SSE chunk.
/// Extracts file paths and pre-fetches them.
pub fn try_prefetch(chunk: &str) {
    // Determine workdir from the chunk (look for `/tmp/` or `/home/` paths)
    let workdir = if let Some(pos) = chunk.find("/tmp/") {
        let end = chunk[pos + 5..].find('/').map(|e| pos + 5 + e).unwrap_or(chunk.len() - pos);
        chunk[pos..pos + end + chunk[pos + 5..].len()].to_string()
    } else if let Some(pos) = chunk.find("/home/") {
        // Extract up to first 5 path segments
        let rest = &chunk[pos..];
        let segments: Vec<&str> = rest.split('/').collect();
        format!("/{}/{}/{}/{}", segments[1], segments[2], segments[3], segments[4])
    } else {
        ".".to_string()
    };

    // Get daemon state for cache warming (best-effort)
    let state = crate::proxy::get_state();
    let mut tracker = PREFETCH_TRACKER.lock().unwrap_or_else(|e| e.into_inner());
    tracker.try_prefetch(chunk, &workdir, &state);
}

// ── 3. Maxwell's Demon: Entropy-Budgeted Erasure ──

/// Tokenize text into words and estimate per-token frequency from a local corpus.
/// Cost = -log2(p(token)). Erase cheapest tokens first until budget exhausted.
pub fn maxwell_compress(text: &str, budget_bits: f64) -> Option<String> {
    if text.len() < 100 { return None; }
    if !text.contains(char::is_whitespace) { return None; }

    // Tokenize on whitespace
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 10 { return None; }

    // Estimate frequency from the text itself (zipf-like: rank-based approximation)
    let mut freq_map: FxHashMap<&str, usize> = FxHashMap::default();
    for w in &words {
        *freq_map.entry(w).or_insert(0) += 1;
    }
    let total = words.len() as f64;

    // Score each word: cost = -log2(freq / total)
    struct WordCost {
        word: String,
        cost: f64,
        idx: usize,
    }
    let mut scored: Vec<WordCost> = words.iter().enumerate().map(|(i, w)| {
        let p = *freq_map.get(w).unwrap_or(&1) as f64 / total;
        let cost = -p.log2(); // High cost = rare/informative, low cost = common/boilerplate
        WordCost { word: w.to_string(), cost, idx: i }
    }).collect();

    // Sort by cost ascending (cheapest first — most erasure-worthy)
    scored.sort_by(|a, b| a.cost.partial_cmp(&b.cost).unwrap_or(std::cmp::Ordering::Equal));

    // Erase cheapest tokens until budget exhausted
    let mut erased = vec![false; words.len()];
    let mut consumed = 0.0f64;
    for wc in &scored {
        if consumed >= budget_bits { break; }
        // Protect short tokens (likely connectors/a/an/the)
        if wc.word.len() <= 2 { continue; }
        // Protect tokens with digits or special chars (error codes, line numbers)
        if wc.word.contains(|c: char| c.is_ascii_digit() || "(){}[]<>=:;".contains(c)) { continue; }
        erased[wc.idx] = true;
        consumed += wc.cost;
    }

    // Reconstruct text, replacing erased tokens with a single placeholder
    let mut result = Vec::new();
    let mut in_gap = false;
    for (i, w) in words.iter().enumerate() {
        if erased[i] {
            if !in_gap {
                result.push("_");
                in_gap = true;
            }
        } else {
            result.push(w);
            in_gap = false;
        }
    }

    let compressed = result.join(" ");
    if compressed.len() < text.len() {
        Some(compressed)
    } else {
        None
    }
}

// ── 4. Asymmetric Tool-Call Compression ──

/// Detect repeated JSON object arrays (like tool results, config lists).
/// Hoist common fields to a header, emit each row as compact delta.
/// Works entirely at the string level — no AST parsing, just string heuristics.
pub fn hoist_json_invariants(content: &str) -> Option<String> {
    // Must be a JSON array with objects
    let trimmed = content.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }

    // Must be large enough to benefit
    if content.len() < 500 { return None; }

    // Try to parse as JSON array
    let parsed: Value = serde_json::from_str(content).ok()?;
    let arr = parsed.as_array()?;
    if arr.len() < 3 { return None; }

    // Extract all keys from the first object
    let first_obj = arr[0].as_object()?;
    let all_keys: Vec<&str> = first_obj.keys().map(|k| k.as_str()).collect();
    if all_keys.len() < 2 { return None; }

    // Find keys that are invariant across ALL objects (same value in every row)
    let mut invariant_keys: Vec<(&str, String)> = Vec::new();
    for k in &all_keys {
        let first_val = &arr[0][k];
        let all_same = arr.iter().all(|obj| obj.get(*k) == Some(first_val));
        if !all_same { continue; }
        let val_str = match first_val {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        // Skip long values (don't hoist big blobs)
        if val_str.len() > 50 { continue; }
        invariant_keys.push((k, val_str));
    }

    if invariant_keys.is_empty() { return None; }
    let ratio = invariant_keys.len() as f64 / all_keys.len() as f64;
    if ratio < 0.1 { return None; }

    // Build output: [S] key=val,key=val ... then per-row: key=val,key=val ...
    let header = invariant_keys.iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(",");

    let mut result = String::with_capacity(content.len() / 2);
    result.push_str(&format!("[S] {}\n", header));

    for (i, obj) in arr.iter().enumerate() {
        let obj_map = obj.as_object()?;
        let diff: Vec<String> = obj_map.iter()
            .filter(|(k, _)| !invariant_keys.iter().any(|(ik, _)| *ik == k.as_str()))
            .map(|(k, v)| {
                let vs = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("{}={}", k, vs)
            })
            .collect();
        result.push_str(&format!("{}:{}\n", i + 1, diff.join(",")));
    }

    if result.len() < content.len() {
        Some(result)
    } else {
        None
    }
}

// ── 6. Dialogue State Ledger ──

/// Extract workflow state from verbose assistant reasoning.
/// Looks for patterns like:
///   "goal: fix the merge function"
///   "trying to compile processor.rs"
///   "the error is on line 42"
///   "next, I need to check the imports"
///
/// Output format: `[STATE] g=...,b=...,cmd=...,res=...,next=...`
/// Using full words for LLM readability.
pub fn extract_dialogue_state(content: &str) -> Option<String> {
    // Must be assistant reasoning (prose, not code)
    if content.len() < 200 { return None; }
    // Skip content with code blocks (too structured to extract state from)
    if content.contains("```") { return None; }
    if content.contains("//") || content.contains("/*") { return None; }

    let _lower = content.to_lowercase();
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < 5 { return None; }

    let mut goal = String::new();
    let mut blocker = String::new();
    let mut last_command = String::new();
    let mut last_result = String::new();
    let mut next_action = String::new();

    // Extract goal: look for "goal:", "aim:", "need to", "let me", "going to"
    for line in &lines {
        let l = line.trim().to_lowercase();
        if l.starts_with("goal:") || l.starts_with("aim:") || l.starts_with("objective:") {
            let rest = line.trim().split_once(':').map(|x| x.1).unwrap_or("").trim().to_string();
            if !rest.is_empty() && rest.len() < 100 {
                goal = rest.chars().take(80).collect();
                break;
            }
        }
    }
    if goal.is_empty() {
        // Fallback: look for "going to" in first sentence
        for line in &lines {
            let l = line.to_lowercase();
            for phrase in &["i will", "i need to", "i should", "let me", "going to fix"] {
                if let Some(pos) = l.find(phrase) {
                    let rest = &l[pos + phrase.len()..];
                    let end = rest.find(['.', ';']).unwrap_or(rest.len().min(60));
                    goal = rest[..end].trim().chars().take(60).collect();
                    break;
                }
            }
            if !goal.is_empty() { break; }
        }
    }

    // Extract blocker: look for "issue:", "problem:", "error:", "bug:", "blocker:"
    for line in &lines {
        let l = line.trim().to_lowercase();
        for prefix in &["issue:", "problem:", "error:", "bug:", "blocker:", "failing:"] {
            if l.starts_with(prefix) {
                let rest = line.trim().split_once(':').map(|x| x.1).unwrap_or("").trim().to_string();
                if !rest.is_empty() && rest.len() < 100 {
                    blocker = rest.chars().take(80).collect();
                    break;
                }
            }
        }
        if !blocker.is_empty() { break; }
    }
    if blocker.is_empty() {
        // Fallback: look for error code or "fails", "breaks"
        for line in &lines {
            if line.contains("E0") || line.contains("error[E") || line.contains("FAILED") {
                blocker = line.trim().chars().take(80).collect();
                break;
            }
        }
    }

    // Extract last command: look for patterns like "ran cargo build", "running cargo test", "trying:"
    for line in &lines {
        let l = line.trim().to_lowercase();
        for prefix in &["ran cargo", "running cargo", "trying cargo", "ran `", "running `", "trying `",
                        "`cargo", "`pytest", "`npm", "`go test"] {
            if l.contains(prefix) {
                let clean = line.trim().trim_matches('`');
                let end = clean.find(['.', ';', '\n']).unwrap_or(clean.len().min(80));
                last_command = clean[..end].trim().to_string();
                break;
            }
        }
        if !last_command.is_empty() { break; }
    }
    if last_command.is_empty() {
        // Fallback: look for "cargo" or "pytest" or "npm"
        for line in &lines {
            for cmd in &["cargo", "pytest", "npm", "go test", "make"] {
                if let Some(pos) = line.to_lowercase().find(cmd) {
                    let end = line[pos..].find(['.', ';', '\n']).unwrap_or(line[pos..].len().min(60));
                    last_command = line[pos..pos + end].trim().chars().take(60).collect();
                    break;
                }
            }
            if !last_command.is_empty() { break; }
        }
    }

    // Extract result: look for "result:", "passed", "failed", "completed", error codes
    for line in &lines {
        let l = line.trim().to_lowercase();
        for prefix in &["result:", "output:", "returned:", "complete", "finished"] {
            if l.starts_with(prefix) || l.starts_with(prefix.trim_end_matches(':')) {
                let rest = line.trim().split_once(':').map(|x| x.1).unwrap_or("").trim().to_string();
                if !rest.is_empty() && rest.len() < 100 {
                    last_result = rest.chars().take(80).collect();
                    break;
                }
            }
        }
        if !last_result.is_empty() { break; }
    }
    if last_result.is_empty() {
        // Look for "passed", "failed", "error[E", "0 failed", "1 failed"
        for line in &lines {
            let l = line.to_lowercase();
            if l.contains("failed") || l.contains("passed") || l.contains("error[E") {
                last_result = line.trim().chars().take(80).collect();
                break;
            }
        }
    }

    // Extract next action: look for "next:", "then I'll", "next step", "next,", "finally"
    for line in lines.iter().rev() {
        let l = line.trim().to_lowercase();
        for prefix in &["next:", "next step:", "final step:", "then i", "next i", "finally"] {
            if l.starts_with(prefix) || l.contains(prefix) {
                let after = if let Some(pos) = l.find(prefix) {
                    &l[pos + prefix.len()..]
                } else { "" };
                let end = after.find(['.', ';']).unwrap_or(after.len().min(60));
                next_action = after[..end].trim().chars().take(60).collect();
                break;
            }
        }
        if !next_action.is_empty() { break; }
    }

    // Only output if we found at least 2 fields
    let fields = [goal.as_str(), blocker.as_str(), last_command.as_str(), last_result.as_str(), next_action.as_str()]
        .iter().filter(|s| !s.is_empty()).count();
    if fields < 2 { return None; }

    // Build compact DSL with full-word keys
    let mut parts: Vec<String> = Vec::new();
    if !goal.is_empty() { parts.push(format!("goal={}", goal)); }
    if !blocker.is_empty() { parts.push(format!("blocker={}", blocker)); }
    if !last_command.is_empty() { parts.push(format!("command={}", last_command)); }
    if !last_result.is_empty() { parts.push(format!("result={}", last_result)); }
    if !next_action.is_empty() { parts.push(format!("next={}", next_action)); }

    let dsl = format!("[STATE] {}", parts.join(" | "));

    if dsl.len() < content.len() {
        Some(dsl)
    } else {
        None
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_feedback_pause_resume() {
        let mut fb = CacheFeedback::new();
        // Low hit rate → pause (recent_avg < 0.30 for first 3 turns)
        assert!(fb.record_turn(10, 100)); // 10% → paused
        assert!(fb.record_turn(20, 100)); // 20% → paused
        assert!(fb.record_turn(30, 100)); // 30% → paused
        // Now fill the 5-turn window with high ratios
        assert!(fb.record_turn(85, 100)); // stays paused (window avg still low)
        assert!(fb.record_turn(85, 100));
        assert!(fb.record_turn(85, 100));
        assert!(fb.record_turn(85, 100));
        // Turn 8: window is [85,85,85,85,85] → avg 0.85 > 0.80 → resume
        assert!(!fb.record_turn(85, 100)); // resumed
        assert!(!fb.is_paused());
    }

    #[test]
    fn test_maxwell_compress_empty_short() {
        assert_eq!(maxwell_compress("hello", 50.0), None);
        assert_eq!(maxwell_compress("", 50.0), None);
    }

    #[test]
    fn test_maxwell_compress_long_prose() {
        let text = "Let me think about this problem. I should consider the best approach \
            for fixing the merge function. The issue is that we're using the wrong comparison operator. \
            We need to change the less-than-or-equal to a strict less-than. \
            This will fix the off-by-one error when the array has duplicate values. \
            Let me check the code and implement the fix.";
        let result = maxwell_compress(text, 50.0);
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.len() < text.len());
        assert!(r.contains("merge") || r.contains("off-by-one") || r.contains("fix"));
    }

    #[test]
    fn test_dialogue_state_basic() {
        let text = "I need to fix the merge function in sort_utils.rs.\n\
            The error is E0308 type mismatch on line 42.\n\
            I ran cargo build and it failed with a type error.\n\
            The result was a compilation error showing the mismatch.\n\
            Next, I should check the function signature and fix the comparison.";
        let result = extract_dialogue_state(text);
        assert!(result.is_some(), "Didn't extract state from: {}", text);
        let r = result.unwrap();
        assert!(r.contains("goal="));
        assert!(r.contains("blocker=") || r.contains("command=") || r.contains("result="));
        assert!(r.starts_with("[STATE]"));
        assert!(r.len() < text.len());
    }

    #[test]
    fn test_hoist_json_invariants() {
        // Repeat enough to exceed 500 chars
        let json = format!("[{}]", (0..8).map(|i| {
            format!(r#"{{"name": "mod{}", "type": "module", "lang": "rust", "path": "./src/mod{}"}}"#, i, i)
        }).collect::<Vec<_>>().join(","));
        assert!(json.len() > 500, "test data must exceed 500 chars");
        let result = hoist_json_invariants(&json);
        assert!(result.is_some(), "Didn't hoist invariants from: {}", json);
        let r = result.unwrap();
        assert!(r.contains("[S]"));
        assert!(r.contains("lang="));
        assert!(r.len() < json.len());
    }

    #[test]
    fn test_hoist_json_no_invariants() {
        // 10 entries with all different values — nothing to hoist
        let json = format!("[{}]", (0..12).map(|i| {
            format!(r#"{{"name": "syn{}", "version": "{}", "author": "{}"}}"#, i, i + 1, char::from_u32(65 + i as u32).unwrap_or('a'))
        }).collect::<Vec<_>>().join(","));
        assert!(json.len() > 500, "test data must exceed 500 chars (got {})", json.len());
        let result = hoist_json_invariants(&json);
        assert!(result.is_none(), "Expected None (all different) but got: {:?}", result.as_deref().unwrap_or("").chars().take(50).collect::<String>());
    }

    #[test]
    fn test_prefetch_tracker() {
        let mut pt = PrefetchTracker::new();
        let state = crate::session_state::SessionState::new(".");
        pt.try_prefetch("I should check src/main.rs for the bug", ".", &state);
        assert!(pt.recently_prefetched.iter().any(|p| p.contains("src/main.rs")));
    }

    #[test]
    fn test_dialogue_state_empty() {
        assert_eq!(extract_dialogue_state("short"), None);
        assert_eq!(extract_dialogue_state("```\ncode\n```"), None);
    }
}
