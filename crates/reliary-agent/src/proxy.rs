//! Provider-agnostic proxy with axum — true SSE streaming support.
// Auth-based routing via routes.rs. No model lists, no provider detection.

use axum::{
    Router, extract::Query, http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use bytes::Bytes;

use rustc_hash::FxHashMap;
use std::collections::HashMap as StdHashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, LazyLock};
use std::time::Instant;
use serde_json::Value;
use tracing::{info, warn, error};

// Shared HTTP client with connection pooling — eliminates per-request TCP+TLS handshake.
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    // Bug 69: total request timeout (default 5 min) to prevent hung upstream
    // requests from leaking memory and FDs.
    let timeout_secs: u64 = std::env::var("RELIARY_PROXY_UPSTREAM_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    reqwest::Client::builder()
        .pool_max_idle_per_host(10)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .expect("reqwest::Client")
});

// Compression dictionary loaded once from FTS5 index — known project symbols
// survive compression while unknown fluff gets stripped.
static COMPRESSION_DICT: LazyLock<Option<reliary_compress::CompressionDict>> =
    LazyLock::new(crate::read_summary::load_dictionary);

// Synchronization for JSONL logging — prevents interleaved lines from concurrent requests.
// JSONL log: persistent file handle (Bug 53 fix).
// Opens /tmp/reliary_proxy.jsonl once and reuses for all subsequent writes.
static JSONL_FILE: LazyLock<Mutex<Option<std::fs::File>>> = LazyLock::new(|| Mutex::new(None));

// Response cache with proper LRU eviction (Bug 52 fix).
// Stores (cached_body, insertion_sequence) to enable oldest-first eviction.
static RESPONSE_CACHE: LazyLock<Mutex<FxHashMap<u64, (String, u64)>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));
static RESPONSE_CACHE_SEQ: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));
const RESPONSE_CACHE_MAX: usize = 120;

static DAEMON_STATE: LazyLock<Mutex<Option<Arc<crate::session_state::SessionState>>>> =
    LazyLock::new(|| Mutex::new(None));

// Guard result cache: keyed by (file_path_hash, content_hash), 60s TTL.
// Prevents redundant FTS5 queries on retry loops.
struct GuardCacheEntry {
    status: String,
    inserted_at: Instant,
}
static GUARD_CACHE: LazyLock<Mutex<FxHashMap<u64, GuardCacheEntry>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));

pub fn get_state() -> Arc<crate::session_state::SessionState> {
    let guard = DAEMON_STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard.clone().unwrap_or_else(|| Arc::new(crate::session_state::SessionState::new(".")))
}

/// Bug 68: infer the request's workdir by looking for any absolute file path
/// in the messages and finding the nearest .reliary/ ancestor.
/// Returns None if no path can be inferred.
fn infer_request_workdir(payload: &serde_json::Value) -> Option<String> {
    let messages = payload.get("messages")?.as_array()?;
    // Scan for any absolute file path in tool_calls or content
    for msg in messages {
        if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
            // Find absolute paths (heuristic: starts with /)
            for word in content.split_whitespace() {
                let path = word.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ';' || c == ')');
                if path.starts_with('/') && std::path::Path::new(path).exists() {
                    if let Some((root, _, _)) = crate::daemon::find_reliary_root(path) {
                        return Some(root);
                    }
                }
            }
        }
        // Check tool_calls function arguments for file paths
        if let Some(tool_calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tool_calls {
                if let Some(args) = tc.get("function").and_then(|f| f.get("arguments")).and_then(|a| a.as_str()) {
                    for word in args.split_whitespace() {
                        let path = word.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ';' || c == ')');
                        if path.starts_with('/') && std::path::Path::new(path).exists() {
                            if let Some((root, _, _)) = crate::daemon::find_reliary_root(path) {
                                return Some(root);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn cache_key(auth: &str, body: &str, is_streaming: bool, model: &str) -> u64 {
    use rustc_hash::FxHasher;
    let mut h = FxHasher::default();
    auth.hash(&mut h);
    body.hash(&mut h);
    is_streaming.hash(&mut h);
    // Bug 58: include model in cache key (was missing — same messages but different
    // model would return wrong model's cached response).
    model.hash(&mut h);
    h.finish()
}

fn cached_response(auth: &str, body: &str, is_streaming: bool, model: &str) -> Option<String> {
    let key = cache_key(auth, body, is_streaming, model);
    RESPONSE_CACHE.lock().ok().and_then(|c| c.get(&key).map(|(s, _)| s.clone()))
}

fn store_response(auth: &str, body: &str, response: &str, is_streaming: bool, model: &str) {
    let key = cache_key(auth, body, is_streaming, model);
    if let Ok(mut cache) = RESPONSE_CACHE.lock() {
        // Bug 52: proper LRU — track insertion sequence, evict oldest when over cap
        let seq = RESPONSE_CACHE_SEQ.lock().map(|mut s| { *s += 1; *s }).unwrap_or(0);
        cache.insert(key, (response.to_string(), seq));
        if cache.len() > RESPONSE_CACHE_MAX {
            // Find the entry with the smallest seq (= oldest)
            if let Some(&oldest_key) = cache.iter().min_by_key(|(_, (_, seq))| *seq).map(|(k, _)| k) {
                cache.remove(&oldest_key);
            }
        }
    }
}

fn resolve_upstream(auth_key: &str) -> Option<String> {
    if let Some(url) = crate::routes::discover_upstream(auth_key) {
        // Bug 66: validate URL scheme is http or https (reject file://, gopher://, etc.)
        if url.starts_with("http://") || url.starts_with("https://") {
            return Some(url);
        }
        return None;
    }
    if let Ok(url) = std::env::var("RELIARY_UPSTREAM_URL") {
        if url.starts_with("http://") || url.starts_with("https://") {
            return Some(url);
        }
    }
    None
}

fn extract_auth_key(headers: &HeaderMap) -> String {
    headers.get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            // Bug 63: case-insensitive "Bearer" / "bearer" / "BEARER" prefix support
            let key = if v.len() >= 7 && v[..7].eq_ignore_ascii_case("bearer ") {
                v[7..].to_string()
            } else {
                v.to_string()
            };
            // Bug 70: reject very long keys (likely malicious or malformed)
            if key.len() > 1024 { String::new() } else { key }
        })
        .unwrap_or_default()
}

// ── Rate Limiting (Bug 49) ──
//
// Per-auth-key token bucket. Defaults to 60 requests / 60 seconds.
// If a single API key makes more than 60 requests in 60s, requests are
// rejected with HTTP 429. Configurable via RELIARY_PROXY_RATE_PER_MIN.

const RATE_LIMIT_PER_MIN_DEFAULT: u32 = 60;
const RATE_BUCKETS_MAX: usize = 1000; // Bug 67: cap to bound memory under attack
static RATE_BUCKETS: LazyLock<Mutex<rustc_hash::FxHashMap<String, (u32, std::time::Instant)>>> =
    LazyLock::new(|| Mutex::new(rustc_hash::FxHashMap::default()));
static RATE_LAST_PRUNE: LazyLock<Mutex<std::time::Instant>> =
    LazyLock::new(|| Mutex::new(std::time::Instant::now()));
const RATE_PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

fn check_rate_limit(auth_key: &str) -> Option<u32> {
    if auth_key.is_empty() { return None; } // No key, no rate limit (let auth check handle it)

    let per_min = std::env::var("RELIARY_PROXY_RATE_PER_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(RATE_LIMIT_PER_MIN_DEFAULT);

    let now = std::time::Instant::now();
    let mut buckets = RATE_BUCKETS.lock().unwrap_or_else(|e| e.into_inner());

    // Periodic sweep: remove entries older than 60s to prevent memory growth
    {
        let mut last_prune = RATE_LAST_PRUNE.lock().unwrap_or_else(|e| e.into_inner());
        if now.duration_since(*last_prune) > RATE_PRUNE_INTERVAL {
            buckets.retain(|_, (_, t)| now.duration_since(*t) < std::time::Duration::from_secs(60));
            *last_prune = now;
        }
    }

    // Bug 67: hard cap on bucket count. If at cap, reject new entries.
    if !buckets.contains_key(auth_key) && buckets.len() >= RATE_BUCKETS_MAX {
        return Some(60); // Reject until next sweep
    }

    let entry = buckets.entry(auth_key.to_string()).or_insert((0, now));
    // Reset bucket if more than 60s have passed
    if now.duration_since(entry.1) > std::time::Duration::from_secs(60) {
        entry.0 = 0;
        entry.1 = now;
    }
    if entry.0 >= per_min {
        let retry_after = 60u32.saturating_sub(now.duration_since(entry.1).as_secs() as u32);
        return Some(retry_after.max(1));
    }
    entry.0 += 1;
    None
}

fn daemon_cmd_str(cmd: &str) -> String {
    crate::daemon::daemon_handle_cmd_str(cmd, &get_state())
}

fn jsonl_log(entry: &serde_json::Value) {
    // Bug 53: reuse persistent file handle (was re-opening on every call).
    let mut guard = JSONL_FILE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        if let Ok(fh) = std::fs::OpenOptions::new()
            .create(true).append(true).open("/tmp/reliary_proxy.jsonl")
        {
            *guard = Some(fh);
        } else {
            return;
        }
    }
    if let Some(fh) = guard.as_mut() {
        use std::io::Write;
        let _ = writeln!(fh, "{}", serde_json::to_string(entry).unwrap_or_default());  // GUARDED: intentional
    }
}

// ── History Compression Components ──

// Per-auth-key state — first-appearance freeze cache.
// `content_cache`: maps content hash → compressed version.
struct PerKeyState {
    content_cache: FxHashMap<u64, String>,
}

impl PerKeyState {
    fn new() -> Self {
        Self { content_cache: FxHashMap::default() }
    }

    /// Content hash for cache lookup.
    fn content_hash(content: &str) -> u64 {
        use rustc_hash::FxHasher;
        let mut h = FxHasher::default();
        content.hash(&mut h);
        h.finish()
    }
}

// Global per-auth-key state store
static PER_KEY_STATE: LazyLock<Mutex<FxHashMap<String, PerKeyState>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));

fn get_or_create_state(auth_key: &str) -> std::sync::MutexGuard<'static, FxHashMap<String, PerKeyState>> {
    let mut guard = PER_KEY_STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard.entry(auth_key.to_string()).or_insert_with(PerKeyState::new);
    guard
}

// Sanitize malformed messages that violate OpenAI/DeepSeek spec.
//
// Pi's retry-after-error mechanism produces several invalid sequences:
//   1. Empty assistant messages with no content/tool_calls (Pi's retry marker)
//   2. Assistant messages whose tool_call_ids were ALREADY responded to earlier
//      (Pi re-uses the same IDs on retry, causing DeepSeek to reject)
//
// Pattern observed in failing BFCL runs:
//   assistant{tc=[id1,id2,id3]} → tool{id1} tool{id2} tool{id3}
//   → assistant{tc=[id1,id2,id3]} → tool{id1} tool{id2} tool{id3}
//                       ↑ DeepSeek rejects: tool_calls reuse already-responded IDs
//
// Fix:
//   1. Find consecutive assistant-with-tool_calls messages where the second reuses
//      IDs from the first.
//   2. Strip the duplicate tool_calls from the second assistant AND its following
//      tool responses (otherwise those become orphans).
//   3. Remove empty assistant messages that aren't the final message.
//
// No-op for well-formed sequences. Default-on; opt-out via RELIARY_PROXY_SANITIZER=0.
fn sanitize_malformed_messages(payload: &mut Value) {
    let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };

    // Pass 1: detect and fix duplicate tool_call_id reuse.
    // Find an assistant with tool_calls. Look at the NEXT assistant with tool_calls.
    // If they share IDs, strip the second assistant's tool_calls AND its following tool messages.
    let n = messages.len();
    let mut to_remove: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < n {
        if to_remove.contains(&i) {
            i += 1;
            continue;
        }
        let msg = &messages[i];
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            i += 1;
            continue;
        }
        let first_ids: std::collections::HashSet<String> = msg.get("tool_calls")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|v| v.get("id").and_then(|x| x.as_str()).map(String::from)).collect())
            .unwrap_or_default();
        if first_ids.is_empty() {
            i += 1;
            continue;
        }
        // Look for the next assistant with tool_calls that reuses any of these IDs.
        let mut j = i + 1;
        while j < n {
            let next = &messages[j];
            if next.get("role").and_then(|r| r.as_str()) != Some("assistant") {
                j += 1;
                continue;
            }
            let next_ids: Vec<String> = next.get("tool_calls")
                .and_then(|t| t.as_array())
                .map(|a| a.iter().filter_map(|v| v.get("id").and_then(|x| x.as_str()).map(String::from)).collect())
                .unwrap_or_default();
            if next_ids.is_empty() {
                j += 1;
                continue;
            }
            // Check overlap
            let overlap: Vec<&String> = next_ids.iter().filter(|id| first_ids.contains(*id)).collect();
            if overlap.is_empty() {
                j += 1;
                continue;
            }
            // Found it. Strip tool_calls from assistant j, and remove any tool messages
            // that respond to the overlapping IDs (which would become orphans).
            if let Some(tc_arr) = messages[j].get_mut("tool_calls").and_then(|t| t.as_array_mut()) {
                tc_arr.retain(|v| {
                    v.get("id").and_then(|i| i.as_str())
                        .map(|id| !first_ids.contains(id))
                        .unwrap_or(true)
                });
            }
            // Now look forward from j and remove tool messages that respond to overlapping IDs.
            let mut k = j + 1;
            while k < n {
                let m = &messages[k];
                if m.get("role").and_then(|r| r.as_str()) == Some("tool") {
                    if let Some(tcid) = m.get("tool_call_id").and_then(|x| x.as_str()) {
                        if overlap.iter().any(|id| id.as_str() == tcid) {
                            to_remove.push(k);
                            k += 1;
                            continue;
                        }
                    }
                    k += 1;
                } else {
                    break; // stop at next non-tool
                }
            }
            break;
        }
        i += 1;
    }
    for i in to_remove.iter().rev() {
        messages.remove(*i);
    }

    // Pass 2: remove empty assistant messages (no content, no tool_calls) that aren't
    // the final message in the conversation.
    let mut to_remove: Vec<usize> = Vec::new();
    let n = messages.len();
    for (i, msg) in messages.iter().enumerate() {
        if i + 1 == n {
            continue;
        }
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let content_empty = match msg.get("content") {
            Some(Value::String(s)) => s.is_empty(),
            Some(Value::Array(a)) => a.is_empty(),
            Some(Value::Null) | None => true,
            _ => false,
        };
        let has_tool_calls = msg.get("tool_calls")
            .and_then(|t| t.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if !content_empty || has_tool_calls {
            continue;
        }
        to_remove.push(i);
    }
    for i in to_remove.iter().rev() {
        messages.remove(*i);
    }
}

// Compress old assistant reasoning — strip verbose explanations, keep code blocks intact.
// Splits message into code blocks (```...```) and prose sections.
// Compresses prose, leaves code verbatim.
fn compress_assistant_text(text: &str, dict: Option<&reliary_compress::CompressionDict>) -> Option<String> {
    // First try full-text compress (works for prose-only with no code blocks)
    if let Some(compressed) = reliary_compress::compress_reasoning(text, dict) {
        return Some(compressed);
    }

    // Split on code blocks
    let mut parts: Vec<String> = Vec::new();
    let mut in_code = false;
    let mut code_buf = String::new();
    let mut prose_buf = String::new();

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_code {
                parts.push(code_buf.clone());
                code_buf.clear();
                in_code = false;
            } else {
                if !prose_buf.is_empty() {
                    parts.push(prose_buf.clone());
                    prose_buf.clear();
                }
                in_code = true;
                code_buf.push_str(line);
                code_buf.push('\n');
            }
        } else if in_code {
            code_buf.push_str(line);
            code_buf.push('\n');
        } else {
            prose_buf.push_str(line);
            prose_buf.push('\n');
        }
    }
    if in_code && !code_buf.is_empty() {
        parts.push(code_buf);
    } else if !prose_buf.is_empty() {
        parts.push(prose_buf);
    }

    // Compress each section: keep code verbatim, compress prose
    let mut result = String::new();
    let mut total_original = 0usize;
    let mut total_compressed = 0usize;

    for part in &parts {
        total_original += part.len();
        if part.contains("```") || part.len() < 50 {
            result.push_str(part);
            total_compressed += part.len();
        } else {
            let compressed = reliary_compress::compress_reasoning(part, dict);
            match compressed {
                Some(c) if c.len() < part.len() => {
                    result.push_str(&c);
                    result.push('\n');
                    total_compressed += c.len();
                }
                _ => {
                    result.push_str(part);
                    total_compressed += part.len();
                }
            }
        }
    }
    if total_original > 0 && total_compressed < total_original {
        Some(result)
    } else {
        None
    }
}

// Full sift pipeline: adaptive content-type-aware compression.
// 1. Classify lines with skeleton normalization (UUID→{uuid}, hex→{hash}, etc.)
// 2. Detect output type (JSON/Diff/Tabular/Prefixed/Normal)
// 3. Apply expert compression per type
// 4. MaxwellGate entropy guard on result
fn sift_compress_tool_result(content: &str) -> String {
    if content.len() < 200 { return content.to_string(); }

    // Step 1: Very large content — info-preserving zone truncate if FTS5 available,
    // else fall back to blind zone truncate. Info-zone preserves error lines and
    // project-specific identifiers, enabling more aggressive truncation without
    // signal loss (Phase 3 from break-ceiling plan).
    let working = if content.lines().count() > 200 {
        let info_scored = build_info_scorer();
        if let Some(scorer) = info_scored {
            // With scorer: keep top-15 by info score (vs blind 30+15=45 lines)
            reliary_sift::zone_truncate_info(content, 15, Some(scorer))
        } else {
            // No scorer available: blind zone truncate (legacy behavior)
            reliary_sift::zone_truncate(content, 30, 15)
        }
    } else {
        content.to_string()
    };

    // Step 2: Classify lines (skeleton normalization, error/progress/summary detection)
    let lines = reliary_sift::classify::classify(&working);
    if lines.is_empty() { return working; }

    // Step 3: Detect compression strategy
    let raw_lines: Vec<(String, reliary_sift::classify::Line)> = lines.iter()
        .map(|l| (l.text.clone(), l.clone()))
        .collect();
    let strategy = reliary_sift::classify::detect_strategy(&raw_lines);

    // Step 4: Apply expert compression per strategy (now uses aggressive_skeleton
    // when content is template-filled — Phase 1 from break-ceiling plan)
    let compressed = reliary_sift::filter::format_output(&lines, strategy);

    // Step 5: If adaptive didn't help, fall through to existing mechanisms
    if compressed.len() >= working.len() || compressed.is_empty() {
        // Step 5a: Command output collapse (cargo/test)
        let collapsed = reliary_output::compress_output(&working);
        if collapsed.len() < working.len() {
            return collapsed;
        }
        // Step 5b: File content classify + compress
        let clines = reliary_sift::classify_content(&working);
        if reliary_sift::looks_like_content(&clines) {
            let cc = reliary_sift::compress_content(clines, true);
            let result = cc.join("\n");
            if result.len() < working.len() {
                return result;
            }
        }
    } else {
        // SRCR safety floor check on aggressive skeleton output.
        // If the compressed version destroyed too much signal (below floor),
        // return the working (pre-skeleton) content instead.
        if let Some(result) = sr_floor_check(&compressed, &working) {
            return result;
        }
        return compressed;
    }

    // Step 6: MaxwellGate — if information-dense, don't force compression
    let gate = reliary_sift::MaxwellGate::default();
    if gate.score(&working).is_none() {
        return working;
    }

    working
}

/// SRCR safety floor. Returns Some(working) if compressed output's SRCR
/// is below the configured floor — i.e., the compression destroyed too much
/// signal and we should ship the pre-skeleton content instead.
/// Returns None if floor is disabled (0.0) or compression passes the floor check.
fn sr_floor_check(compressed: &str, working: &str) -> Option<String> {
    let floor: f64 = std::env::var("RELIARY_PROXY_SRCR_FLOOR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.3);
    if floor <= 0.0 {
        return None;
    }
    let (srcr, _, _) = reliary_compress::srcr_for_compression(working, compressed);
    if srcr < floor {
        // Floor blocked — return pre-skeleton working content
        Some(working.to_string())
    } else {
        None
    }
}

/// Build a scorer closure that uses FTS5 document frequency for info scoring.
/// Returns None if FTS5 DF weighting is disabled (RELIARY_PROXY_FT_WEIGHT=0)
/// or if FTS5 index is unavailable (no project index, empty DB, etc).
/// Falls back to blind zone truncation in that case.
fn build_info_scorer() -> Option<impl Fn(&str) -> f64> {
    // Gate Phase 2 (FTS5 DF weighting) behind opt-in env var until validated
    // in live sessions. Default off.
    let ft_enabled = std::env::var("RELIARY_PROXY_FT_WEIGHT")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    if !ft_enabled {
        return None;
    }
    use std::sync::Mutex;
    // Try to open the project FTS5 index. Look for it in CWD or PWD env.
    let candidates = [
        std::env::var("RELIARY_INDEX_DB").ok(),
        Some(".reliary/index.sqlite".to_string()),
        Some("/tmp/reliary/index.sqlite".to_string()),
    ];
    let mut fw = None;
    for cand in candidates.iter().flatten() {
        if let Some(w) = reliary_search::ft_weight::FtWeight::open(cand) {
            fw = Some(w);
            break;
        }
    }
    let fw = fw?;
    let fw_mutex = Mutex::new(fw);
    Some(move |line: &str| -> f64 {
        let mut guard = fw_mutex.lock().unwrap_or_else(|e| e.into_inner());
        guard.line_info_score(line)
    })
}

// Compress the assistant message in an API response body before returning to agent.
// Parses JSON, finds choices[0].message.content, compresses, re-serializes.
// For SSE: scans final data chunk for content field, compresses in-place.
fn compress_response_body(body: &str, is_sse: bool) -> String {
    if body.len() < 500 { return body.to_string(); }

    if is_sse {
        // SSE: find the last data: line with content, compress it
        let mut lines: Vec<String> = body.lines().map(|s| s.to_string()).collect();
        for i in (0..lines.len()).rev() {
            let line = &lines[i];
            if !line.starts_with("data: ") { continue; }
            let json_str = &line[6..];
            if json_str == "[DONE]" { continue; }
            if let Ok(mut v) = serde_json::from_str::<Value>(json_str) {
                if let Some(choices) = v.get_mut("choices").and_then(|c| c.as_array_mut()) {
                    if let Some(choice) = choices.first_mut() {
                        // Try delta first, then message (streaming vs non-streaming format)
                        let compressed_content: Option<String> = {
                            let content = choice.get("delta")
                                .and_then(|d| d.get("content"))
                                .or_else(|| choice.get("message").and_then(|m| m.get("content")))
                                .and_then(|c| c.as_str());
                            if let Some(c) = content {
                                if c.len() > 300 {
                                    compress_assistant_text(c, COMPRESSION_DICT.as_ref())
                                } else { None }
                            } else { None }
                        };
                        if let Some(compressed) = compressed_content {
                            if let Some(delta) = choice.get_mut("delta") {
                                delta["content"] = Value::String(compressed);
                            } else if let Some(msg) = choice.get_mut("message") {
                                msg["content"] = Value::String(compressed);
                            }
                        }
                    }
                }
                lines[i] = format!("data: {}", serde_json::to_string(&v).unwrap_or_else(|_| json_str.to_string()));
                break;
            }
        }
        return lines.join("\n");
    }

    // Non-streaming JSON response
    if let Ok(mut v) = serde_json::from_str::<Value>(body) {
        if let Some(choices) = v.get_mut("choices").and_then(|c| c.as_array_mut()) {
            if let Some(choice) = choices.first_mut() {
                if let Some(msg) = choice.get_mut("message") {
                    if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                        if content.len() > 300 {
                            if let Some(compressed) = compress_assistant_text(content, COMPRESSION_DICT.as_ref()) {
                                msg["content"] = Value::String(compressed);
                                return serde_json::to_string(&v).unwrap_or_else(|_| body.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    body.to_string()
}

// First-appearance freeze compression: compress every message on first occurrence,
// cache the compressed version, and use the cached version forever after.
// This preserves KV cache stability — the compressed version is what the API/SDK
// has cached from the start.
fn compress_messages(messages: &mut [Value], state: &mut PerKeyState) -> (usize, usize) {
    let mut history_saved: usize = 0;

    // Turn 1 has only system + user message(s) — nothing to compress
    if messages.len() <= 2 { return (0, 0); }

    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

        // Never compress user or system messages
        if role == "user" || role == "system" { continue; }

        let content = match msg.get("content") {
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };
        if content.len() < 100 { continue; }

        let hash = PerKeyState::content_hash(&content);

        // If we already have a cached version, use it
        if let Some(cached) = state.content_cache.get(&hash) {
            history_saved += content.len().saturating_sub(cached.len());
            msg["content"] = Value::String(cached.clone());
            continue;
        }

        // First occurrence: compress and cache
        let compressed = match role {
            "assistant" => {
                let existing = compress_assistant_text(&content, COMPRESSION_DICT.as_ref());
                // Novel mechanisms (Maxwell, DSL) — disabled via RELIARY_PROXY_NOVEL_COMPRESS=0
                let novel_disabled = std::env::var("RELIARY_PROXY_NOVEL_COMPRESS").is_ok_and(|v| v == "0");
                if !novel_disabled {
                    let maxwell = crate::novel_compress::maxwell_compress(&content, 50.0);
                    let state_dsl = crate::novel_compress::extract_dialogue_state(&content);
                    let mut best: Option<(String, usize)> = existing.clone().map(|c| { let s = content.len().saturating_sub(c.len()); (c, s) });
                    if let Some(c) = &maxwell {
                        let s = content.len().saturating_sub(c.len());
                        if best.as_ref().is_none_or(|b| s > b.1) { best = Some((c.clone(), s)); }
                    }
                    if let Some(c) = &state_dsl {
                        let s = content.len().saturating_sub(c.len());
                        if best.as_ref().is_none_or(|b| s > b.1) { best = Some((c.clone(), s)); }
                    }
                    best.map(|(c, _)| c)
                } else {
                    existing
                }
            }
            "tool" | "toolResult" => {
                let sifted = sift_compress_tool_result(&content);
                let sifted_opt = if sifted.len() < content.len() { Some(sifted) } else { None };
                // Novel mechanism (invariant hoisting) — disabled via RELIARY_PROXY_NOVEL_COMPRESS=0
                let novel_disabled = std::env::var("RELIARY_PROXY_NOVEL_COMPRESS").is_ok_and(|v| v == "0");
                if !novel_disabled {
                    let hoisted = crate::novel_compress::hoist_json_invariants(&content);
                    let mut best: Option<(String, usize)> = sifted_opt.clone().map(|c| { let s = content.len().saturating_sub(c.len()); (c, s) });
                    if let Some(c) = &hoisted {
                        let s = content.len().saturating_sub(c.len());
                        if best.as_ref().is_none_or(|b| s > b.1) { best = Some((c.clone(), s)); }
                    }
                    best.map(|(c, _)| c)
                } else {
                    sifted_opt
                }
            }
            _ => None,
        };

        if let Some(c) = compressed {
            if c.len() < content.len() && state.content_cache.len() < 200 {
                history_saved += content.len().saturating_sub(c.len());
                state.content_cache.insert(hash, c.clone());
                msg["content"] = Value::String(c);
            }
            // Don't cache uncompressed content — it grows without bound
        }
    }

    (history_saved, 0)
}

// ── Health / Ping ──

async fn health() -> impl IntoResponse {
    (StatusCode::OK, [("content-type", "application/json")], "{\"status\":\"ok\"}")
}

async fn ping() -> &'static str { "pong" }

// ── Daemon GET routes ──

async fn search_handler(Query(params): Query<StdHashMap<String, String>>) -> String {
    let q = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let p = params.get("path").map(|s| s.as_str()).unwrap_or(".");
    daemon_cmd_str(&format!("search {} {}", q, p))
}

async fn risk_handler(Query(params): Query<StdHashMap<String, String>>) -> String {
    let f = params.get("file").map(|s| s.as_str()).unwrap_or("");
    daemon_cmd_str(&format!("risk {}", f))
}

async fn compress_handler(Query(params): Query<StdHashMap<String, String>>) -> String {
    let t = params.get("text").map(|s| s.as_str()).unwrap_or("");
    daemon_cmd_str(&format!("compress {}", t))
}

async fn veto_handler(Query(params): Query<StdHashMap<String, String>>) -> String {
    let f = params.get("file").map(|s| s.as_str()).unwrap_or("");
    let t = params.get("text").map(|s| s.as_str()).unwrap_or("");
    daemon_cmd_str(&format!("veto {} {}", f, t))
}

async fn muzzle_handler(Query(params): Query<StdHashMap<String, String>>) -> String {
    let st = params.get("state").map(|s| s.as_str()).unwrap_or("");
    let s = get_state();
    match st {
        "on" => { s.set_muzzle(true); "muzzled\n".to_string() }
        "off" => { s.set_muzzle(false); "unmuzzled\n".to_string() }
        _ => "ERROR: state must be on|off\n".to_string()
    }
}

async fn prior_handler(Query(params): Query<StdHashMap<String, String>>) -> String {
    let p = params.get("path").map(|s| s.as_str()).unwrap_or(".");
    daemon_cmd_str(&format!("prior {}", p))
}

async fn read_summary_handler(Query(params): Query<StdHashMap<String, String>>) -> String {
    let f = params.get("file").map(|s| s.as_str()).unwrap_or("");
    daemon_cmd_str(&format!("read-summary {}", f))
}

async fn status_handler() -> &'static str { "ok\n" }

async fn who_calls_handler(Query(params): Query<StdHashMap<String, String>>) -> String {
    let file = params.get("file").map(|s| s.as_str()).unwrap_or("");
    let identifier = params.get("identifier").map(|s| s.as_str()).unwrap_or("");
    if file.is_empty() || identifier.is_empty() {
        return "[]".to_string();
    }
    // Resolve project root from file path
    let root = if let Some((r, _, _)) = crate::daemon::find_reliary_root(file) {
        r
    } else {
        return "[]".to_string();
    };
    let index_path = format!("{}/.reliary/index.sqlite", root);
    let db = match rusqlite::Connection::open(&index_path) {
        Ok(d) => d,
        Err(_) => return "[]".to_string(),
    };
    // Normalize paths: index stores relative paths
    let rel_file = file.trim_start_matches('/').trim_start_matches(root.trim_end_matches('/')).trim_start_matches('/');
    let callers = reliary_search::search::who_calls(&db, identifier, rel_file);
    if !callers.is_empty() {
        tracing::info!("who_calls: {} referenced by {} files for {}", identifier, callers.len(), rel_file);
    }
    serde_json::to_string(&callers).unwrap_or_else(|_| "[]".to_string())
}

// ── Proxy POST handler ──

async fn proxy_post(
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let auth_key = extract_auth_key(&headers);

    // Bug 49: per-auth-key rate limit (token bucket, 60 req/min default)
    if let Some(retry_after) = check_rate_limit(&auth_key) {
        return (StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", retry_after.to_string())],
            Json(serde_json::json!({"error": "rate limit exceeded"}))).into_response();
    }

    let upstream_url = match resolve_upstream(&auth_key) {
        Some(url) => url,
        None => return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error":"unknown api key"}))).into_response(),
    };

    let mut payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": format!("json parse: {}", e)}))).into_response(),
    };

    let is_streaming = payload.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // Sanitize malformed messages before any routing decision.
    // Default-on because it's a no-op for well-formed sequences and fixes
    // a class of Pi/Claude Code retry bugs that produce empty assistant
    // messages violating OpenAI spec ("assistant with tool_calls must be
    // followed by tool messages"). Opt-out via RELIARY_PROXY_SANITIZER=0.
    let sanitizer_enabled = !std::env::var("RELIARY_PROXY_SANITIZER").is_ok_and(|v| v == "0");
    if sanitizer_enabled {
        sanitize_malformed_messages(&mut payload);
    }

    // Passthrough mode: relay raw request/response with zero compression,
    // zero guard, zero history modification. Sanitizer still runs (default-on).
    // Useful as an honest baseline when benchmarking accuracy preservation —
    // the proxy adds HTTP hop overhead but nothing else.
    let passthrough = std::env::var("RELIARY_PROXY_PASSTHROUGH").is_ok_and(|v| v == "1");
    if passthrough {
        let body_bytes = serde_json::to_vec(&payload).unwrap_or_default();
        let mut req_builder = HTTP_CLIENT.post(&upstream_url)
            .header("Content-Type", "application/json")
            .body(body_bytes);
        if let Some(auth_val) = headers.get("authorization") {
            req_builder = req_builder.header("authorization", auth_val);
        }
        let upstream = match req_builder.send().await {
            Ok(r) => r,
            Err(e) => return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": format!("upstream: {}", e)}))).into_response(),
        };
        let status = upstream.status();
        let content_type = upstream.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_string();
        let bytes = upstream.bytes().await.unwrap_or_default();
        return (status, [("content-type", content_type)], bytes).into_response();
    }

    // Normalize roles: translate provider-specific roles to API-compatible.
    // Harmless for Anthropic /v1/messages (their roles are user/assistant, not developer).
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages.iter_mut() {
            if let Some(role) = msg.get_mut("role") {
                if let Some("developer" | "latest_reminder") = role.as_str() {
                    *role = Value::String("system".to_string());
                }
            }
        }
    }

// (empty-assistant sanitizer was moved above the passthrough check so it applies
// to both passthrough and full proxy code paths)

    // Context filter: collapse old tool results to 1-line summaries (preserves message sequence for KV cache)
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let mut turn_count = 0;
        for msg in messages.iter_mut() {
            match msg.get("role").and_then(|r| r.as_str()).unwrap_or("") {
                "user" => { turn_count += 1; }
                "tool" | "toolResult" if turn_count > 8 => {
                    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    let len = content.len();
                    if len > 100 {
                        msg["content"] = Value::String(format!("[tool result: {} chars — collapsed]", len));
                    }
                }
                _ => {}
            }
        }
    }

    // Guard: check edit tool calls for orphaned references (ON by default, disable via RELIARY_PROXY_GUARD_DISABLE=1)
    if !std::env::var("RELIARY_PROXY_GUARD_DISABLE").is_ok_and(|v| v == "1") {
        if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
            if let Some(last) = messages.last() {
                if last.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                    let content = last.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    // Bug 59: only check tool_calls array (prose mentions of "edit"/"write"
                    // are too noisy — every assistant message that discusses editing triggers
                    // a false positive). Use exact tool name match on the tool_calls function.
                    let has_edit_tool = last.get("tool_calls")
                        .and_then(|tc| tc.as_array())
                        .map(|calls| calls.iter().any(|tc| {
                            tc.get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                                .map(|n| n == "edit" || n == "write" || n == "sed")
                                .unwrap_or(false)
                        }))
                        .unwrap_or(false);
                    if has_edit_tool {
                        if let Some((file_path, new_text)) = extract_edit_from_assistant(content) {
                            if let Some((root, index_path, _)) = crate::daemon::find_reliary_root(&file_path) {
                                let rel_paths = resolve_index_paths(&file_path, &root);
                                for rp in &rel_paths {
                                    // Guard result cache: skip FTS5 query for repeated same-content edits
                                    let mut cache_key_hasher = rustc_hash::FxHasher::default();
                                    rp.hash(&mut cache_key_hasher);
                                    new_text.hash(&mut cache_key_hasher);
                                    let gk = cache_key_hasher.finish();
                                    let cached_status = GUARD_CACHE.lock().ok().and_then(|mut c| {
                                        if let Some(entry) = c.get(&gk) {
                                            if entry.inserted_at.elapsed() < std::time::Duration::from_secs(60) {
                                                return Some(entry.status.clone());
                                            }
                                        }
                                        c.remove(&gk);
                                        None
                                    });
                                    let guard_status = match cached_status {
                                        Some(s) => s,
                                        None => {
                                            let result = crate::guard::check_diff(&index_path, rp, &new_text);
                                            let status = result.get("status").and_then(|s| s.as_str()).unwrap_or("error").to_string();
                                            if let Ok(mut c) = GUARD_CACHE.lock() {
                                                // Evict stale entries (elapsed > 60s) to bound memory.
                                                c.retain(|_, e| e.inserted_at.elapsed() < std::time::Duration::from_secs(60));
                                                // Hard cap to prevent unbounded growth.
                                                if c.len() >= 500 {
                                                    if let Some(&oldest_key) = c.iter()
                                                        .min_by_key(|(_, e)| e.inserted_at)
                                                        .map(|(k, _)| k)
                                                    {
                                                        c.remove(&oldest_key);
                                                    }
                                                }
                                                c.insert(gk, GuardCacheEntry { status: status.clone(), inserted_at: std::time::Instant::now() });
                                            }
                                            status
                                        }
                                    };
                                    if guard_status != "clean" {
                                        messages.push(serde_json::json!({
                                            "role": "user",
                                            "content": format!("[guard: potential cross-file reference issues in {} - verify]", rp)
                                        }));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Anti-decision: record outcomes from tool results and annotate (off by default) ──
    if std::env::var("RELIARY_PROXY_FEATURE_ANTI").is_ok_and(|v| v == "1") {
        // Bug 68: use the request's workdir inferred from any file path in messages,
        // not the daemon's startup workdir (which may serve multiple projects).
        let workdir = infer_request_workdir(&payload)
            .unwrap_or_else(|| get_state().workdir.to_string_lossy().to_string());
        if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
            for msg in messages.iter() {
                if let Some((file, identifier, operation, success)) =
                    crate::antidecision::extract_tool_call(msg)
                {
                    crate::antidecision::record(&workdir, &file, &identifier, &operation, success);
                }
            }
            for msg in messages.iter_mut() {
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
                let content = match msg.get("content").and_then(|c| c.as_str()).map(|s| s.to_string()) { Some(s) => s, None => continue };
                if role != "user" && role != "tool" && role != "toolResult" { continue; }
                let known_anti: Vec<(String, String, String)> = {
                    let mut list = Vec::new();
                    if let Ok(db) = crate::antidecision::ANTI_DB.lock() {
                        if let Some((counters, _)) = db.get(&workdir) {
                            for key in counters.keys() {
                                if let Some(rest) = key.strip_prefix(&format!("{}::", workdir)) {
                                    if let Some((file, rest2)) = rest.split_once("::") {
                                        let identifier = rest2.to_string();
                                        if content.contains(file) && content.contains(&identifier) {
                                            let ann = format!(" -{}", identifier);
                                            list.push((file.to_string(), ann, identifier));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    list
                };
                for (file, ann, _identifier) in &known_anti {
                    let new_ref = format!("{} /*{}/**/", file, ann);
                    let annotated = content.replacen(file.as_str(), &new_ref, 1);
                    if annotated != content {
                        msg["content"] = Value::String(annotated);
                        break;
                    }
                }
            }
        }
    }

    // Compact tool call arguments: minify JSON content in tool messages
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages.iter_mut() {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role != "tool" && role != "toolResult" { continue; }
            if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                if content.len() > 500 && (content.trim_start().starts_with('{') || content.trim_start().starts_with('[')) {
                    // Try to parse and re-serialize compactly
                    if let Ok(v) = serde_json::from_str::<Value>(content) {
                        let compact = serde_json::to_string(&v).unwrap_or_default();
                        if compact.len() < content.len() {
                            msg["content"] = Value::String(compact);
                        }
                    }
                }
            }
        }
    }

    // Dedup identical messages (catches agent duplication bugs)
    if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let mut seen: rustc_hash::FxHashSet<u64> = rustc_hash::FxHashSet::default();
        let mut to_remove: Vec<usize> = Vec::new();
        for (i, msg) in messages.iter().enumerate() {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
            if role.is_empty() || content.is_empty() { continue; }
            let mut h = rustc_hash::FxHasher::default();
            role.hash(&mut h);
            content.hash(&mut h);
            let key = h.finish();
            if seen.contains(&key) {
                to_remove.push(i);
            } else {
                seen.insert(key);
            }
        }
        for i in to_remove.iter().rev() {
            messages.remove(*i);
        }
    }

    // System prompt stripping: on turn 2+, replace system prompt with cached marker.
    // Providers KV-cache the system prompt after turn 1. Stripping saves ~1000+ tokens/turn.
    // Default ON. Disable via RELIARY_PROXY_STRIP_SYSTEM_PROMPT=0
    if !std::env::var("RELIARY_PROXY_STRIP_SYSTEM_PROMPT").is_ok_and(|v| v == "0") {
        if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
            let turn_count = messages.iter().filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user")).count();
            if turn_count >= 2 {
                if let Some(first) = messages.first_mut() {
                    if first.get("role").and_then(|r| r.as_str()) == Some("system") {
                        first["content"] = Value::String("[system prompt cached]".to_string());
                    }
                }
            }
        }
    }

    // First-appearance freeze: compress every message on first occurrence
    let (history_saved, _aggressiveness) = {
        let mut guard = get_or_create_state(&auth_key);
        if let Some(state) = guard.get_mut(&auth_key) {
            if let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
                compress_messages(messages, state)
            } else {
                (0, 0)
            }
        } else {
            tracing::error!("state missing after insert for auth_key {}", &auth_key[..8.min(auth_key.len())]);
            (0, 0)
        }
    };

    // Response cache (streaming and non-streaming)
    if let Some(messages) = payload.get("messages") {
        if let Ok(msg_str) = serde_json::to_string(messages) {
            // Bug 58: include model in cache key
            let model = payload.get("model").and_then(|m| m.as_str()).unwrap_or("");
            if let Some(cached) = cached_response(&auth_key, &msg_str, is_streaming, model) {
                let content_type = if is_streaming { "text/event-stream" } else { "application/json" };
                return (StatusCode::OK, [("content-type", content_type)], cached).into_response();
            }
        }
    }

    let body_bytes = serde_json::to_vec(&payload).unwrap_or_default();

    let mut req_builder = HTTP_CLIENT.post(&upstream_url)
        .header("Content-Type", "application/json")
        .body(body_bytes.clone());

    if let Some(auth_val) = headers.get("authorization") {
        req_builder = req_builder.header("authorization", auth_val);
    }

    let hdr_history_saved = history_saved.to_string();

    match req_builder.send().await {
        Ok(mut upstream_resp) => {
            // If upstream returned an error status, forward the error body as JSON
            // (not SSE chunks). Pi's SSE parser fails when non-"data: " prefixed
            // bytes arrive in the stream.
            let upstream_status = upstream_resp.status();
            let upstream_ct = upstream_resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            if !upstream_status.is_success() {
                // Bug 71: cap error response body to 10MB (was unbounded — 100MB HTML
                // error pages caused OOM).
                let bytes = upstream_resp.bytes().await.unwrap_or_default();
                let capped = if bytes.len() > reliary_core::MAX_FILE_SIZE as usize {
                    let mut c = bytes[..reliary_core::MAX_FILE_SIZE as usize].to_vec();
                    c.extend_from_slice(b"\n[... truncated, body exceeded 10MB cap ...]");
                    c
                } else {
                    bytes.to_vec()
                };
                return (upstream_status, [("content-type", upstream_ct)], capped).into_response();
            }
            // Also detect non-SSE content-types (e.g., upstream returned JSON error
            // even with success status). Forward as-is.
            if is_streaming && !upstream_ct.contains("event-stream") {
                let bytes = upstream_resp.bytes().await.unwrap_or_default();
                let capped = if bytes.len() > reliary_core::MAX_FILE_SIZE as usize {
                    bytes[..reliary_core::MAX_FILE_SIZE as usize].to_vec()
                } else {
                    bytes.to_vec()
                };
                return (upstream_status, [("content-type", upstream_ct)], capped).into_response();
            }
            if is_streaming {
                // True SSE streaming: forward chunks as they arrive.
                // Uses reqwest::Response::chunk() loop → tokio::mpsc → axum Body::from_stream.
                // This preserves time-to-first-token (~500ms) instead of buffering.
                let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::convert::Infallible>>(32);
                let ak = auth_key.clone();

                tokio::spawn(async move {
                    let mut total_bytes = Vec::new();
                    let mut last_chunk_with_usage = String::new();
                    let mut finish_sent = false;
                    // Accumulate a rolling tail buffer to detect finish_reason that spans
                    // chunk boundaries. Many SSE chunkers split the JSON literal across
                    // chunks so a single-chunk window check is unreliable.
                    let mut rolling_tail: Vec<u8> = Vec::with_capacity(4096);
                    // Bug 56: debounce prefetch — accumulate chunks, flush every 32KB or on stream end
                    let mut pf_buffer = String::new();
                    const PF_FLUSH_BYTES: usize = 32_768;
                    loop {
                        match upstream_resp.chunk().await {
                            Ok(Some(chunk)) => {
                                let chunk_str = String::from_utf8_lossy(&chunk);
                                // Stream-aware prefetch: accumulate chunks, debounce flush.
                                if !std::env::var("RELIARY_PROXY_PREFETCH").is_ok_and(|v| v == "0") {
                                    pf_buffer.push_str(&chunk_str);
                                    if pf_buffer.len() >= PF_FLUSH_BYTES {
                                        let buf = std::mem::take(&mut pf_buffer);
                                        tokio::task::spawn_blocking(move || {
                                            crate::novel_compress::try_prefetch(&buf);
                                        });
                                    }
                                }
                                if chunk_str.contains("\"usage\"") || chunk_str.contains("\"prompt_tokens\"") {
                                    last_chunk_with_usage = chunk_str.to_string();
                                }
                                total_bytes.extend_from_slice(&chunk);
                                // Update rolling tail (last 1024 bytes is enough for finish_reason)
                                rolling_tail.extend_from_slice(&chunk);
                                if rolling_tail.len() > 1024 {
                                    let drop = rolling_tail.len() - 1024;
                                    rolling_tail.drain(..drop);
                                }

                                // Detect finish_reason across chunk boundaries using the
                                // accumulated tail. The previous per-chunk 20-byte window
                                // missed finish_reason split across chunks in samples 1, 2.
                                let tail_str = String::from_utf8_lossy(&rolling_tail);
                                if !finish_sent && (tail_str.contains("\"finish_reason\":\"stop\"")
                                    || tail_str.contains("\"finish_reason\":\"length\""))
                                {
                                    finish_sent = true;
                                }

                                // Forward chunk to client
                                if tx.send(Ok(chunk)).await.is_err() {
                                    break;
                                }
                            }
                            Ok(None) => break, // Stream complete
                            Err(e) => {
                                tracing::warn!("upstream stream chunk error: {}", e);
                                break;
                            }
                        }
                    }
                    // Flush remaining prefetch buffer at stream end (Bug 56).
                    if !pf_buffer.is_empty() {
                        let buf = std::mem::take(&mut pf_buffer);
                        tokio::task::spawn_blocking(move || {
                            crate::novel_compress::try_prefetch(&buf);
                        });
                    }
                    // Stream complete. If the upstream never sent a finish_reason (rare;
                    // happens when the upstream connection drops mid-stream), inject a
                    // synthetic finish chunk so Pi's parser doesn't fail with
                    // "Stream ended without finish_reason". This is the canonical fix
                    // for the v0.6.x stream-ended bug.
                    if !finish_sent {
                        let synthetic = Bytes::from_static(b"data: {\"id\":\"synthetic\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"reliary-synthetic\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
                        let _ = tx.send(Ok(synthetic)).await;
                    }
                    // Channel drop signals stream end to axum

                    // Parse usage from the buffered final chunk (or full body)
                    let usage_text = if !last_chunk_with_usage.is_empty() {
                        last_chunk_with_usage.as_str()
                    } else {
                        &String::from_utf8_lossy(&total_bytes)
                    };
                    if usage_text.contains("\"usage\"") {
                        let pt = usage_text.split("\"prompt_tokens\":").nth(1)
                            .and_then(|s| s.trim_start().split(|c: char| !c.is_ascii_digit()).next())
                            .and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                        let ct = usage_text.split("\"completion_tokens\":").nth(1)
                            .and_then(|s| s.trim_start().split(|c: char| !c.is_ascii_digit()).next())
                            .and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                        jsonl_log(&serde_json::json!({
                            "event": "stream_usage",
                            "auth_prefix": &ak[..ak.len().min(12)],
                            "prompt_tokens": pt,
                            "completion_tokens": ct,
                        }));
                        // Cache-hit feedback loop
                        if !std::env::var("RELIARY_PROXY_CACHE_FEEDBACK").is_ok_and(|v| v == "0") {
                            let hit_tokens = usage_text.split("\"prompt_cache_hit_tokens\":").nth(1)
                                .and_then(|s| s.trim_start().split(|c: char| !c.is_ascii_digit()).next())
                                .and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                            if pt > 0 {
                                let _paused = crate::novel_compress::feed_cache_metrics(&ak, hit_tokens, pt);
                            }
                        }
                    }

                    // Cache the full body (best-effort — skips if serialization fails)
                    if let Ok(msg_str) = serde_json::to_string(&payload.get("messages").unwrap_or(&Value::Null)) {
                        let model = payload.get("model").and_then(|m| m.as_str()).unwrap_or("");
                        store_response(&auth_key, &msg_str, &String::from_utf8_lossy(&total_bytes), true, model);
                    }
                });

                let body_len_hdr = "streaming".to_string();
                let mut resp = axum::response::Response::new(
                    axum::body::Body::from_stream(
                        tokio_stream::wrappers::ReceiverStream::new(rx)
                    )
                );
                resp.headers_mut().insert("content-type", header::HeaderValue::from_static("text/event-stream"));
                resp.headers_mut().insert("cache-control", header::HeaderValue::from_static("no-cache"));
                if let Ok(hv) = header::HeaderValue::from_str(&hdr_history_saved) {
                    resp.headers_mut().insert("x-reliaty-history-saved", hv);
                }
                if let Ok(hv) = header::HeaderValue::from_str(&body_len_hdr) {
                    let _ = hv; // Header value reserved for future use
                }
                resp
            } else {
                match upstream_resp.bytes().await {
                    Ok(bytes) => {
                        let raw_str = String::from_utf8_lossy(&bytes).to_string();
                        // Compress response body before returning to agent
                        let body_str = compress_response_body(&raw_str, false);
                        let model = payload.get("model").and_then(|m| m.as_str()).unwrap_or("");
                        store_response(&auth_key, &String::from_utf8_lossy(&body_bytes), &body_str, false, model);

                        jsonl_log(&serde_json::json!({
                            "event": "proxy_response",
                            "auth_prefix": &auth_key[..auth_key.len().min(12)],
                            "history_saved": history_saved,
                            "response_compressed": body_str.len() < raw_str.len(),
                            "timestamp": std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs()).unwrap_or(0),
                        }));

                        let mut resp = (StatusCode::OK, [("content-type", "application/json")], body_str.clone()).into_response();
                        if let Ok(hv) = header::HeaderValue::from_str(&hdr_history_saved) {
                            resp.headers_mut().insert("x-reliaty-history-saved", hv);
                        }
                        resp
                    }
                    Err(_) => (StatusCode::BAD_GATEWAY, "empty upstream response").into_response(),
                }
            }
        }
        Err(e) => {
            (StatusCode::BAD_GATEWAY, format!("upstream error: {}", e)).into_response()
        }
    }
}

// Extract file path and new content from an edit tool call embedded in assistant JSON.
fn extract_edit_from_assistant(text: &str) -> Option<(String, String)> {
    // Try to find edit/apply-edit tool call patterns in the assistant's response.
    // Pattern 1: "edit" -> "filePath": "..." "newText": "..."
    if let Some(file_start) = text.find("\"filePath\"") {
        let after_file = &text[file_start + 10..];
        let file_path = after_file.split('"').nth(1).map(|s| s.to_string())?;
        if let Some(text_start) = text.find("\"newText\"") {
            let after_text = &text[text_start + 9..];
            let new_text = after_text.split('"').nth(1).map(|s| s.to_string())?;
            return Some((file_path, new_text));
        }
    }
    // Pattern 2: try term-encoded "write" -> "path": "..."
    if let Some(file_start) = text.find("\"path\":") {
        let after_file = &text[file_start + 6..];
        let file_path = after_file.split('"').nth(1).map(|s| s.to_string())?;
        if let Some(text_start) = text.find("\"content\":") {
            let after_text = &text[text_start + 9..];
            let new_text = after_text.split('"').nth(1).map(|s| s.to_string())?;
            return Some((file_path, new_text));
        }
    }
    None
}

// Try multiple relative path forms to match the index's stored paths.
fn resolve_index_paths(file_path: &str, root: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    if file_path.starts_with(root) {
        let rel = file_path[root.len() + 1..].trim_start_matches('/').to_string();
        candidates.push(rel.clone());
        if let Some(stripped) = rel.strip_prefix("crates/") {
            candidates.push(stripped.to_string());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = cwd.to_string_lossy().to_string();
        if file_path.starts_with(&cwd_str) {
            let rel = file_path[cwd_str.len() + 1..].trim_start_matches('/').to_string();
            candidates.push(rel.clone());
            if let Some(stripped) = rel.strip_prefix("crates/") {
                candidates.push(stripped.to_string());
            }
        }
    }
    candidates.push(file_path.to_string());
    candidates
}

// GET /check-diff — check a proposed edit for structural issues.
async fn check_diff_handler(Query(params): Query<StdHashMap<String, String>>) -> String {
    let file_path = params.get("file").map(|s| s.as_str()).unwrap_or("");
    let new_content = params.get("content").map(|s| s.as_str()).unwrap_or("");
    if file_path.is_empty() || new_content.is_empty() {
        return "{\"error\": \"missing file or content param\"}".to_string();
    }
    if let Some((root, index_path, _)) = crate::daemon::find_reliary_root(file_path) {
        // Try multiple relative path forms to match index
        let rel_paths = resolve_index_paths(file_path, &root);
        // Try each, return first that produces warnings
        for rp in &rel_paths {
            let result = crate::guard::check_diff(&index_path, rp, new_content);
            if result.get("status").and_then(|s| s.as_str()) != Some("clean") {
                return serde_json::to_string(&result).unwrap_or_else(|_| "{\"error\": \"serialization failed\"}".to_string());
            }
        }
        // All returned clean — return the first
        let result = crate::guard::check_diff(&index_path, &rel_paths[0], new_content);
        serde_json::to_string(&result).unwrap_or_else(|_| "{\"error\": \"serialization failed\"}".to_string())
    } else {
        "{\"error\": \"no .reliary index\"}".to_string()
    }
}

// GET /read-validated — warn about externally-referenced identifiers before editing.
async fn read_validated_handler(Query(params): Query<StdHashMap<String, String>>) -> String {
    let file_path = params.get("file").map(|s| s.as_str()).unwrap_or("");
    if file_path.is_empty() {
        return "{\"error\": \"missing file param\"}".to_string();
    }
    if let Some((root, index_path, _)) = crate::daemon::find_reliary_root(file_path) {
        use std::io::Read;
        let rel_paths = resolve_index_paths(file_path, &root);
        let rel_path = rel_paths.first().map(|s| s.as_str()).unwrap_or(file_path);
        let full_path = std::path::Path::new(&root).join(rel_path);
        let mut content = String::new();
        if let Ok(mut f) = std::fs::File::open(&full_path) { // GUARDED: intentional - small file, will spawn_blocking
            if let Ok(meta) = f.metadata() {
                if meta.len() > crate::daemon::MAX_FILE_SIZE {
                    return serde_json::json!({"error": "file too large"}).to_string();
                }
            }
            if f.read_to_string(&mut content).is_err() {  // GUARDED: intentional
                return serde_json::json!({"error": "cannot read file"}).to_string();
            }
        }
        // Try each path form
        for rp in &rel_paths {
            let result = crate::guard::read_validated(&index_path, rp, &content);
            if result.get("status").and_then(|s| s.as_str()) != Some("clean") {
                return serde_json::to_string(&result).unwrap_or_else(|_| "{\"error\": \"serialization failed\"}".to_string());
            }
        }
        let result = crate::guard::read_validated(&index_path, &rel_paths[0], &content);
        serde_json::to_string(&result).unwrap_or_else(|_| "{\"error\": \"serialization failed\"}".to_string())
    } else {
        "{\"error\": \"no .reliary index\"}".to_string()
    }
}

// ── Startup ──

pub async fn start(port: u16, daemon_state: Option<Arc<crate::session_state::SessionState>>) -> Result<(), String> {
    if let Some(s) = daemon_state {
        if let Ok(mut guard) = DAEMON_STATE.lock() {
            *guard = Some(s);
        }
    }

    // Scavenger thread
    let state = get_state();
    std::thread::Builder::new()
        .name("scavenger".into())
        .spawn(move || {
            loop {
                let sc = Arc::clone(&state);
                if let Err(e) = std::panic::catch_unwind(|| {
                    crate::scavenger::scavenger_loop(sc);
                }) {
                    error!("scavenger crashed: {:?}", e);
                }
                std::thread::sleep(std::time::Duration::from_secs(120));
            }
        })
        .ok();  // GUARDED: intentional

    #[cfg(unix)] {
        if let Ok(limit) = rlimit::getrlimit(rlimit::Resource::NOFILE) {
            if limit.0 < 1024 {
                warn!("file descriptor limit is {} (recommended >= 1024)", limit.0);
            }
        }
    }

    let addr = format!("127.0.0.1:{}", port);

    let app = Router::new()
        .route("/health", get(health))
        .route("/ping", get(ping))
        .route("/search", get(search_handler))
        .route("/risk", get(risk_handler))
        .route("/compress", get(compress_handler))
        .route("/veto", get(veto_handler))
        .route("/muzzle", get(muzzle_handler))
        .route("/prior", get(prior_handler))
        .route("/read-summary", get(read_summary_handler))
        .route("/who-calls", get(who_calls_handler))
        .route("/status", get(status_handler))
        .route("/check-diff", get(check_diff_handler))
        .route("/read-validated", get(read_validated_handler))
        .route("/v1/chat/completions", post(proxy_post))
        .route("/v1/messages", post(proxy_post))  // Anthropic/Claude Code compatibility
        .route("/mcp/sse", get(crate::mcp_sse::sse_handler))
        .route("/mcp/messages", post(crate::mcp_sse::messages_handler))
        .layer(tower_http::cors::CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("bind: {}", e))?;

    info!(target: "reliary", "v{} ready — daemon + proxy on :{}", env!("CARGO_PKG_VERSION"), port);
    info!(target: "reliary", "Routes: /health /ping /search /risk /compress /veto /muzzle /prior");
    info!(target: "reliary", "Proxy features: compression=on guard={} anti={} (set RELIARY_PROXY_GUARD_DISABLE=1 to disable guard)",
        if std::env::var("RELIARY_PROXY_GUARD_DISABLE").is_ok_and(|v| v == "1") { "off" } else { "on" },
        if std::env::var("RELIARY_PROXY_FEATURE_ANTI").is_ok_and(|v| v == "1") { "on" } else { "off" });

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("serve: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sanitize_removes_duplicate_tool_call_ids() {
        // Pi retry pattern: assistant with tc, tools respond, then another assistant
        // reuses the SAME tool_call_ids. DeepSeek rejects this. Sanitizer strips
        // the duplicate tc and its orphaned tool responses.
        let mut payload = json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "tool_calls": [
                    {"id": "call_00", "type": "function", "function": {"name": "x", "arguments": "{}"}},
                ]},
                {"role": "tool", "tool_call_id": "call_00", "content": "ok"},
                {"role": "assistant", "tool_calls": [
                    {"id": "call_00", "type": "function", "function": {"name": "x", "arguments": "{}"}},
                ]},
                {"role": "tool", "tool_call_id": "call_00", "content": "ok"},
                {"role": "user", "content": "next"},
            ]
        });
        sanitize_malformed_messages(&mut payload);
        let msgs = payload["messages"].as_array().unwrap();
        // Expectation: the SECOND assistant (with duplicate IDs) should have its
        // tool_calls stripped. Its following tool response should also be removed.
        // Net: messages shrink from 6 to ~3 (user, assistant, tool).
        assert!(msgs.len() < 6, "messages should shrink, got {} msgs: {:?}", msgs.len(), msgs);
        // Verify only the FIRST assistant has tool_calls
        let assistants_with_tc = msgs.iter().filter(|m| {
            m["role"] == "assistant"
                && m.get("tool_calls").and_then(|t| t.as_array()).map(|a| !a.is_empty()).unwrap_or(false)
        }).count();
        assert_eq!(assistants_with_tc, 1, "only first assistant should retain tool_calls, got {}", assistants_with_tc);
    }

    #[test]
    fn sanitize_keeps_well_formed_sequences() {
        // Standard sequence: assistant tc → tools → user. Should be unchanged.
        let mut payload = json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "tool_calls": [
                    {"id": "call_A", "type": "function", "function": {"name": "x", "arguments": "{}"}},
                ]},
                {"role": "tool", "tool_call_id": "call_A", "content": "ok"},
                {"role": "user", "content": "next"},
            ]
        });
        let before = payload["messages"].as_array().unwrap().len();
        sanitize_malformed_messages(&mut payload);
        let after = payload["messages"].as_array().unwrap().len();
        assert_eq!(before, after, "well-formed sequence should be unchanged");
    }

    #[test]
    fn sanitize_removes_empty_assistant_messages() {
        let mut payload = json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": ""},
                {"role": "user", "content": "next"},
            ]
        });
        sanitize_malformed_messages(&mut payload);
        let msgs = payload["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "empty assistant should be removed");
    }

    #[test]
    fn sanitize_preserves_last_empty_assistant() {
        // A final empty assistant (the LLM produced nothing) is preserved.
        let mut payload = json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": ""},
            ]
        });
        sanitize_malformed_messages(&mut payload);
        let msgs = payload["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "final empty assistant preserved");
    }
}
